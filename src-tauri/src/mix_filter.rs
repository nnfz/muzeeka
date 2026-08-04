//! Per-deck low-pass / high-pass for Mix Transition preview.
//!
//! Cutoffs are driven from the UI envelope (normalized 0..1 → Hz, matching
//! the frontend `envelopeVToDisplay` mapping). Outside all filter blocks the
//! stages bypass so dry audio is unchanged.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use parking_lot::Mutex;

use crate::bass;

// ── Biquad (RBJ cookbook) ────────────────────────────────────────────────────

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
        // Unity / bypass
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

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

fn lowpass_coeffs(sample_rate: f64, freq: f64, q: f64) -> BiquadCoeffs {
    let sr = sample_rate.max(8000.0);
    let f = freq.clamp(20.0, sr * 0.45);
    let q = q.max(0.1);
    let w0 = 2.0 * PI * f / sr;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * q);

    let b0 = (1.0 - cos_w0) / 2.0;
    let b1 = 1.0 - cos_w0;
    let b2 = (1.0 - cos_w0) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha;
    let inv = 1.0 / a0;
    BiquadCoeffs {
        b0: b0 * inv,
        b1: b1 * inv,
        b2: b2 * inv,
        a1: a1 * inv,
        a2: a2 * inv,
    }
}

fn highpass_coeffs(sample_rate: f64, freq: f64, q: f64) -> BiquadCoeffs {
    let sr = sample_rate.max(8000.0);
    let f = freq.clamp(20.0, sr * 0.45);
    let q = q.max(0.1);
    let w0 = 2.0 * PI * f / sr;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * q);

    let b0 = (1.0 + cos_w0) / 2.0;
    let b1 = -(1.0 + cos_w0);
    let b2 = (1.0 + cos_w0) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha;
    let inv = 1.0 / a0;
    BiquadCoeffs {
        b0: b0 * inv,
        b1: b1 * inv,
        b2: b2 * inv,
        a1: a1 * inv,
        a2: a2 * inv,
    }
}

/// Match frontend log mapping for low-pass envelope value → Hz.
pub fn envelope_v_to_lp_hz(v: f64) -> f64 {
    let x = v.clamp(0.0, 1.0);
    80.0 * (18000.0_f64 / 80.0).powf(x)
}

/// Match frontend log mapping for high-pass envelope value → Hz.
pub fn envelope_v_to_hp_hz(v: f64) -> f64 {
    let x = v.clamp(0.0, 1.0);
    20.0 * (8000.0_f64 / 20.0).powf(x)
}

const FILTER_Q: f64 = 0.707; // Butterworth-ish
/// LP above this ≈ open (bypass).
const LP_BYPASS_HZ: f32 = 16000.0;
/// HP below this ≈ open (bypass).
const HP_BYPASS_HZ: f32 = 28.0;

// ── Context ──────────────────────────────────────────────────────────────────

/// One deck's LP+HP filter state. Lifetime: while the deck source is live.
/// Owned via `Box` and passed as raw `*mut` into BASS DSP `user`.
pub struct MixFilterCtx {
    sample_rate_hz: AtomicU32,
    channels: AtomicUsize,
    bytes_per_sample: AtomicU32,
    float_dsp: AtomicBool,
    /// f32 bits; 0 = bypass LP
    lp_hz_bits: AtomicU32,
    /// f32 bits; 0 = bypass HP
    hp_hz_bits: AtomicU32,
    lp_enabled: AtomicBool,
    hp_enabled: AtomicBool,
    lp_states: Mutex<Vec<BiquadState>>,
    hp_states: Mutex<Vec<BiquadState>>,
    lp_coeffs: Mutex<BiquadCoeffs>,
    hp_coeffs: Mutex<BiquadCoeffs>,
    last_lp_bits: AtomicU32,
    last_hp_bits: AtomicU32,
}

