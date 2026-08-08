// Resonant low-pass / high-pass for the DSP rack.
//
// Same RBJ biquads the Mix Transition preview uses (`biquad.rs`), but with the Q
// exposed: `mix_filter.rs` hardcodes 0.707 because it is sweeping cutoffs
// automatically, whereas a filter you place in a rack by hand is only interesting
// if you can make it squelch.
//
// Realtime notes (mirrors equalizer.rs / limiter.rs):
// - Coefficients + channel count live in ArcSwap so the audio callback never
//   waits on UI writes.
// - Only IIR memory (`states`) uses a Mutex — mutated every buffer.
// - f32 / i16 process paths are duplicated on purpose so the hot loop stays free
//   of format branches.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::biquad::{highpass_coeffs, lowpass_coeffs, BiquadCoeffs, BiquadState};

/// LP at or above this counts as fully open — that stage bypasses.
pub const LP_OPEN_HZ: f32 = 20000.0;
/// HP at or below this counts as fully open — that stage bypasses.
pub const HP_OPEN_HZ: f32 = 20.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterSettings {
    pub enabled: bool,
    /// Low-pass cutoff in Hz. `LP_OPEN_HZ` = open.
    pub lp_hz: f32,
    /// High-pass cutoff in Hz. `HP_OPEN_HZ` = open.
    pub hp_hz: f32,
    /// Q for both stages. 0.707 is flat Butterworth; higher peaks at the cutoff.
    pub resonance: f32,
}

impl Default for FilterSettings {
    fn default() -> Self {
        // Both stages open, so dropping a fresh Filter into the rack is inaudible
        // until the user actually moves a cutoff.
        Self {
            enabled: false,
            lp_hz: LP_OPEN_HZ,
            hp_hz: HP_OPEN_HZ,
            resonance: 0.707,
        }
    }
}

impl FilterSettings {
    pub fn clamp(mut self) -> Self {
        self.lp_hz = self.lp_hz.clamp(HP_OPEN_HZ, LP_OPEN_HZ);
        self.hp_hz = self.hp_hz.clamp(HP_OPEN_HZ, LP_OPEN_HZ);
        // Above ~8 a self-resonant sweep can add 18+ dB at the cutoff, which is
        // past "character" and into clipping the mixer on its own.
        self.resonance = self.resonance.clamp(0.5, 8.0);
        self
    }

    fn lp_active(&self) -> bool {
        self.lp_hz < LP_OPEN_HZ
    }

    fn hp_active(&self) -> bool {
        self.hp_hz > HP_OPEN_HZ
    }
}

/// Immutable DSP parameters published atomically to the audio thread.
struct FilterRuntimeSnapshot {
    channels: usize,
    lp_on: bool,
    hp_on: bool,
    lp: BiquadCoeffs,
    hp: BiquadCoeffs,
}

impl Default for FilterRuntimeSnapshot {
    fn default() -> Self {
        Self {
            channels: 2,
            lp_on: false,
            hp_on: false,
            lp: BiquadCoeffs::default(),
            hp: BiquadCoeffs::default(),
        }
    }
}

/// One filter instance in the rack. Lives behind an `Arc` in `ChainSlot`.
pub struct FilterDspContext {
    settings: RwLock<FilterSettings>,
    enabled: AtomicBool,
    coeffs_dirty: AtomicBool,
    sample_rate_hz: AtomicU32,
    channels: AtomicU32,
    process_count: AtomicU64,
    runtime: ArcSwap<FilterRuntimeSnapshot>,
    /// Per-channel LP and HP memory. `Vec<(lp, hp)>` keeps the pair together so a
    /// channel-count change resizes both at once.
    states: Mutex<Vec<(BiquadState, BiquadState)>>,
}

impl Default for FilterDspContext {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterDspContext {
    pub fn new() -> Self {
        Self {
            settings: RwLock::new(FilterSettings::default()),
            enabled: AtomicBool::new(false),
            coeffs_dirty: AtomicBool::new(true),
            sample_rate_hz: AtomicU32::new(44100),
            channels: AtomicU32::new(2),
            process_count: AtomicU64::new(0),
            runtime: ArcSwap::from_pointee(FilterRuntimeSnapshot::default()),
            states: Mutex::new(vec![(BiquadState::default(), BiquadState::default()); 2]),
        }
    }

    pub fn get_settings(&self) -> FilterSettings {
        self.settings.read().clone()
    }

