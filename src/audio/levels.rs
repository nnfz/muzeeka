//! Three levels for the playing-track bars.
//!
//! Low is bass under ~200 Hz. Middle is where claps and plucks actually sit,
//! about 200 Hz–4.5 kHz. High is everything above that, so hats and air move
//! the top bar. A narrow needle at 60 Hz was ringing on the thump of those
//! hits and the crack fell between the needles.
//!
//! The mixer DSP runs ahead of the speakers by the playback buffer, so the
//! result is held until that audio is due. Nothing is decoded a second time,
//! and the callback does not allocate or take a lock.

use std::sync::atomic::{AtomicU32, Ordering};

use super::biquad::{self, BiquadCoeffs, BiquadState};

static RATE: AtomicU32 = AtomicU32::new(48_000);
static OUT: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
/// How long the measured audio still sits in the output buffer, in seconds.
static OUTPUT_DELAY_SECS: AtomicU32 = AtomicU32::new(0);

const LOW_HZ: f64 = 200.0;
const HIGH_HZ: f64 = 4_500.0;
/// A bit of lift so the top bar, which carries less energy, still moves.
const DISPLAY_GAIN: [f32; 3] = [1.0, 1.15, 1.7];
const ATTACK_SECS: f32 = 0.03;
const RELEASE_SECS: f32 = 0.14;
const HOP_SECS: f32 = 0.01;

/// Latest bar levels, each 0..1. Lock-free. Zero until the mixer has run.
pub fn levels() -> [f32; 3] {
    [
        f32::from_bits(OUT[0].load(Ordering::Relaxed)),
        f32::from_bits(OUT[1].load(Ordering::Relaxed)),
        f32::from_bits(OUT[2].load(Ordering::Relaxed)),
    ]
}

pub(crate) fn set_rate(rate: u32) {
    if rate >= 8_000 {
        RATE.store(rate, Ordering::Relaxed);
    }
}

/// Seconds from "the mixer just rendered this" to "the speakers play it".
pub(crate) fn set_output_delay(secs: f32) {
    let secs = secs.clamp(0.0, 1.2);
    OUTPUT_DELAY_SECS.store(secs.to_bits(), Ordering::Relaxed);
}

fn output_delay_secs() -> f32 {
    f32::from_bits(OUTPUT_DELAY_SECS.load(Ordering::Relaxed))
}

/// Low / mid / high energy. State stays with the caller so a test can feed a
/// buffer without starting BASS.
pub struct BandMeter {
    rate: f32,
    hop: u32,
    low_c: [BiquadCoeffs; 2],
    mid_hp_c: [BiquadCoeffs; 2],
    mid_lp_c: [BiquadCoeffs; 2],
    high_c: [BiquadCoeffs; 2],
    low: [BiquadState; 2],
    mid_hp: [BiquadState; 2],
    mid_lp: [BiquadState; 2],
    high: [BiquadState; 2],
    acc: [f32; 3],
    acc_n: u32,
    shown: [f32; 3],
    /// Rendered frames. The delay ring is keyed by this, not by the wall clock.
    rendered: u64,
    hear_delay_secs: f32,
    delay_levels: [[f32; 3]; DELAY_CAP],
    delay_at: [u64; DELAY_CAP],
    delay_i: usize,
    delay_count: usize,
}

const DELAY_CAP: usize = 192;

impl BandMeter {
    pub fn new(rate: f32) -> Self {
        let rate = if rate >= 8_000.0 { rate } else { 48_000.0 };
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let sr = rate as f64;
        let pair = |lowpass: bool, hz: f64| {
            let c = if lowpass {
                biquad::lowpass_coeffs(sr, hz, q)
            } else {
                biquad::highpass_coeffs(sr, hz, q)
            };
            // Two Butterworth sections: 24 dB/octave, so a clap does not ring the bass bar.
            [c, c]
        };
        let hop = ((rate * HOP_SECS).round() as u32).clamp(64, 4_096);
        Self {
            rate,
            hop,
            low_c: pair(true, LOW_HZ),
            mid_hp_c: pair(false, LOW_HZ),
            mid_lp_c: pair(true, HIGH_HZ),
            high_c: pair(false, HIGH_HZ),
            low: [BiquadState::default(); 2],
            mid_hp: [BiquadState::default(); 2],
            mid_lp: [BiquadState::default(); 2],
            high: [BiquadState::default(); 2],
            acc: [0.0; 3],
            acc_n: 0,
            shown: [0.0; 3],
            rendered: 0,
            hear_delay_secs: 0.0,
            delay_levels: [[0.0; 3]; DELAY_CAP],
            delay_at: [0; DELAY_CAP],
            delay_i: 0,
            delay_count: 0,
        }
    }