impl MixFilterCtx {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            sample_rate_hz: AtomicU32::new(44100),
            channels: AtomicUsize::new(2),
            bytes_per_sample: AtomicU32::new(4),
            float_dsp: AtomicBool::new(true),
            lp_hz_bits: AtomicU32::new(0),
            hp_hz_bits: AtomicU32::new(0),
            lp_enabled: AtomicBool::new(false),
            hp_enabled: AtomicBool::new(false),
            lp_states: Mutex::new(vec![BiquadState::default(); 2]),
            hp_states: Mutex::new(vec![BiquadState::default(); 2]),
            lp_coeffs: Mutex::new(BiquadCoeffs::default()),
            hp_coeffs: Mutex::new(BiquadCoeffs::default()),
            last_lp_bits: AtomicU32::new(u32::MAX),
            last_hp_bits: AtomicU32::new(u32::MAX),
        })
    }

    pub fn configure(&self, sample_rate: u32, channels: u32, float_samples: bool) {
        let chans = channels.max(1) as usize;
        let rate = if sample_rate > 0 { sample_rate } else { 44100 };
        self.sample_rate_hz.store(rate, Ordering::Release);
        self.channels.store(chans, Ordering::Release);
        self.float_dsp.store(float_samples, Ordering::Release);
        self.bytes_per_sample
            .store(if float_samples { 4 } else { 2 }, Ordering::Release);
        {
            let mut lp = self.lp_states.lock();
            let mut hp = self.hp_states.lock();
            *lp = vec![BiquadState::default(); chans];
            *hp = vec![BiquadState::default(); chans];
        }
        // Force coeff rebuild
        self.last_lp_bits.store(u32::MAX, Ordering::Release);
        self.last_hp_bits.store(u32::MAX, Ordering::Release);
        self.rebuild_coeffs_if_needed();
    }

    /// `None` = bypass that stage.
    pub fn set_targets(&self, lp_hz: Option<f32>, hp_hz: Option<f32>) {
        match lp_hz {
            Some(hz) if hz < LP_BYPASS_HZ => {
                self.lp_enabled.store(true, Ordering::Release);
                self.lp_hz_bits
                    .store(hz.clamp(40.0, 20000.0).to_bits(), Ordering::Release);
            }
            _ => {
                self.lp_enabled.store(false, Ordering::Release);
                self.lp_hz_bits.store(0, Ordering::Release);
            }
        }
        match hp_hz {
            Some(hz) if hz > HP_BYPASS_HZ => {
                self.hp_enabled.store(true, Ordering::Release);
                self.hp_hz_bits
                    .store(hz.clamp(20.0, 12000.0).to_bits(), Ordering::Release);
            }
            _ => {
                self.hp_enabled.store(false, Ordering::Release);
                self.hp_hz_bits.store(0, Ordering::Release);
            }
        }
    }

    fn rebuild_coeffs_if_needed(&self) {
        let sr = self.sample_rate_hz.load(Ordering::Acquire).max(8000) as f64;
        let lp_bits = self.lp_hz_bits.load(Ordering::Acquire);
        let hp_bits = self.hp_hz_bits.load(Ordering::Acquire);
        let lp_on = self.lp_enabled.load(Ordering::Acquire);
        let hp_on = self.hp_enabled.load(Ordering::Acquire);

        if lp_bits != self.last_lp_bits.load(Ordering::Acquire) {
            self.last_lp_bits.store(lp_bits, Ordering::Release);
            let c = if lp_on && lp_bits != 0 {
                lowpass_coeffs(sr, f32::from_bits(lp_bits) as f64, FILTER_Q)
            } else {
                BiquadCoeffs::default()
            };
            *self.lp_coeffs.lock() = c;
            if !lp_on {
                for s in self.lp_states.lock().iter_mut() {
                    s.reset();
                }
            }
        }
        if hp_bits != self.last_hp_bits.load(Ordering::Acquire) {
            self.last_hp_bits.store(hp_bits, Ordering::Release);
            let c = if hp_on && hp_bits != 0 {
                highpass_coeffs(sr, f32::from_bits(hp_bits) as f64, FILTER_Q)
            } else {
                BiquadCoeffs::default()
            };
            *self.hp_coeffs.lock() = c;
            if !hp_on {
                for s in self.hp_states.lock().iter_mut() {
                    s.reset();
                }
            }
        }
    }

    pub fn process_buffer_f32(&self, samples: &mut [f32]) {
        let lp_on = self.lp_enabled.load(Ordering::Acquire);
        let hp_on = self.hp_enabled.load(Ordering::Acquire);
        if !lp_on && !hp_on {
            return;
        }
        self.rebuild_coeffs_if_needed();
        let channels = self.channels.load(Ordering::Acquire).max(1);
        if samples.is_empty() {
            return;
        }
        let lp_c = *self.lp_coeffs.lock();
        let hp_c = *self.hp_coeffs.lock();
        let mut lp_st = self.lp_states.lock();
        let mut hp_st = self.hp_states.lock();
        if lp_st.len() != channels {
            *lp_st = vec![BiquadState::default(); channels];
        }
        if hp_st.len() != channels {
            *hp_st = vec![BiquadState::default(); channels];
        }
        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let mut x = samples[idx] as f64;
                if hp_on {
                    x = hp_st[ch].process(x, &hp_c);
                }
                if lp_on {
                    x = lp_st[ch].process(x, &lp_c);
                }
                samples[idx] = x.clamp(-1.0, 1.0) as f32;
            }
        }
    }

    pub fn process_buffer_i16(&self, samples: &mut [i16]) {
        let lp_on = self.lp_enabled.load(Ordering::Acquire);
        let hp_on = self.hp_enabled.load(Ordering::Acquire);
        if !lp_on && !hp_on {
            return;
        }
        self.rebuild_coeffs_if_needed();
        let channels = self.channels.load(Ordering::Acquire).max(1);
        if samples.is_empty() {
            return;
        }
        let lp_c = *self.lp_coeffs.lock();
        let hp_c = *self.hp_coeffs.lock();
        let mut lp_st = self.lp_states.lock();
        let mut hp_st = self.hp_states.lock();
        if lp_st.len() != channels {
            *lp_st = vec![BiquadState::default(); channels];
        }
        if hp_st.len() != channels {
            *hp_st = vec![BiquadState::default(); channels];
        }
        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let mut x = samples[idx] as f64 / 32768.0;
                if hp_on {
                    x = hp_st[ch].process(x, &hp_c);
                }
                if lp_on {
                    x = lp_st[ch].process(x, &lp_c);
                }
                samples[idx] = (x.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            }
        }
    }
}

