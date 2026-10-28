use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

const LOOKAHEAD_MS: f64 = 3.0;

const ATTACK_WINDOW_DIVISOR: f64 = 4.0;

const METER_FLOOR_DB: f64 = 40.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimiterSettings {
    pub enabled: bool,
    pub gain_db: f32,
    pub ceiling_db: f32,
    pub release_ms: f32,
    #[serde(default)]
    pub clip: bool,
}

impl Default for LimiterSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            gain_db: 0.0,
            ceiling_db: -0.3,
            release_ms: 120.0,
            clip: false,
        }
    }
}

impl LimiterSettings {
    pub fn clamp(mut self) -> Self {
        self.gain_db = self.gain_db.clamp(0.0, 12.0);
        self.ceiling_db = self.ceiling_db.clamp(-6.0, 0.0);
        self.release_ms = self.release_ms.clamp(10.0, 1000.0);
        self
    }
}

#[inline(always)]
fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

struct LimiterRuntimeSnapshot {
    channels: usize,
    drive_linear: f64,
    ceiling_linear: f64,
    delay_frames: usize,
    attack_coeff: f64,
    release_coeff: f64,
    clip: bool,
}

impl Default for LimiterRuntimeSnapshot {
    fn default() -> Self {
        Self {
            channels: 2,
            drive_linear: 1.0,
            ceiling_linear: 1.0,
            delay_frames: 1,
            attack_coeff: 1.0,
            release_coeff: 1.0,
            clip: false,
        }
    }
}

struct LimiterState {
    ring: Vec<f64>,
    write_pos: usize,
    envelope: f64,
    window: VecDeque<(u64, f64)>,
    frame: u64,
}

impl LimiterState {
    fn new(delay_frames: usize, channels: usize) -> Self {
        let frames = delay_frames.max(1);
        Self {
            ring: vec![0.0; frames * channels.max(1)],
            write_pos: 0,
            envelope: 1.0,
            window: VecDeque::with_capacity(frames + 2),
            frame: 0,
        }
    }

    fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write_pos = 0;
        self.envelope = 1.0;
        self.window.clear();
        self.frame = 0;
    }
}

pub struct LimiterDspContext {
    settings: RwLock<LimiterSettings>,
    enabled: AtomicBool,
    params_dirty: AtomicBool,
    reset_pending: AtomicBool,
    sample_rate_hz: AtomicU32,
    channels: AtomicUsize,
    process_count: AtomicU64,
    reduction_db: AtomicU64,
    runtime: ArcSwap<LimiterRuntimeSnapshot>,
    state: Mutex<LimiterState>,
}

impl Default for LimiterDspContext {
    fn default() -> Self {
        Self::new()
    }
}

impl LimiterDspContext {
    pub fn new() -> Self {
        Self {
            settings: RwLock::new(LimiterSettings::default()),
            enabled: AtomicBool::new(false),
            params_dirty: AtomicBool::new(true),
            reset_pending: AtomicBool::new(true),
            sample_rate_hz: AtomicU32::new(44100),
            channels: AtomicUsize::new(2),
            process_count: AtomicU64::new(0),
            reduction_db: AtomicU64::new(0.0f64.to_bits()),
            runtime: ArcSwap::from_pointee(LimiterRuntimeSnapshot::default()),
            state: Mutex::new(LimiterState::new(1, 2)),
        }
    }

    pub fn clear_meter(&self) {
        self.reduction_db.store(0f64.to_bits(), Ordering::Relaxed);
    }

    pub fn process_count(&self) -> u64 {
        self.process_count.load(Ordering::Relaxed)
    }

    pub fn reduction_db(&self) -> f64 {
        f64::from_bits(self.reduction_db.load(Ordering::Relaxed))
    }

    pub fn get_settings(&self) -> LimiterSettings {
        self.settings.read().clone()
    }

    pub fn set_settings(&self, settings: LimiterSettings) {
        let s = settings.clamp();
        let turning_on = s.enabled && !self.enabled.swap(s.enabled, Ordering::AcqRel);
        let mode_changed = {
            let mut guard = self.settings.write();
            let changed = guard.clip != s.clip;
            *guard = s;
            changed
        };
        if turning_on || mode_changed {
            self.reset_pending.store(true, Ordering::Release);
        }
        self.params_dirty.store(true, Ordering::Release);
    }

    pub fn configure_stream(&self, sample_rate: u32, channels: u32) {
        let chans = channels.max(1) as usize;
        let rate = if sample_rate > 0 { sample_rate } else { 44100 };
        self.sample_rate_hz.store(rate, Ordering::Release);
        self.channels.store(chans, Ordering::Release);

        let delay_frames = Self::delay_frames_for(rate);
        *self.state.lock() = LimiterState::new(delay_frames, chans);
        self.params_dirty.store(true, Ordering::Release);
        self.rebuild_params_if_needed();
    }

    fn delay_frames_for(sample_rate: u32) -> usize {
        ((sample_rate as f64 * LOOKAHEAD_MS / 1000.0).round() as usize).max(1)
    }

