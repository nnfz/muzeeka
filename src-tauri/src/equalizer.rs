// 64-bit floating-point graphic equalizer — foobar2000-style 1/3-octave bands.
//
// Processes PCM in a BASS DSP callback. All filter math is done in f64
// (double precision), matching foobar2000's internal DSP pipeline. This
// eliminates coefficient quantization errors and rounding noise that
// accumulate across a 15-band biquad cascade when using f32.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Deserializer, Serialize};

pub const BAND_COUNT: usize = 15;

/// Standard 1/3-octave center frequencies (Hz), matching foobar2000's built-in EQ.
pub const BAND_FREQUENCIES: [f32; BAND_COUNT] = [
    25.0, 40.0, 63.0, 100.0, 160.0, 250.0, 400.0, 630.0, 1000.0, 1600.0, 2500.0, 4000.0,
    6300.0, 10000.0, 16000.0,
];

/// Q factor for 1/3-octave bandwidth: Q = 1 / (2 * sinh(ln(2)/2 * 1/3)) ≈ 4.318
const BAND_Q: f64 = 4.318;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[inline(always)]
fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

#[inline(always)]
fn process_frame(
    sample: f64,
    preamp: f64,
    coeffs: &[BiquadCoeffs],
    states: &mut [BiquadState],
) -> f64 {
    let mut value = sample * preamp;
    for (state, coeff) in states.iter_mut().zip(coeffs.iter()) {
        value = state.process(value, coeff);
    }
    value
}

/// Thread-safe EQ context passed as BASS DSP user data.
pub struct EqDspContext {
    settings: RwLock<EqualizerSettings>,
    enabled: AtomicBool,  // fast path check, updated on set_settings
    coeffs_dirty: AtomicBool,
    sample_rate: RwLock<f64>,
    channels: RwLock<usize>,
    /// Bytes per sample in the DSP buffer (2 = int16, 4 = float32).
    bytes_per_sample: AtomicU32,
    /// Set when BASS_CONFIG_FLOATDSP is active — buffer is always float32.
    float_dsp: AtomicBool,
    /// Set when DSP was attached with BASS_DSP_FLOAT.
    dsp_float_forced: AtomicBool,
    process_count: AtomicU64,
    coeffs: RwLock<Vec<BiquadCoeffs>>,
    states: Mutex<Vec<Vec<BiquadState>>>,
    /// Cached preamp linear gain, recomputed when coeffs are dirty.
    preamp_linear: RwLock<f64>,
}

impl EqDspContext {
    pub fn new() -> Self {
        Self {
            settings: RwLock::new(EqualizerSettings::default()),
            enabled: AtomicBool::new(false),
            coeffs_dirty: AtomicBool::new(true),
            sample_rate: RwLock::new(44100.0),
            channels: RwLock::new(2),
            bytes_per_sample: AtomicU32::new(4),
            float_dsp: AtomicBool::new(false),
            dsp_float_forced: AtomicBool::new(false),
            process_count: AtomicU64::new(0),
            coeffs: RwLock::new(vec![BiquadCoeffs::default(); BAND_COUNT]),
            states: Mutex::new(vec![vec![BiquadState::default(); BAND_COUNT]; 2]),
            preamp_linear: RwLock::new(1.0),
        }
    }

    pub fn set_float_dsp_enabled(&self, enabled: bool) {
        self.float_dsp.store(enabled, Ordering::Release);
    }

