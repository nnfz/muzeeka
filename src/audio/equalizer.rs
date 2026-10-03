// 64-bit floating-point graphic equalizer — foobar2000-style 1/3-octave bands.
//
// Processes PCM in a BASS DSP callback. All filter math is done in f64
// (double precision), matching foobar2000's internal DSP pipeline. This
// eliminates coefficient quantization errors and rounding noise that
// accumulate across a multi-band biquad cascade when using f32.
//
// Realtime notes:
// - Coefficients + preamp + channel count live in ArcSwap so the audio
//   callback never waits on UI writes (set_settings / configure_stream).
// - Only IIR filter memory (`states`) uses a Mutex — must be shared mutably
//   across callbacks; reconfigure is rare.
// - f32 / i16 process paths are intentionally duplicated (not templated)
//   so the per-sample loop stays monomorphized without an is_float branch.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Deserializer, Serialize};

pub const BAND_COUNT: usize = 17;

/// Graphic EQ centers (Hz) through 20 kHz — closer to foobar2000's upper range.
/// Top band (20 kHz) is a high-shelf so cut/boost holds to Nyquist (not a peaking bell).
pub const BAND_FREQUENCIES: [f32; BAND_COUNT] = [
    25.0, 40.0, 63.0, 100.0, 160.0, 250.0, 400.0, 630.0, 1000.0, 1600.0, 2500.0, 4000.0,
    6300.0, 10000.0, 12500.0, 16000.0, 20000.0,
];

/// Q factor for 1/3-octave peaking bandwidth: Q = 1 / (2 * sinh(ln(2)/2 * 1/3)) ≈ 4.318
const BAND_Q: f64 = 4.318;

/// Shelf slope for the top high-shelf band (S = 1 → steepest RBJ shelf).
const HIGH_SHELF_S: f64 = 1.0;

/// Index of the highest band — peaking would return to 0 dB above the center,
/// so harsh air / cymbals come back. High-shelf keeps the cut (or boost) to Nyquist.
const HIGH_SHELF_BAND: usize = BAND_COUNT - 1;

fn deserialize_bands<'de, D>(deserializer: D) -> Result<[f32; BAND_COUNT], D::Error>
where
    D: Deserializer<'de>,
{
    let values: Vec<f32> = Vec::deserialize(deserializer)?;
    let mut bands = [0.0f32; BAND_COUNT];
    for (i, gain) in values.iter().take(BAND_COUNT).enumerate() {
        bands[i] = *gain;
    }
    Ok(bands)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqualizerSettings {
    pub enabled: bool,
    pub preamp_db: f32,
    #[serde(default, deserialize_with = "deserialize_bands")]
    pub bands_db: [f32; BAND_COUNT],
}

impl Default for EqualizerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp_db: 0.0,
            bands_db: [0.0; BAND_COUNT],
        }
    }
}

impl EqualizerSettings {
    pub fn clamp(mut self) -> Self {
        self.preamp_db = self.preamp_db.clamp(-15.0, 15.0);
        for gain in &mut self.bands_db {
            *gain = gain.clamp(-20.0, 20.0);
        }
        self
    }
}

// --- 64-bit biquad filter ---

#[derive(Clone, Copy)]
struct BiquadCoeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct BiquadState {
    z1: f64,
    z2: f64,
}

/// Flush subnormals to zero. Tiny IIR residues from quiet passages otherwise
/// become denormal floats and can burn a full CPU core (track-dependent lag).
#[inline(always)]
fn undenormal(x: f64) -> f64 {
    if x.abs() < 1.0e-15 {
        0.0
    } else {
        x
    }
}

impl BiquadState {
    #[inline(always)]
    fn process(&mut self, input: f64, c: &BiquadCoeffs) -> f64 {
        let output = c.b0 * input + self.z1;
        self.z1 = undenormal(c.b1 * input - c.a1 * output + self.z2);
        self.z2 = undenormal(c.b2 * input - c.a2 * output);
        undenormal(output)
    }
}

