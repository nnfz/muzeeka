// Loudness stage for the mixer output — a look-ahead brickwall limiter, plus an
// opt-in hard-clip mode for when clean is not the point.
//
// Sits after the equalizer in the mixer's DSP chain. `gain_db` pushes the whole
// signal in (up to +12 dB). What happens at the ceiling depends on `clip`:
//
//   - `clip == false` (limiter): the ceiling is held without ever squaring off a
//     peak. Gets loud and dense, stays clean. See the guarantee below.
//   - `clip == true` (hard clip): peaks are simply chopped at the ceiling. No
//     look-ahead, no gain movement, no added latency — the harmonics that
//     chopping generates *are* the sound. This is distortion, on purpose.
//
// How the limiter's ceiling is actually guaranteed:
//   - Output is delayed by a `LOOKAHEAD_MS` ring, so at frame n the detector
//     already knows the peaks up to n while it is only emitting frame n−D.
//   - The applied gain is a *sliding minimum* of the per-sample target gain over
//     that window, so it has already reached its lowest value D frames before the
//     peak it was computed for reaches the output.
//   - A one-pole smoother (τ = D/4, ~98 % settled across the window) keeps gain
//     movement click-free without letting the peak escape; a final clamp catches
//     the ~2 % residual.
//
// Realtime notes (mirrors equalizer.rs):
// - Gain / ceiling / release / clip live in ArcSwap so the audio callback never
//   waits on UI writes.
// - Delay ring + envelope + detector deque live in a Mutex — mutated every buffer
//   in limiter mode, untouched in clip mode (which needs no history).
// - f32 / i16 process paths are duplicated on purpose, like the EQ, so the hot
//   loop stays free of format branches.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

/// How far ahead the peak detector sees. 3 ms is enough for the gain to slew
/// down transparently and short enough not to smear transients.
const LOOKAHEAD_MS: f64 = 3.0;

/// Attack time as a fraction of the look-ahead window. τ = D/4 leaves the
/// smoother ~98 % settled (1 − e⁻⁴) by the time the peak reaches the output.
const ATTACK_WINDOW_DIVISOR: f64 = 4.0;