    fn rebuild_params_if_needed(&self) {
        if !self.params_dirty.swap(false, Ordering::AcqRel) {
            return;
        }

        let settings = self.settings.read().clone();
        let sample_rate = self.sample_rate_hz.load(Ordering::Acquire).max(8000);
        let channels = self.channels.load(Ordering::Acquire).max(1);
        let delay_frames = Self::delay_frames_for(sample_rate);

        let attack_tau = (delay_frames as f64 / ATTACK_WINDOW_DIVISOR).max(1.0);
        let release_tau = (settings.release_ms as f64 / 1000.0 * sample_rate as f64).max(1.0);

        self.runtime.store(Arc::new(LimiterRuntimeSnapshot {
            channels,
            drive_linear: db_to_linear(settings.gain_db as f64),
            ceiling_linear: db_to_linear(settings.ceiling_db as f64),
            delay_frames,
            attack_coeff: 1.0 - (-1.0 / attack_tau).exp(),
            release_coeff: 1.0 - (-1.0 / release_tau).exp(),
            clip: settings.clip,
        }));
    }

    fn publish_reduction(&self, min_gain: f64) {
        let db = if min_gain >= 1.0 {
            0.0
        } else {
            (-20.0 * min_gain.max(1.0e-6).log10()).min(METER_FLOOR_DB)
        };
        self.reduction_db.store(db.to_bits(), Ordering::Relaxed);
    }

    pub fn process_buffer_f32(&self, samples: &mut [f32]) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }

        self.rebuild_params_if_needed();

        let snap = self.runtime.load_full();
        let channels = snap.channels;
        if channels == 0 || samples.is_empty() {
            return;
        }

        self.process_count.fetch_add(1, Ordering::Relaxed);

        if snap.clip {
            let mut worst = 1.0f64;
            for s in samples.iter_mut() {
                let driven = *s as f64 * snap.drive_linear;
                let mag = driven.abs();
                if mag > snap.ceiling_linear {
                    let kept = snap.ceiling_linear / mag;
                    if kept < worst {
                        worst = kept;
                    }
                }
                *s = driven.clamp(-snap.ceiling_linear, snap.ceiling_linear) as f32;
            }
            self.publish_reduction(worst);
            return;
        }

        let mut state = self.state.lock();
        if state.ring.len() != snap.delay_frames * channels {
            return;
        }
        if self.reset_pending.swap(false, Ordering::AcqRel) {
            state.reset();
        }

        let frames = samples.len() / channels;

        let mut min_gain = 1.0f64;
        for frame in 0..frames {
            let base = frame * channels;
            let mut peak = 0.0f64;
            for ch in 0..channels {
                let v = (samples[base + ch] as f64 * snap.drive_linear).abs();
                if v > peak {
                    peak = v;
                }
            }

            let gain = state.step(&snap, peak);
            if gain < min_gain {
                min_gain = gain;
            }

            let pos = state.write_pos;
            for ch in 0..channels {
                let delayed = state.ring[pos + ch];
                state.ring[pos + ch] = samples[base + ch] as f64;
                let out = (delayed * snap.drive_linear * gain)
                    .clamp(-snap.ceiling_linear, snap.ceiling_linear);
                samples[base + ch] = out as f32;
            }
            state.advance(channels);
        }

        drop(state);
        self.publish_reduction(min_gain);
    }

    pub fn process_buffer_i16(&self, samples: &mut [i16]) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }

        self.rebuild_params_if_needed();

        let snap = self.runtime.load_full();
        let channels = snap.channels;
        if channels == 0 || samples.is_empty() {
            return;
        }

        self.process_count.fetch_add(1, Ordering::Relaxed);

        if snap.clip {
            let mut worst = 1.0f64;
            for s in samples.iter_mut() {
                let driven = *s as f64 / 32768.0 * snap.drive_linear;
                let mag = driven.abs();
                if mag > snap.ceiling_linear {
                    let kept = snap.ceiling_linear / mag;
                    if kept < worst {
                        worst = kept;
                    }
                }
                let out = driven.clamp(-snap.ceiling_linear, snap.ceiling_linear);
                *s = (out.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            }
            self.publish_reduction(worst);
            return;
        }

        let mut state = self.state.lock();
        if state.ring.len() != snap.delay_frames * channels {
            return;
        }
        if self.reset_pending.swap(false, Ordering::AcqRel) {
            state.reset();
        }

        let frames = samples.len() / channels;

        let mut min_gain = 1.0f64;
        for frame in 0..frames {
            let base = frame * channels;
            let mut peak = 0.0f64;
            for ch in 0..channels {
                let v = (samples[base + ch] as f64 / 32768.0 * snap.drive_linear).abs();
                if v > peak {
                    peak = v;
                }
            }

            let gain = state.step(&snap, peak);
            if gain < min_gain {
                min_gain = gain;
            }

            let pos = state.write_pos;
            for ch in 0..channels {
                let delayed = state.ring[pos + ch];
                state.ring[pos + ch] = samples[base + ch] as f64 / 32768.0;
                let out = (delayed * snap.drive_linear * gain)
                    .clamp(-snap.ceiling_linear, snap.ceiling_linear);
                samples[base + ch] = (out.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            }
            state.advance(channels);
        }

        drop(state);
        self.publish_reduction(min_gain);
    }
}