/// Peaking EQ coefficients (RBJ Audio EQ Cookbook), computed in f64.
fn peaking_coeffs(sample_rate: f64, freq: f64, gain_db: f64, q: f64) -> BiquadCoeffs {
    if gain_db.abs() < 0.001 {
        return BiquadCoeffs::default();
    }

    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * freq / sample_rate;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * q);

    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos_w0;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha / a;

    let inv_a0 = 1.0 / a0;

    BiquadCoeffs {
        b0: b0 * inv_a0,
        b1: b1 * inv_a0,
        b2: b2 * inv_a0,
        a1: a1 * inv_a0,
        a2: a2 * inv_a0,
    }
}

/// High-shelf coefficients (RBJ Audio EQ Cookbook).
/// Gain applies from ~freq up through Nyquist — does not return to 0 dB like a bell.
fn high_shelf_coeffs(sample_rate: f64, freq: f64, gain_db: f64, shelf_slope: f64) -> BiquadCoeffs {
    if gain_db.abs() < 0.001 {
        return BiquadCoeffs::default();
    }

    let a = 10f64.powf(gain_db / 40.0);
    // Keep w0 safely below Nyquist so cos/sin stay well-behaved at high rates.
    let w0 = (2.0 * PI * freq / sample_rate).min(PI * 0.99);
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let s = shelf_slope.max(0.1);
    let alpha = (sin_w0 / 2.0) * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).sqrt();
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

    let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
    let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
    let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
    let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
    let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
    let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

    let inv_a0 = 1.0 / a0;
    BiquadCoeffs {
        b0: b0 * inv_a0,
        b1: b1 * inv_a0,
        b2: b2 * inv_a0,
        a1: a1 * inv_a0,
        a2: a2 * inv_a0,
    }
}

#[inline(always)]
fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

#[inline(always)]
fn process_frame(
    sample: f64,
    preamp: f64,
    coeffs: &[BiquadCoeffs; BAND_COUNT],
    states: &mut [BiquadState],
) -> f64 {
    let mut value = sample * preamp;
    for (state, coeff) in states.iter_mut().zip(coeffs.iter()) {
        value = state.process(value, coeff);
    }
    value
}

/// Immutable DSP parameters published atomically to the audio thread.
struct EqRuntimeSnapshot {
    channels: usize,
    preamp_linear: f64,
    coeffs: [BiquadCoeffs; BAND_COUNT],
}

impl Default for EqRuntimeSnapshot {
    fn default() -> Self {
        Self {
            channels: 2,
            preamp_linear: 1.0,
            coeffs: [BiquadCoeffs::default(); BAND_COUNT],
        }
    }
}

/// Thread-safe EQ context. One instance per rack slot, driven by `dsp_chain`.
pub struct EqDspContext {
    settings: RwLock<EqualizerSettings>,
    enabled: AtomicBool,
    coeffs_dirty: AtomicBool,
    sample_rate_hz: AtomicU32,
    channels: AtomicUsize,
    process_count: AtomicU64,
    /// Coeffs / preamp / channel count — lock-free load in the audio callback.
    runtime: ArcSwap<EqRuntimeSnapshot>,
    /// IIR memory only; must stay Mutex (mutated every buffer).
    states: Mutex<Vec<Vec<BiquadState>>>,
}

impl Default for EqDspContext {
    fn default() -> Self {
        Self::new()
    }
}

impl EqDspContext {
    pub fn new() -> Self {
        Self {
            settings: RwLock::new(EqualizerSettings::default()),
            enabled: AtomicBool::new(false),
            coeffs_dirty: AtomicBool::new(true),
            sample_rate_hz: AtomicU32::new(44100),
            channels: AtomicUsize::new(2),
            process_count: AtomicU64::new(0),
            runtime: ArcSwap::from_pointee(EqRuntimeSnapshot::default()),
            states: Mutex::new(vec![vec![BiquadState::default(); BAND_COUNT]; 2]),
        }
    }

    pub fn process_count(&self) -> u64 {
        self.process_count.load(Ordering::Relaxed)
    }

    pub fn get_settings(&self) -> EqualizerSettings {
        self.settings.read().clone()
    }

    pub fn set_settings(&self, settings: EqualizerSettings) {
        let s = settings.clamp();
        self.enabled.store(s.enabled, Ordering::Release);
        *self.settings.write() = s;
        self.coeffs_dirty.store(true, Ordering::Release);
    }