    pub fn set_dsp_float_forced(&self, forced: bool) {
        self.dsp_float_forced.store(forced, Ordering::Release);
        if forced {
            self.bytes_per_sample.store(4, Ordering::Release);
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

    pub fn configure_stream(&self, sample_rate: u32, channels: u32, channel_flags: u32) {
        let chans = channels.max(1) as usize;
        let float_channel = channel_flags & crate::bass::BASS_SAMPLE_FLOAT != 0;
        let bytes_per_sample = if self.dsp_float_forced.load(Ordering::Acquire)
            || self.float_dsp.load(Ordering::Acquire)
            || float_channel
        {
            4
        } else {
            2
        };

        let rate = if sample_rate > 0 {
            sample_rate as f64
        } else {
            44100.0
        };
        *self.sample_rate.write() = rate;
        *self.channels.write() = chans;
        self.bytes_per_sample
            .store(bytes_per_sample, Ordering::Release);
        *self.states.lock() =
            vec![vec![BiquadState::default(); BAND_COUNT]; chans];
        self.coeffs_dirty.store(true, Ordering::Release);
    }

    fn rebuild_coeffs_if_needed(&self) {
        if !self.coeffs_dirty.swap(false, Ordering::AcqRel) {
            return;
        }

        let settings = self.settings.read();
        let sample_rate = (*self.sample_rate.read()).max(8000.0);
        let mut coeffs = self.coeffs.write();
        coeffs.clear();
        coeffs.reserve(BAND_COUNT);

        for (i, &freq) in BAND_FREQUENCIES.iter().enumerate() {
            coeffs.push(peaking_coeffs(
                sample_rate,
                freq as f64,
                settings.bands_db[i] as f64,
                BAND_Q,
            ));
        }

        *self.preamp_linear.write() = db_to_linear(settings.preamp_db as f64);
    }

    /// Process interleaved 32-bit float PCM.
    /// Samples are promoted to f64 for processing, then truncated back to f32.
    pub fn process_buffer_f32(&self, samples: &mut [f32]) {
        // Fast path: avoid heavy locking if EQ off
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }

        self.rebuild_coeffs_if_needed();

        let channels = *self.channels.read();
        if channels == 0 || samples.is_empty() {
            return;
        }

        let preamp = *self.preamp_linear.read();
        let coeffs = self.coeffs.read();
        let mut states = self.states.lock();

        let frames = samples.len() / channels;
        self.process_count.fetch_add(1, Ordering::Relaxed);
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let sample = samples[idx] as f64;
                samples[idx] = process_frame(sample, preamp, &coeffs, &mut states[ch]) as f32;
            }
        }
    }

    /// Process interleaved 16-bit PCM.
    /// Samples are promoted to f64 for processing, then quantized back to i16.
    pub fn process_buffer_i16(&self, samples: &mut [i16]) {
        // Fast path: avoid heavy locking if EQ off
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }

        self.rebuild_coeffs_if_needed();

        let channels = *self.channels.read();
        if channels == 0 || samples.is_empty() {
            return;
        }

        let preamp = *self.preamp_linear.read();
        let coeffs = self.coeffs.read();
        let mut states = self.states.lock();

        let frames = samples.len() / channels;
        self.process_count.fetch_add(1, Ordering::Relaxed);
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let sample = samples[idx] as f64 / 32768.0;
                let processed = process_frame(sample, preamp, &coeffs, &mut states[ch]);
                samples[idx] = (processed.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            }
        }
    }
}

/// BASS DSP callback — must match DSPPROC signature.
pub unsafe extern "system" fn eq_dsp_callback(
    _handle: u32,
    _channel: u32,
    buffer: *mut std::ffi::c_void,
    length: u32,
    user: *mut std::ffi::c_void,
) {
    if buffer.is_null() || user.is_null() || length < 2 {
        return;
    }

    let ctx = &*(user as *const EqDspContext);
    let use_float = ctx.dsp_float_forced.load(Ordering::Acquire)
        || ctx.bytes_per_sample.load(Ordering::Acquire) >= 4;

    if use_float {
        let sample_count = (length / 4) as usize;
        if sample_count == 0 {
            return;
        }
        let samples =
            std::slice::from_raw_parts_mut(buffer as *mut f32, sample_count);
        ctx.process_buffer_f32(samples);
    } else {
        let sample_count = (length / 2) as usize;
        if sample_count == 0 {
            return;
        }
        let samples =
            std::slice::from_raw_parts_mut(buffer as *mut i16, sample_count);
        ctx.process_buffer_i16(samples);
    }
}