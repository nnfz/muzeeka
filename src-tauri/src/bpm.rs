//! Energy-envelope BPM detection via BASS decode streams.
//!
//! Runs on the player's BASS thread (same device as playback). Keeps analysis
//! short so the UI does not freeze.

use crate::bass::{self, BassLibrary};
use crate::player::Player;

const MIN_BPM: f32 = 70.0;
const MAX_BPM: f32 = 180.0;
/// Seconds of audio to analyze (middle of the track when possible).
const ANALYZE_SECS: f64 = 45.0;
/// Skip this many seconds from the start (intros / silence).
const SKIP_SECS: f64 = 12.0;
/// Energy hop size in mono samples (~5.8 ms at 44.1 kHz) — finer BPM lag grid.
const HOP: usize = 256;

/// Detect BPM for an on-disk audio file.
pub fn detect_bpm_for_path(player: &Player, path: &str) -> Result<f32, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("Empty audio path".into());
    }
    if !std::path::Path::new(&path).is_file() {
        return Err(format!("File not found: {path}"));
    }

    // Ensure BASS is up (setup already does this; safe to call again).
    player.init()?;

    player.with_bass(move |bass| detect_bpm_with_bass(bass, &path))
}

fn open_decode(bass: &BassLibrary, path: &str) -> Result<u32, String> {
    // Prefer float mono for analysis; fall back if the codec rejects MONO.
    let attempts = [
        bass::BASS_STREAM_DECODE | bass::BASS_SAMPLE_FLOAT | bass::BASS_SAMPLE_MONO,
        bass::BASS_STREAM_DECODE | bass::BASS_SAMPLE_FLOAT,
        bass::BASS_STREAM_DECODE, // 16-bit stereo → convert below
    ];
    let mut last_err = String::from("Failed to open decode stream");
    for flags in attempts {
        match bass.stream_create_file(path, flags) {
            Ok(h) if h != 0 => return Ok(h),
            Ok(_) => last_err = bass.last_error_string(),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn detect_bpm_with_bass(bass: &BassLibrary, path: &str) -> Result<f32, String> {
    let handle = open_decode(bass, path)?;

    let result = (|| {
        let info = bass.channel_get_info(handle)?;
        let sample_rate = if info.freq > 0 { info.freq } else { 44100 };
        let chans = info.chans.max(1) as usize;
        let is_float = (info.flags & bass::BASS_SAMPLE_FLOAT) != 0;

        // Jump past intro when the file is long enough.
        let total_secs = {
            let len = bass.channel_get_length(handle, bass::BASS_POS_BYTE);
            if len == 0 || len == u64::MAX {
                0.0
            } else {
                bass.channel_bytes2seconds(handle, len).max(0.0)
            }
        };
        if total_secs > SKIP_SECS + ANALYZE_SECS + 5.0 {
            let pos = bass.channel_seconds2bytes(handle, SKIP_SECS);
            let _ = bass.channel_set_position(handle, pos, bass::BASS_POS_BYTE);
        }

        let max_mono = ((ANALYZE_SECS * sample_rate as f64) as usize).max(sample_rate as usize);
        let mut mono: Vec<f32> = Vec::with_capacity(max_mono);
        let mut float_chunk = vec![0f32; 8192 * chans];
        let mut i16_chunk = vec![0i16; 8192 * chans];

        let mut pulls = 0u32;
        let mut last_pull = 0usize;
        while mono.len() < max_mono && pulls < 50_000 {
            pulls += 1;
            let frames = if is_float {
                let n = bass.channel_get_data_f32(handle, &mut float_chunk)?;
                if n == 0 {
                    break;
                }
                last_pull = n;
                let frames = n / chans;
                let need = (max_mono - mono.len()).min(frames);
                for i in 0..need {
                    if chans == 1 {
                        mono.push(float_chunk[i]);
                    } else {
                        let mut s = 0.0f32;
                        for c in 0..chans {
                            s += float_chunk[i * chans + c];
                        }
                        mono.push(s / chans as f32);
                    }
                }
                frames
            } else {
                let n = bass.channel_get_data_i16(handle, &mut i16_chunk)?;
                if n == 0 {
                    break;
                }
                last_pull = n;
                let frames = n / chans;
                let need = (max_mono - mono.len()).min(frames);
                for i in 0..need {
                    if chans == 1 {
                        mono.push(i16_chunk[i] as f32 / 32768.0);
                    } else {
                        let mut s = 0.0f32;
                        for c in 0..chans {
                            s += i16_chunk[i * chans + c] as f32 / 32768.0;
                        }
                        mono.push(s / chans as f32);
                    }
                }
                frames
            };
            if frames == 0 {
                break;
            }
        }

        if mono.len() < (sample_rate as usize / 2) {
            return Err(format!(
                "Not enough audio to detect BPM (got {} samples, last pull {}, rate {} Hz, chans {}, float={})",
                mono.len(),
                last_pull,
                sample_rate,
                chans,
                is_float
            ));
        }

        estimate_bpm(&mono, sample_rate).ok_or_else(|| {
            "Could not detect a stable BPM — try tapping instead".to_string()
        })
    })();

    let _ = bass.channel_free(handle);
    match &result {
        Ok(bpm) => eprintln!("[bpm] detect ok for {path}: {bpm}"),
        Err(e) => eprintln!("[bpm] detect failed for {path}: {e}"),
    }
    result
}

/// Parabolic peak interpolation: given y[i-1], y[i], y[i+1], return fractional
/// offset in [-0.5, 0.5] from index i (0 when flat / degenerate).
fn parabolic_offset(ym1: f32, y0: f32, yp1: f32) -> f32 {
    let denom = 2.0 * (2.0 * y0 - yp1 - ym1);
    if denom.abs() < 1e-12 {
        return 0.0;
    }
    ((yp1 - ym1) / denom).clamp(-0.5, 0.5)
}

/// Prefer whole BPM when close (tags are almost always integers); else 0.5 step.
fn snap_bpm(bpm: f32) -> f32 {
    let nearest_int = bpm.round();
    if (bpm - nearest_int).abs() <= 0.4 {
        return nearest_int.clamp(MIN_BPM, MAX_BPM);
    }
    let half = (bpm * 2.0).round() / 2.0;
    half.clamp(MIN_BPM, MAX_BPM)
}

fn fold_bpm_range(mut bpm: f32) -> Option<f32> {
    while bpm < MIN_BPM {
        bpm *= 2.0;
    }
    while bpm > MAX_BPM {
        bpm *= 0.5;
    }
    if (MIN_BPM..=MAX_BPM).contains(&bpm) {
        Some(bpm)
    } else {
        None
    }
}

/// Envelope → autocorrelation + IOI histogram BPM estimate.
pub fn estimate_bpm(mono: &[f32], sample_rate: u32) -> Option<f32> {
    if mono.len() < sample_rate as usize / 2 || sample_rate < 8000 {
        return None;
    }

    // RMS energy per hop.
    let mut energy = Vec::with_capacity(mono.len() / HOP + 1);
    let mut i = 0;
    while i + HOP <= mono.len() {
        let mut e = 0.0f32;
        for s in &mono[i..i + HOP] {
            e += s * s;
        }
        energy.push((e / HOP as f32).sqrt());
        i += HOP;
    }
    if energy.len() < 64 {
        return None;
    }

    // Onset strength = positive energy diff, lightly smoothed.
    let mut onset = vec![0.0f32; energy.len()];
    for i in 1..energy.len() {
        let d = energy[i] - energy[i - 1];
        onset[i] = if d > 0.0 { d } else { 0.0 };
    }
    if onset.len() >= 3 {
        let mut sm = onset.clone();
        for i in 1..onset.len() - 1 {
            sm[i] = (onset[i - 1] + onset[i] * 2.0 + onset[i + 1]) * 0.25;
        }
        onset = sm;
    }

    let peak = onset.iter().cloned().fold(0.0f32, f32::max);
    if peak < 1e-9 {
        return None;
    }
    for v in &mut onset {
        *v /= peak;
    }

    let hop_rate = sample_rate as f32 / HOP as f32;
    let min_lag = ((hop_rate * 60.0 / MAX_BPM).floor() as usize).max(2);
    let max_lag = ((hop_rate * 60.0 / MIN_BPM).ceil() as usize).min(onset.len() / 2);
    if max_lag <= min_lag + 2 {
        return None;
    }

    // --- Autocorrelation (raw; no 2× bonus — that boosted double-tempo false peaks) ---
    let mut corr_scores = vec![0.0f32; max_lag + 1];
    let mut best_corr = f32::NEG_INFINITY;
    for lag in min_lag..=max_lag {
        let mut corr = 0.0f32;
        let n = onset.len() - lag;
        for i in 0..n {
            corr += onset[i] * onset[i + lag];
        }
        corr /= n as f32;
        corr_scores[lag] = corr;
        if corr > best_corr {
            best_corr = corr;
        }
    }
    if best_corr < 1e-9 {
        return None;
    }

    // --- Peak-picking IOI histogram ---
    let thresh = 0.12f32;
    let mut peaks: Vec<usize> = Vec::new();
    for i in 2..onset.len().saturating_sub(2) {
        if onset[i] >= thresh
            && onset[i] >= onset[i - 1]
            && onset[i] >= onset[i + 1]
            && onset[i] >= onset[i - 2]
            && onset[i] >= onset[i + 2]
            && peaks.last().map(|p| i - p >= min_lag).unwrap_or(true)
        {
            peaks.push(i);
        }
    }

    let mut hist = vec![0.0f32; max_lag + 1];
    for a in 0..peaks.len() {
        for b in (a + 1)..peaks.len().min(a + 10) {
            let lag = peaks[b] - peaks[a];
            if lag >= min_lag && lag <= max_lag {
                hist[lag] += 1.0;
            }
        }
    }
    let mut hist_sm = hist.clone();
    for i in min_lag..=max_lag {
        let lo = i.saturating_sub(1);
        let hi = (i + 1).min(max_lag);
        let mut s = 0.0f32;
        let mut c = 0.0f32;
        for j in lo..=hi {
            s += hist[j];
            c += 1.0;
        }
        hist_sm[i] = s / c;
    }
    let hist_peak = hist_sm.iter().cloned().fold(0.0f32, f32::max).max(1e-9);

    // Local peaks in the autocorrelation (not just global max — global often = double-tempo).
    let mut lag_peaks: Vec<usize> = Vec::new();
    for lag in (min_lag + 1)..max_lag {
        if corr_scores[lag] >= corr_scores[lag - 1]
            && corr_scores[lag] >= corr_scores[lag + 1]
            && corr_scores[lag] > best_corr * 0.35
        {
            lag_peaks.push(lag);
        }
    }
    // Always consider global max too.
    let global_max_lag = (min_lag..=max_lag)
        .max_by(|a, b| {
            corr_scores[*a]
                .partial_cmp(&corr_scores[*b])
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(min_lag);
    if !lag_peaks.contains(&global_max_lag) {
        lag_peaks.push(global_max_lag);
    }

    // Score each peak lag as a *musical* tempo candidate.
    // Prefer the period where BOTH the beat lag and 2×lag (two-beat) are strong,
    // and apply a mild prior around ~120 so 179 loses to 90/124 when ambiguous.
    let mut best_score = f32::NEG_INFINITY;
    let mut chosen_lag = global_max_lag;
    for &lag in &lag_peaks {
        let corr_n = corr_scores[lag] / best_corr;
        let hist_n = hist_sm[lag] / hist_peak;
        let lag2 = lag * 2;
        let two_beat = if lag2 <= max_lag {
            corr_scores[lag2] / best_corr
        } else {
            0.0
        };
        // Half-lag = double tempo. If half is *stronger*, this lag is likely the true beat
        // (we are at the slower period). If half is weak, lag may already be correct.
        let lag_half = lag / 2;
        let half = if lag_half >= min_lag && lag % 2 == 0 {
            corr_scores[lag_half] / best_corr
        } else {
            0.0
        };

        let bpm = 60.0 * hop_rate / lag as f32;
        let prior = tempo_prior(bpm);

        // Penalize being the double-tempo of a strong half-period.
        let double_penalty = if half > corr_n * 0.9 { 0.75 } else { 1.0 };
        // Reward having a solid two-beat correlation (true meters usually do).
        let bar_bonus = 1.0 + 0.2 * two_beat;

        let score = (corr_n * 0.55 + hist_n * 0.20 + two_beat * 0.25)
            * prior
            * double_penalty
            * bar_bonus;

        if score > best_score {
            best_score = score;
            chosen_lag = lag;
        }
    }

    if best_score < 0.05 && best_corr < 0.002 {
        return None;
    }

    // Walk toward slower tempi while 2×lag stays competitive (8th-notes → quarter).
    let mut lag_i = chosen_lag;
    loop {
        let lag2 = lag_i * 2;
        if lag2 > max_lag {
            break;
        }
        let c1 = corr_scores[lag_i];
        let c2 = corr_scores[lag2];
        if c2 >= c1 * 0.70 {
            lag_i = lag2;
            continue;
        }
        break;
    }

    // Sub-lag refine via parabola.
    let refined_lag = {
        let ym1 = if lag_i > min_lag {
            corr_scores[lag_i - 1]
        } else {
            corr_scores[lag_i]
        };
        let y0 = corr_scores[lag_i];
        let yp1 = if lag_i < max_lag {
            corr_scores[lag_i + 1]
        } else {
            corr_scores[lag_i]
        };
        let frac = parabolic_offset(ym1, y0, yp1);
        (lag_i as f32 + frac).max(min_lag as f32)
    };

    let mut bpm = fold_bpm_range(60.0 * hop_rate / refined_lag)?;
    bpm = snap_bpm(bpm);

    // Edge BPMs (near 70 or 180) are usually octave errors. Prefer the best
    // peak inside the common song range when it is not much weaker.
    if !(95.0..=150.0).contains(&bpm) {
        if let Some(alt) = best_bpm_in_range(
            95.0,
            150.0,
            hop_rate,
            min_lag,
            max_lag,
            &corr_scores,
            &hist_sm,
            hist_peak,
            best_corr,
        ) {
            let lag_cur = bpm_to_lag(bpm, hop_rate, min_lag, max_lag);
            let lag_alt = bpm_to_lag(alt, hop_rate, min_lag, max_lag);
            let c_cur = corr_scores.get(lag_cur).copied().unwrap_or(0.0);
            let c_alt = corr_scores.get(lag_alt).copied().unwrap_or(0.0);
            // Switch if the common-range peak is at least ~half as strong.
            if c_alt >= c_cur * 0.45 || c_alt >= best_corr * 0.40 {
                bpm = snap_bpm(alt);
            }
        }
    }

    // Final octave nudge for remaining high outliers (179 → 89.5 is still wrong,
    // but 160+ almost always wants /2 when half is in-range and not dead).
    if bpm > 155.0 {
        if let Some(half) = fold_bpm_range(bpm * 0.5) {
            let lag_h = bpm_to_lag(half, hop_rate, min_lag, max_lag);
            let lag_b = bpm_to_lag(bpm, hop_rate, min_lag, max_lag);
            let c_h = corr_scores.get(lag_h).copied().unwrap_or(0.0);
            let c_b = corr_scores.get(lag_b).copied().unwrap_or(0.0);
            if c_h >= c_b * 0.50 {
                bpm = snap_bpm(half);
            }
        }
    }

    Some(bpm)
}

fn bpm_to_lag(bpm: f32, hop_rate: f32, min_lag: usize, max_lag: usize) -> usize {
    let lag = (60.0 * hop_rate / bpm).round() as usize;
    lag.clamp(min_lag, max_lag)
}

/// Best BPM inside [lo, hi] by combined corr/hist score (parabolically refined).
fn best_bpm_in_range(
    lo: f32,
    hi: f32,
    hop_rate: f32,
    min_lag: usize,
    max_lag: usize,
    corr_scores: &[f32],
    hist_sm: &[f32],
    hist_peak: f32,
    best_corr: f32,
) -> Option<f32> {
    let lag_lo = bpm_to_lag(hi, hop_rate, min_lag, max_lag); // higher BPM → shorter lag
    let lag_hi = bpm_to_lag(lo, hop_rate, min_lag, max_lag);
    if lag_hi <= lag_lo + 1 {
        return None;
    }
    let mut best_lag = lag_lo;
    let mut best_s = f32::NEG_INFINITY;
    for lag in lag_lo..=lag_hi {
        let corr_n = corr_scores[lag] / best_corr.max(1e-9);
        let hist_n = hist_sm[lag] / hist_peak.max(1e-9);
        let bpm = 60.0 * hop_rate / lag as f32;
        let s = (corr_n * 0.75 + hist_n * 0.25) * tempo_prior(bpm);
        if s > best_s {
            best_s = s;
            best_lag = lag;
        }
    }
    let ym1 = if best_lag > min_lag {
        corr_scores[best_lag - 1]
    } else {
        corr_scores[best_lag]
    };
    let y0 = corr_scores[best_lag];
    let yp1 = if best_lag < max_lag {
        corr_scores[best_lag + 1]
    } else {
        corr_scores[best_lag]
    };
    let frac = parabolic_offset(ym1, y0, yp1);
    let refined = (best_lag as f32 + frac).max(min_lag as f32);
    fold_bpm_range(60.0 * hop_rate / refined)
}

/// Mild prior: common song tempos cluster near 100–130. Far outliers (e.g. 179)
/// need a clearly stronger correlation to win.
fn tempo_prior(bpm: f32) -> f32 {
    let z = (bpm - 118.0) / 28.0;
    let g = (-0.5 * z * z).exp();
    // Keep a floor so rare slow/fast tracks still work.
    0.30 + 0.70 * g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_clicks(sr: u32, bpm: f32, secs: f32) -> Vec<f32> {
        let n = (sr as f32 * secs) as usize;
        let period = (60.0 / bpm) * sr as f32;
        let mut mono = vec![0.0f32; n];
        let mut t = 0.0f32;
        while t < n as f32 {
            let i = t as usize;
            if i + 64 < n {
                for k in 0..64 {
                    let env = 1.0 - (k as f32 / 64.0);
                    mono[i + k] = env * env;
                }
            }
            t += period;
        }
        mono
    }

    #[test]
    fn detects_synthetic_120_bpm() {
        let sr = 44100u32;
        let mono = synth_clicks(sr, 120.0, 20.0);
        let est = estimate_bpm(&mono, sr).expect("bpm");
        assert!((est - 120.0).abs() < 1.0, "expected ~120, got {est}");
    }

    #[test]
    fn detects_synthetic_124_bpm() {
        let sr = 44100u32;
        let mono = synth_clicks(sr, 124.0, 25.0);
        let est = estimate_bpm(&mono, sr).expect("bpm");
        assert!(
            (est - 124.0).abs() < 1.0,
            "expected ~124, got {est} (was rounding to 122.5)"
        );
    }

    #[test]
    fn snap_prefers_integer() {
        assert_eq!(snap_bpm(123.8), 124.0);
        assert_eq!(snap_bpm(123.9), 124.0);
        assert_eq!(snap_bpm(122.2), 122.0);
        // Far from integer → half-step grid
        assert_eq!(snap_bpm(122.6), 122.5);
    }
}