    /// Hold measurements until the output buffer would have played them.
    /// A sudden drop means the playback buffer was flushed, so the held
    /// readings belong to audio that will not be heard.
    pub fn set_hear_delay_secs(&mut self, secs: f32) {
        let secs = secs.clamp(0.0, 1.2);
        if self.hear_delay_secs - secs > 0.2 {
            self.delay_count = 0;
            self.delay_i = 0;
            self.shown = [0.0; 3];
        }
        self.hear_delay_secs = secs;
    }

    /// `interleaved` is stereo float frames. Returns the smoothed 0..1 levels
    /// lined up with what is coming out of the speakers.
    pub fn push_stereo(&mut self, interleaved: &[f32]) -> [f32; 3] {
        for frame in interleaved.chunks_exact(2) {
            let sample = ((frame[0] + frame[1]) * 0.5) as f64;
            let low = run_pair(&mut self.low, &self.low_c, sample);
            let mid = run_pair(&mut self.mid_hp, &self.mid_hp_c, sample);
            let mid = run_pair(&mut self.mid_lp, &self.mid_lp_c, mid);
            let high = run_pair(&mut self.high, &self.high_c, sample);
            self.acc[0] += (low as f32) * (low as f32);
            self.acc[1] += (mid as f32) * (mid as f32);
            self.acc[2] += (high as f32) * (high as f32);
            self.acc_n += 1;
            self.rendered = self.rendered.saturating_add(1);
            if self.acc_n < self.hop {
                continue;
            }
            let n = self.acc_n as f32;
            let raw = [0, 1, 2].map(|band| shape((self.acc[band] / n).sqrt() * DISPLAY_GAIN[band]));
            self.acc = [0.0; 3];
            self.acc_n = 0;
            for (shown, target) in self.shown.iter_mut().zip(raw) {
                let secs = if target >= *shown {
                    ATTACK_SECS
                } else {
                    RELEASE_SECS
                };
                let keep = (-(self.hop as f32) / self.rate / secs).exp();
                *shown = target + (*shown - target) * keep;
            }
            self.delay_levels[self.delay_i] = self.shown;
            self.delay_at[self.delay_i] = self.rendered;
            self.delay_i = (self.delay_i + 1) % DELAY_CAP;
            if self.delay_count < DELAY_CAP {
                self.delay_count += 1;
            }
        }
        self.audible()
    }

    fn audible(&self) -> [f32; 3] {
        if self.delay_count == 0 {
            return [0.0; 3];
        }
        let delay_frames = (self.hear_delay_secs * self.rate) as u64;
        let heard = self.rendered.saturating_sub(delay_frames);
        let mut idx = if self.delay_i == 0 {
            DELAY_CAP - 1
        } else {
            self.delay_i - 1
        };
        for _ in 0..self.delay_count {
            if self.delay_at[idx] <= heard {
                return self.delay_levels[idx];
            }
            idx = if idx == 0 { DELAY_CAP - 1 } else { idx - 1 };
        }
        [0.0; 3]
    }
}

fn run_pair(state: &mut [BiquadState; 2], coeffs: &[BiquadCoeffs; 2], input: f64) -> f64 {
    let mid = state[0].process(input, &coeffs[0]);
    state[1].process(mid, &coeffs[1])
}

fn shape(rms: f32) -> f32 {
    if rms < 1.0e-4 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    // −44 dBFS is the stub. A full-scale sine (rms −3 dB) sits near 0.8.
    ((db + 44.0) / 52.0).clamp(0.0, 1.0)
}

struct LocalMeter {
    rate: u32,
    meter: BandMeter,
}

thread_local! {
    static LOCAL: std::cell::RefCell<Option<LocalMeter>> = const { std::cell::RefCell::new(None) };
}

/// Mixer DSP. The buffer is the float mix that is about to play. It is not written.
pub(crate) unsafe extern "system" fn mixer_dsp(
    _handle: u32,
    _channel: u32,
    buffer: *mut std::ffi::c_void,
    length: u32,
    _user: *mut std::ffi::c_void,
) {
    if buffer.is_null() || length < 8 {
        return;
    }
    let samples = (length as usize) / std::mem::size_of::<f32>();
    if samples < 2 {
        return;
    }
    let interleaved = std::slice::from_raw_parts(buffer.cast::<f32>(), samples);
    let rate = RATE.load(Ordering::Relaxed).max(8_000);
    LOCAL.with(|slot| {
        let mut local = slot.borrow_mut();
        let meter = local.get_or_insert_with(|| LocalMeter {
            rate,
            meter: BandMeter::new(rate as f32),
        });
        if meter.rate != rate {
            *meter = LocalMeter {
                rate,
                meter: BandMeter::new(rate as f32),
            };
        }
        meter.meter.set_hear_delay_secs(output_delay_secs());
        let shown = meter.meter.push_stereo(interleaved);
        for (out, level) in OUT.iter().zip(shown) {
            out.store(level.to_bits(), Ordering::Relaxed);
        }
    });
}