/// BASS DSP callback — user = `*mut MixFilterCtx`.
pub unsafe extern "system" fn mix_filter_dsp_callback(
    _handle: u32,
    _channel: u32,
    buffer: *mut std::ffi::c_void,
    length: u32,
    user: *mut std::ffi::c_void,
) {
    if buffer.is_null() || user.is_null() || length < 2 {
        return;
    }
    {
        use std::sync::atomic::{AtomicBool, Ordering as Ord};
        static REG: AtomicBool = AtomicBool::new(false);
        if !REG.swap(true, Ord::Relaxed) {
            crate::process_util::register_audio_thread();
        }
    }
    let ctx = &*(user as *const MixFilterCtx);
    let use_float = ctx.float_dsp.load(Ordering::Acquire)
        || ctx.bytes_per_sample.load(Ordering::Acquire) >= 4;
    if use_float {
        let n = (length / 4) as usize;
        if n == 0 {
            return;
        }
        let samples = std::slice::from_raw_parts_mut(buffer as *mut f32, n);
        ctx.process_buffer_f32(samples);
    } else {
        let n = (length / 2) as usize;
        if n == 0 {
            return;
        }
        let samples = std::slice::from_raw_parts_mut(buffer as *mut i16, n);
        ctx.process_buffer_i16(samples);
    }
}

/// Attach filter DSP to a playback channel. Returns (dsp handle, leaked-or-boxed ctx pointer ownership).
/// The `Box` is returned so the player can free it after removing DSP.
pub fn attach_mix_filter(
    bass: &bass::BassLibrary,
    channel: u32,
) -> Result<(u32, Box<MixFilterCtx>), String> {
    if channel == 0 {
        return Err("no channel".into());
    }
    let mut ctx = MixFilterCtx::new();
    let (rate, chans, float_ch) = match bass.channel_get_info(channel) {
        Ok(info) => {
            let float = info.flags & bass::BASS_SAMPLE_FLOAT != 0;
            (info.freq, info.chans.max(1), float)
        }
        Err(_) => (44100, 2, true),
    };
    // Prefer float DSP path (same as EQ).
    ctx.configure(rate, chans, true);
    let user = ctx.as_mut() as *mut MixFilterCtx as *mut std::ffi::c_void;
    let dsp = bass
        .channel_set_dsp_ex(
            channel,
            mix_filter_dsp_callback,
            user,
            // Run before mixer sum; after tempo if present on this channel.
            0,
            bass::BASS_DSP_FLOAT,
        )
        .or_else(|_| {
            bass.channel_set_dsp(channel, mix_filter_dsp_callback, 0, user)
        })?;
    // Re-configure if float force failed (rare).
    if float_ch {
        ctx.configure(rate, chans, true);
    }
    Ok((dsp, ctx))
}

pub fn detach_mix_filter(
    bass: &bass::BassLibrary,
    channel: u32,
    dsp: u32,
    _ctx: Box<MixFilterCtx>,
) {
    if channel != 0 && dsp != 0 {
        let _ = bass.channel_remove_dsp(channel, dsp);
    }
    // Box dropped here
}