    pub fn set_settings(&self, settings: FilterSettings) {
        let s = settings.clamp();
        self.enabled.store(s.enabled, Ordering::Release);
        *self.settings.write() = s;
        self.coeffs_dirty.store(true, Ordering::Release);
    }

    pub fn process_count(&self) -> u64 {
        self.process_count.load(Ordering::Relaxed)
    }

    pub fn configure_stream(&self, sample_rate: u32, channels: u32) {
        let chans = channels.max(1);
        let rate = if sample_rate > 0 { sample_rate } else { 44100 };
        self.sample_rate_hz.store(rate, Ordering::Release);
        self.channels.store(chans, Ordering::Release);
        *self.states.lock() =
            vec![(BiquadState::default(), BiquadState::default()); chans as usize];
        self.coeffs_dirty.store(true, Ordering::Release);
        // Publish matching channel count immediately so the next buffer is consistent.
        self.rebuild_coeffs_if_needed();
    }

    fn rebuild_coeffs_if_needed(&self) {
        if !self.coeffs_dirty.swap(false, Ordering::AcqRel) {
            return;
        }

        let settings = self.settings.read().clone();
        let sample_rate = self.sample_rate_hz.load(Ordering::Acquire).max(8000) as f64;
        let channels = self.channels.load(Ordering::Acquire).max(1) as usize;
        let q = settings.resonance as f64;

        let lp_on = settings.lp_active();
        let hp_on = settings.hp_active();
        self.runtime.store(Arc::new(FilterRuntimeSnapshot {
            channels,
            lp_on,
            hp_on,
            lp: if lp_on {
                lowpass_coeffs(sample_rate, settings.lp_hz as f64, q)
            } else {
                BiquadCoeffs::default()
            },
            hp: if hp_on {
                highpass_coeffs(sample_rate, settings.hp_hz as f64, q)
            } else {
                BiquadCoeffs::default()
            },
        }));
    }

    /// Process interleaved 32-bit float PCM.
    ///
    /// Intentionally separate from `process_buffer_i16` so the hot loop stays free
    /// of format branches and inlines cleanly.
    pub fn process_buffer_f32(&self, samples: &mut [f32]) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        self.rebuild_coeffs_if_needed();

        let snap = self.runtime.load_full();
        // Both stages open is the common case for a freshly added filter — do not
        // burn a mutex and a full pass to multiply by 1.
        if (!snap.lp_on && !snap.hp_on) || snap.channels == 0 || samples.is_empty() {
            return;
        }

        let channels = snap.channels;
        let mut states = self.states.lock();
        if states.len() != channels {
            // Mid-reconfigure race: skip this buffer rather than panic / desync.
            return;
        }

