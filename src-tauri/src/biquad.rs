//! Shared RBJ-cookbook biquad primitives.
//!
//! Extracted from `mix_filter.rs` so the DSP-chain `Filter` effect can reuse the
//! exact same math the Mix Transition preview has been shipping. `equalizer.rs`
//! keeps its own peaking / high-shelf coefficients — different formulas, and its
//! hot loop is tuned around them.

use std::f64::consts::PI;

#[derive(Clone, Copy)]
pub struct BiquadCoeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
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
pub struct BiquadState {
    z1: f64,
    z2: f64,
}

#[inline(always)]
pub fn undenormal(x: f64) -> f64 {
    if x.abs() < 1.0e-15 {
        0.0
    } else {
        x
    }
}

impl BiquadState {
    #[inline(always)]
    pub fn process(&mut self, input: f64, c: &BiquadCoeffs) -> f64 {
        let output = c.b0 * input + self.z1;
        self.z1 = undenormal(c.b1 * input - c.a1 * output + self.z2);
        self.z2 = undenormal(c.b2 * input - c.a2 * output);
        undenormal(output)
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

pub fn lowpass_coeffs(sample_rate: f64, freq: f64, q: f64) -> BiquadCoeffs {
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

pub fn highpass_coeffs(sample_rate: f64, freq: f64, q: f64) -> BiquadCoeffs {
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