    /// Sample format is the chain's business (`dsp_chain.rs` decides once per
    /// buffer for the whole rack), so this only takes rate and channel count.
    pub fn configure_stream(&self, sample_rate: u32, channels: u32) {
        let chans = channels.max(1) as usize;
        let rate = if sample_rate > 0 { sample_rate } else { 44100 };
        self.sample_rate_hz.store(rate, Ordering::Release);
        self.channels.store(chans, Ordering::Release);
        *self.states.lock() = vec![vec![BiquadState::default(); BAND_COUNT]; chans];
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
        let channels = self.channels.load(Ordering::Acquire).max(1);

        let mut coeffs = [BiquadCoeffs::default(); BAND_COUNT];
        for (i, &freq) in BAND_FREQUENCIES.iter().enumerate() {
            let gain = settings.bands_db[i] as f64;
            coeffs[i] = if i == HIGH_SHELF_BAND {
                // Top band: high shelf so cut/boost holds to Nyquist (foobar-style).
                high_shelf_coeffs(sample_rate, freq as f64, gain, HIGH_SHELF_S)
            } else {
                peaking_coeffs(sample_rate, freq as f64, gain, BAND_Q)
            };
        }

        self.runtime.store(Arc::new(EqRuntimeSnapshot {
            channels,
            preamp_linear: db_to_linear(settings.preamp_db as f64),
            coeffs,
        }));
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

        self.rebuild_coeffs_if_needed();

        let snap = self.runtime.load_full();
        let channels = snap.channels;
        if channels == 0 || samples.is_empty() {
            return;
        }

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
                let sample = samples[idx] as f64;
                samples[idx] =
                    process_frame(sample, snap.preamp_linear, &snap.coeffs, &mut states[ch]) as f32;
            }
        }
    }

    /// Process interleaved 16-bit PCM.
    /// Samples are promoted to f64 for processing, then quantized back to i16.
    ///
    /// Duplicated vs f32 path on purpose — see `process_buffer_f32`.
    pub fn process_buffer_i16(&self, samples: &mut [i16]) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }

        self.rebuild_coeffs_if_needed();

        let snap = self.runtime.load_full();
        let channels = snap.channels;
        if channels == 0 || samples.is_empty() {
            return;
        }

        let mut states = self.states.lock();
        if states.len() != channels {
            return;
        }

        let frames = samples.len() / channels;
        self.process_count.fetch_add(1, Ordering::Relaxed);
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let sample = samples[idx] as f64 / 32768.0;
                let processed =
                    process_frame(sample, snap.preamp_linear, &snap.coeffs, &mut states[ch]);
                samples[idx] = (processed.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// High shelf at −12 dB / 16 kHz must still attenuate near Nyquist, unlike peaking.
    #[test]
    fn high_shelf_stays_down_above_center() {
        let sr = 48000.0;
        let peak = peaking_coeffs(sr, 20000.0, -12.0, BAND_Q);
        let shelf = high_shelf_coeffs(sr, 20000.0, -12.0, HIGH_SHELF_S);

        // Frequency response magnitude of a biquad at normalized ω.
        fn mag(c: &BiquadCoeffs, w: f64) -> f64 {
            let z1_re = w.cos();
            let z1_im = -w.sin();
            let z2_re = (2.0 * w).cos();
            let z2_im = -(2.0 * w).sin();
            let num_re = c.b0 + c.b1 * z1_re + c.b2 * z2_re;
            let num_im = c.b1 * z1_im + c.b2 * z2_im;
            let den_re = 1.0 + c.a1 * z1_re + c.a2 * z2_re;
            let den_im = c.a1 * z1_im + c.a2 * z2_im;
            let num = (num_re * num_re + num_im * num_im).sqrt();
            let den = (den_re * den_re + den_im * den_im).sqrt();
            num / den
        }

        // Just below Nyquist (24 kHz @ 48 kHz) — above the 20 kHz center.
        let w = 2.0 * PI * 23000.0 / sr;
        let peak_db = 20.0 * mag(&peak, w).log10();
        let shelf_db = 20.0 * mag(&shelf, w).log10();

        // Peaking has largely returned toward 0 dB; shelf should still be strongly cut.
        assert!(
            peak_db > -6.0,
            "peaking should recover toward 0 dB above center, got {peak_db:.2} dB"
        );
        assert!(
            shelf_db < -8.0,
            "high shelf should stay cut above center, got {shelf_db:.2} dB"
        );
    }
}