impl LimiterState {
    #[inline]
    fn step(&mut self, snap: &LimiterRuntimeSnapshot, peak: f64) -> f64 {
        let target = if peak > snap.ceiling_linear {
            snap.ceiling_linear / peak
        } else {
            1.0
        };

        while self.window.back().is_some_and(|&(_, v)| v >= target) {
            self.window.pop_back();
        }
        self.window.push_back((self.frame, target));
        let oldest = self.frame.saturating_sub(snap.delay_frames as u64);
        while self.window.front().is_some_and(|&(i, _)| i < oldest) {
            self.window.pop_front();
        }
        let goal = self.window.front().map(|&(_, v)| v).unwrap_or(1.0);

        let coeff = if goal < self.envelope {
            snap.attack_coeff
        } else {
            snap.release_coeff
        };
        self.envelope += (goal - self.envelope) * coeff;
        self.envelope
    }

    #[inline]
    fn advance(&mut self, channels: usize) {
        self.frame = self.frame.wrapping_add(1);
        self.write_pos += channels;
        if self.write_pos >= self.ring.len() {
            self.write_pos = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn context(settings: LimiterSettings) -> LimiterDspContext {
        let ctx = LimiterDspContext::new();
        ctx.configure_stream(44100, 2);
        ctx.set_settings(settings);
        ctx
    }

    #[test]
    fn hard_drive_never_exceeds_ceiling() {
        let ctx = context(LimiterSettings {
            enabled: true,
            gain_db: 12.0,
            ceiling_db: 0.0,
            release_ms: 60.0,
            clip: false,
        });

        let mut peak = 0.0f32;
        let mut buf = [0.0f32; 4410];
        let block_len = buf.len();
        for block in 0..20 {
            for (i, s) in buf.iter_mut().enumerate() {
                let t = (block * block_len + i) as f64 / 44100.0;
                *s = (2.0 * PI * 60.0 * t).sin() as f32;
            }
            ctx.process_buffer_f32(&mut buf);
            if block > 0 {
                for &s in &buf {
                    peak = peak.max(s.abs());
                }
            }
        }

        assert!(peak <= 1.0, "limiter let a peak through: {peak:.6}");
        assert!(peak > 0.98, "limiter left too much headroom: {peak:.6}");
    }

    #[test]
    fn transient_does_not_escape_lookahead() {
        let ctx = context(LimiterSettings {
            enabled: true,
            gain_db: 12.0,
            ceiling_db: 0.0,
            release_ms: 1000.0,
            clip: false,
        });

        let mut buf = [0.0f32; 2048];
        ctx.process_buffer_f32(&mut buf);

        let mut hit = [0.0f32; 2048];
        for (i, s) in hit.iter_mut().enumerate() {
            if i >= 1024 {
                *s = if i % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
        ctx.process_buffer_f32(&mut hit);
        let peak = hit.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak <= 1.0, "transient escaped the limiter: {peak:.6}");
    }

    #[test]
    fn quiet_signal_passes_unchanged() {
        let ctx = context(LimiterSettings {
            enabled: true,
            gain_db: 0.0,
            ceiling_db: 0.0,
            release_ms: 100.0,
            clip: false,
        });

        let mut buf = [0.01f32; 2048];
        ctx.process_buffer_f32(&mut buf);
        buf = [0.01f32; 2048];
        ctx.process_buffer_f32(&mut buf);

        for &s in &buf {
            assert!((s - 0.01).abs() < 1.0e-6, "quiet signal altered: {s}");
        }
        assert!(ctx.reduction_db() < 0.01, "meter showed phantom reduction");
    }

    #[test]
    fn clip_squares_peaks_immediately() {
        let ctx = context(LimiterSettings {
            enabled: true,
            gain_db: 12.0,
            ceiling_db: 0.0,
            release_ms: 60.0,
            clip: true,
        });

        let mut buf = [0.0f32; 1024];
        for (i, s) in buf.iter_mut().enumerate() {
            *s = (2.0 * PI * 60.0 * i as f64 / 44100.0).sin() as f32 * 0.5;
        }
        let expected_first = (buf[0] as f64 * 10.0_f64.powf(12.0 / 20.0)) as f32;
        ctx.process_buffer_f32(&mut buf);

        assert!(
            (buf[0] - expected_first).abs() < 1.0e-6,
            "clip mode added latency: {} vs {expected_first}",
            buf[0]
        );

        let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(
            (peak - 1.0).abs() < 1.0e-6,
            "clip missed the ceiling: {peak:.6}"
        );

        let pinned = buf.iter().filter(|s| s.abs() >= 1.0 - 1.0e-6).count();
        assert!(
            pinned > buf.len() / 4,
            "signal was not actually chopped: {pinned}"
        );
        assert!(ctx.reduction_db() > 5.0, "meter ignored the chopping");
    }
}