        let frames = samples.len() / channels;
        self.process_count.fetch_add(1, Ordering::Relaxed);
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let mut x = samples[idx] as f64;
                let (lp, hp) = &mut states[ch];
                if snap.hp_on {
                    x = hp.process(x, &snap.hp);
                }
                if snap.lp_on {
                    x = lp.process(x, &snap.lp);
                }
                // Resonance can overshoot well past unity; leave the ceiling to a
                // limiter downstream but keep it inside float sanity.
                samples[idx] = x.clamp(-4.0, 4.0) as f32;
            }
        }
    }

    /// Process interleaved 16-bit PCM. Duplicated vs f32 — see `process_buffer_f32`.
    pub fn process_buffer_i16(&self, samples: &mut [i16]) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        self.rebuild_coeffs_if_needed();

        let snap = self.runtime.load_full();
        if (!snap.lp_on && !snap.hp_on) || snap.channels == 0 || samples.is_empty() {
            return;
        }

        let channels = snap.channels;
        let mut states = self.states.lock();
        if states.len() != channels {
            return;
        }

        let frames = samples.len() / channels;
        self.process_count.fetch_add(1, Ordering::Relaxed);
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let mut x = samples[idx] as f64 / 32768.0;
                let (lp, hp) = &mut states[ch];
                if snap.hp_on {
                    x = hp.process(x, &snap.hp);
                }
                if snap.lp_on {
                    x = lp.process(x, &snap.lp);
                }
                samples[idx] = (x.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn context(settings: FilterSettings) -> FilterDspContext {
        let ctx = FilterDspContext::new();
        ctx.configure_stream(44100, 2);
        ctx.set_settings(settings);
        ctx
    }

    /// Interleaved stereo: both channels carry the same tone. Feeding a mono
    /// buffer to a 2-channel context would make each channel step two samples at a
    /// time, so a per-channel biquad would see twice the frequency asked for.
    fn sine(frames: usize, hz: f64) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = (2.0 * PI * hz * i as f64 / 44100.0).sin() as f32 * 0.5;
            out.push(s);
            out.push(s);
        }
        out
    }

    /// RMS over the tail only: the first few ms are the filter's own settling.
    fn tail_rms(samples: &[f32]) -> f64 {
        let start = samples.len() / 2;
        let tail = &samples[start..];
        let sum: f64 = tail.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum / tail.len() as f64).sqrt()
    }

    /// A 200 Hz low-pass must kill 5 kHz and leave 100 Hz essentially alone.
    #[test]
    fn lowpass_kills_treble_passes_bass() {
        let ctx = context(FilterSettings {
            enabled: true,
            lp_hz: 200.0,
            hp_hz: HP_OPEN_HZ,
            resonance: 0.707,
        });

        let mut high = sine(8192, 5000.0);
        let dry_high = tail_rms(&high);
        ctx.process_buffer_f32(&mut high);
        let wet_high = tail_rms(&high);
        assert!(
            wet_high < dry_high * 0.02,
            "5 kHz survived a 200 Hz LP: {wet_high:.5} vs dry {dry_high:.5}"
        );

        // Fresh instance so the previous run's IIR memory does not carry over.
        let ctx = context(FilterSettings {
            enabled: true,
            lp_hz: 200.0,
            hp_hz: HP_OPEN_HZ,
            resonance: 0.707,
        });
        let mut low = sine(8192, 100.0);
        let dry_low = tail_rms(&low);
        ctx.process_buffer_f32(&mut low);
        let wet_low = tail_rms(&low);
        assert!(
            wet_low > dry_low * 0.85,
            "100 Hz was attenuated by a 200 Hz LP: {wet_low:.5} vs dry {dry_low:.5}"
        );
    }

    /// High-pass is the mirror image — 100 Hz goes, 5 kHz stays.
    #[test]
    fn highpass_kills_bass_passes_treble() {
        let ctx = context(FilterSettings {
            enabled: true,
            lp_hz: LP_OPEN_HZ,
            hp_hz: 2000.0,
            resonance: 0.707,
        });
        let mut low = sine(8192, 100.0);
        let dry_low = tail_rms(&low);
        ctx.process_buffer_f32(&mut low);
        assert!(
            tail_rms(&low) < dry_low * 0.02,
            "100 Hz survived a 2 kHz HP: {:.5}",
            tail_rms(&low)
        );

        let ctx = context(FilterSettings {
            enabled: true,
            lp_hz: LP_OPEN_HZ,
            hp_hz: 2000.0,
            resonance: 0.707,
        });
        let mut high = sine(8192, 8000.0);
        let dry_high = tail_rms(&high);
        ctx.process_buffer_f32(&mut high);
        assert!(
            tail_rms(&high) > dry_high * 0.85,
            "8 kHz was attenuated by a 2 kHz HP: {:.5}",
            tail_rms(&high)
        );
    }

    /// Both stages open must be bit-exact dry, not "close enough" — that is what
    /// makes adding a Filter to the rack safe before you touch anything.
    #[test]
    fn wide_open_is_bit_exact_dry() {
        let ctx = context(FilterSettings {
            enabled: true,
            lp_hz: LP_OPEN_HZ,
            hp_hz: HP_OPEN_HZ,
            resonance: 4.0,
        });
        let dry = sine(2048, 440.0);
        let mut wet = dry.clone();
        ctx.process_buffer_f32(&mut wet);
        assert_eq!(dry, wet, "open filter was not transparent");
        assert_eq!(ctx.process_count(), 0, "open filter still ran the loop");
    }

    /// Resonance is the reason this exists: at the cutoff, high Q must be louder
    /// than flat Q.
    #[test]
    fn resonance_peaks_at_the_cutoff() {
        let flat = context(FilterSettings {
            enabled: true,
            lp_hz: 1000.0,
            hp_hz: HP_OPEN_HZ,
            resonance: 0.707,
        });
        let sharp = context(FilterSettings {
            enabled: true,
            lp_hz: 1000.0,
            hp_hz: HP_OPEN_HZ,
            resonance: 6.0,
        });

        let mut a = sine(8192, 1000.0);
        let mut b = a.clone();
        flat.process_buffer_f32(&mut a);
        sharp.process_buffer_f32(&mut b);
        assert!(
            tail_rms(&b) > tail_rms(&a) * 2.0,
            "Q had no effect at the cutoff: flat {:.5} vs sharp {:.5}",
            tail_rms(&a),
            tail_rms(&b)
        );
    }
}