/// Gain reduction floor reported to the meter.
const METER_FLOOR_DB: f64 = 40.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimiterSettings {
    pub enabled: bool,
    /// Pre-limiter gain in dB (0..+12) — how hard the signal is pushed in.
    pub gain_db: f32,
    /// Output ceiling in dBFS. 0 = absolute brickwall at full scale.
    pub ceiling_db: f32,
    /// Release time in ms — how fast the limiter gain recovers after a peak.
    pub release_ms: f32,
    /// Hard clip at the ceiling instead of limiting. Distortion on purpose.
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

/// Immutable DSP parameters published atomically to the audio thread.
struct LimiterRuntimeSnapshot {
    channels: usize,
    drive_linear: f64,
    ceiling_linear: f64,
    /// Look-ahead in frames (also the output delay).
    delay_frames: usize,
    /// One-pole coefficient for downward gain movement.
    attack_coeff: f64,
    /// One-pole coefficient for recovery.
    release_coeff: f64,
    /// Chop peaks at the ceiling instead of pulling the gain down.
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

/// Delay ring + detector window. Sized for one stream configuration.
struct LimiterState {
    /// Interleaved delay line, `delay_frames * channels` long.
    ring: Vec<f64>,
    write_pos: usize,
    /// Smoothed gain (linear, 1.0 = no reduction).
    envelope: f64,
    /// Monotonic deque of `(frame, target_gain)` for the sliding minimum.
    /// Never grows past `delay_frames + 1`, so it does not allocate mid-buffer.
    window: VecDeque<(u64, f64)>,
    /// Frame counter driving the window; wraps harmlessly after ~13 M years.
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

    /// Drop stale audio so re-enabling does not replay a few ms of old signal.
    fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write_pos = 0;
        self.envelope = 1.0;
        self.window.clear();
        self.frame = 0;
    }
}

/// Thread-safe limiter context. One instance per rack slot, driven by `dsp_chain`.
pub struct LimiterDspContext {
    settings: RwLock<LimiterSettings>,
    enabled: AtomicBool,
    params_dirty: AtomicBool,
    /// Set when the limiter is switched on — the next buffer clears stale state.
    reset_pending: AtomicBool,
    sample_rate_hz: AtomicU32,
    channels: AtomicUsize,
    process_count: AtomicU64,
    /// Latest gain reduction in dB, as f64 bits — polled by the UI meter.
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

    /// Zero the meter. Called when the chain leaves the mixer: no callback will
    /// run to clear it, so the reading would sit at its last value forever.
    pub fn clear_meter(&self) {
        self.reduction_db.store(0f64.to_bits(), Ordering::Relaxed);
    }

    pub fn process_count(&self) -> u64 {
        self.process_count.load(Ordering::Relaxed)
    }

    /// Gain reduction currently applied, in dB (0 = limiter idle).
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
        // Clip mode never writes the delay ring, so coming back into limiter mode
        // would otherwise emit whatever stale audio was last left in it.
        if turning_on || mode_changed {
            self.reset_pending.store(true, Ordering::Release);
        }
        self.params_dirty.store(true, Ordering::Release);
    }

    /// Sample format is the chain's business (`dsp_chain.rs` decides once per
    /// buffer for the whole rack), so this only takes rate and channel count.
    pub fn configure_stream(&self, sample_rate: u32, channels: u32) {
        let chans = channels.max(1) as usize;
        let rate = if sample_rate > 0 { sample_rate } else { 44100 };
        self.sample_rate_hz.store(rate, Ordering::Release);
        self.channels.store(chans, Ordering::Release);

        let delay_frames = Self::delay_frames_for(rate);
        *self.state.lock() = LimiterState::new(delay_frames, chans);
        self.params_dirty.store(true, Ordering::Release);
        // Publish matching channel count immediately so the next buffer is consistent.
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

        // One-pole coefficients: y += (x − y) · c, with c = 1 − e^(−1/τ_samples).
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

    /// Publish the buffer's worst gain reduction for the UI meter.
    fn publish_reduction(&self, min_gain: f64) {
        let db = if min_gain >= 1.0 {
            0.0
        } else {
            (-20.0 * min_gain.max(1.0e-6).log10()).min(METER_FLOOR_DB)
        };
        self.reduction_db.store(db.to_bits(), Ordering::Relaxed);
    }

    /// Process interleaved 32-bit float PCM.
    /// Samples are promoted to f64 for processing, then truncated back to f32.
    ///
    /// Intentionally separate from `process_buffer_i16` (not a shared generic body)
    /// so the hot loop stays free of format branches and inlines cleanly.
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

        // Clip mode needs no history, so it skips the mutex and the delay ring
        // entirely — and with it the 3 ms of latency the look-ahead costs.
        if snap.clip {
            let mut worst = 1.0f64;
            for s in samples.iter_mut() {
                let driven = *s as f64 * snap.drive_linear;
                let mag = driven.abs();
                if mag > snap.ceiling_linear {
                    // How much of the peak got chopped, as a gain ratio, so the
                    // meter reads the same units as the limiter's reduction.
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
            // Mid-reconfigure race: skip this buffer rather than desync the delay.
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

    /// Process interleaved 16-bit PCM.
    /// Samples are promoted to f64 for processing, then quantized back to i16.
    ///
    /// Duplicated vs f32 path on purpose — see `process_buffer_f32`.
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

        // See `process_buffer_f32` — clip mode is lock-free and latency-free.
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
    /// Feed one frame's peak to the detector and return the gain to apply to the
    /// sample leaving the delay line.
    ///
    /// `window` holds the target gain for every frame still inside the look-ahead,
    /// pruned to a monotonic increasing sequence — so its front is the minimum over
    /// `[frame − delay, frame]`. That is exactly the set of samples whose peaks the
    /// gain must already account for, which is what stops a transient escaping.
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

    /// The whole point: +12 dB of drive must not produce a single sample over the
    /// ceiling, and must still push the output right up against it.
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
            // Skip the first block: the delay line is still priming with silence.
            if block > 0 {
                for &s in &buf {
                    peak = peak.max(s.abs());
                }
            }
        }

        assert!(peak <= 1.0, "limiter let a peak through: {peak:.6}");
        assert!(peak > 0.98, "limiter left too much headroom: {peak:.6}");
    }

    /// A transient after silence must not escape — this is what the look-ahead buys.
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
        ctx.process_buffer_f32(&mut buf); // prime the delay line with silence

        // Silence, then an instant full-scale hit.
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

    /// Below the ceiling with no drive, the limiter is a pure delay — same values out.
    #[test]
    fn quiet_signal_passes_unchanged() {
        let ctx = context(LimiterSettings {
            enabled: true,
            gain_db: 0.0,
            ceiling_db: 0.0,
            release_ms: 100.0,
            clip: false,
        });

        // Constant level, so the delay does not shift what we compare against.
        let mut buf = [0.01f32; 2048];
        ctx.process_buffer_f32(&mut buf);
        buf = [0.01f32; 2048];
        ctx.process_buffer_f32(&mut buf);

        for &s in &buf {
            assert!((s - 0.01).abs() < 1.0e-6, "quiet signal altered: {s}");
        }
        assert!(ctx.reduction_db() < 0.01, "meter showed phantom reduction");
    }

    /// Clip mode squares peaks off at the ceiling from the very first sample —
    /// no delay line to prime, so unlike the limiter tests block 0 counts.
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

        // Zero latency: sample 0 is sample 0, driven — not a delayed zero.
        assert!(
            (buf[0] - expected_first).abs() < 1.0e-6,
            "clip mode added latency: {} vs {expected_first}",
            buf[0]
        );

        let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!((peak - 1.0).abs() < 1.0e-6, "clip missed the ceiling: {peak:.6}");

        // Flat tops are the tell: +12 dB on a 0.5 sine pins most of each half-cycle.
        let pinned = buf.iter().filter(|s| s.abs() >= 1.0 - 1.0e-6).count();
        assert!(pinned > buf.len() / 4, "signal was not actually chopped: {pinned}");
        assert!(ctx.reduction_db() > 5.0, "meter ignored the chopping");
    }
}
