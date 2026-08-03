//! Energy-envelope BPM detection via BASS decode streams.
//!
//! Pipeline:
//! 1. Open a decode stream (float mono → float stereo → 16-bit)
//! 2. Pull mono PCM from several windows across the file
//! 3. Per window: energy envelope → onset → autocorrelation + IOI histogram
//! 4. Vote across windows, refine lag, fold tempo octave, snap for tags
//!
//! Runs on the player's BASS thread. Results are cached by path + mtime + size.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

use parking_lot::Mutex;

use crate::bass::{self, BassLibrary};
use crate::player::Player;

// ── Range / hop ──────────────────────────────────────────────────────────────
const MIN_BPM: f32 = 70.0;
const MAX_BPM: f32 = 180.0;
/// Energy hop in mono samples (~5.8 ms @ 44.1 kHz).
const HOP: usize = 256;
/// Common song-tempo band used for outlier rescue.
const SWEET_BPM_LO: f32 = 95.0;
const SWEET_BPM_HI: f32 = 150.0;

// ── Multi-window layout ──────────────────────────────────────────────────────
/// Length of each analysis window (seconds).
const WINDOW_SECS: f64 = 18.0;
/// Relative start positions in the track (0 = start, 1 = end).
const WINDOW_FRACS: [f64; 3] = [0.12, 0.42, 0.70];
/// Skip absolute intro even on short files.
const MIN_START_SKIP_SECS: f64 = 4.0;

// ── Onset / peak thresholds ──────────────────────────────────────────────────
const ONSET_PEAK_THRESH: f32 = 0.12;
const CORR_PEAK_REL: f32 = 0.35;
const WEAK_SCORE_MIN: f32 = 0.05;
const WEAK_CORR_MIN: f32 = 0.002;

// ── Candidate scoring weights ────────────────────────────────────────────────
const W_CORR: f32 = 0.55;
const W_HIST: f32 = 0.20;
const W_TWO_BEAT: f32 = 0.25;
const DOUBLE_TEMPO_HALF_RATIO: f32 = 0.9;
const DOUBLE_TEMPO_PENALTY: f32 = 0.75;
const BAR_BONUS_SCALE: f32 = 0.20;
const OCTAVE_DOWN_RATIO: f32 = 0.70;
const SWEET_ALT_CORR_RATIO: f32 = 0.45;
const SWEET_ALT_BEST_RATIO: f32 = 0.40;
/// Only consider /2 when result is this high *and* half lands in a musical band.
const HIGH_BPM_CUTOFF: f32 = 168.0;
const HIGH_BPM_HALF_MIN: f32 = 95.0;
const HIGH_BPM_HALF_RATIO: f32 = 0.65;

// ── Tempo prior (Gaussian around ~118 BPM) ───────────────────────────────────
const PRIOR_CENTER: f32 = 118.0;
const PRIOR_SIGMA: f32 = 28.0;
const PRIOR_FLOOR: f32 = 0.30;
const PRIOR_SCALE: f32 = 0.70;

// ── Snap ─────────────────────────────────────────────────────────────────────
const SNAP_INT_TOL: f32 = 0.4;

// ── Vote ─────────────────────────────────────────────────────────────────────
/// Candidates within this many BPM count as the same vote bin.
const VOTE_CLUSTER_BPM: f32 = 1.25;

// ── Cache ────────────────────────────────────────────────────────────────────
struct CacheKey {
    path: String,
    modified: SystemTime,
    size: u64,
}

struct CacheEntry {
    key: CacheKey,
    bpm: f32,
}

fn bpm_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_cache_key(path: &str) -> Option<CacheKey> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    Some(CacheKey {
        path: path.to_string(),
        modified,
        size: meta.len(),
    })
}

fn cache_get(path: &str) -> Option<f32> {
    let key = file_cache_key(path)?;
    let guard = bpm_cache().lock();
    let entry = guard.get(path)?;
    if entry.key.modified == key.modified && entry.key.size == key.size {
        Some(entry.bpm)
    } else {
        None
    }
}

fn cache_put(path: &str, bpm: f32) {
    let Some(key) = file_cache_key(path) else {
        return;
    };
    bpm_cache().lock().insert(
        path.to_string(),
        CacheEntry {
            key,
            bpm,
        },
    );
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Detect BPM for an on-disk audio file (cached by path + mtime + size).
pub fn detect_bpm_for_path(player: &Player, path: &str) -> Result<f32, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("Empty audio path".into());
    }
    if !Path::new(&path).is_file() {
        return Err(format!("File not found: {path}"));
    }

    if let Some(cached) = cache_get(&path) {
        eprintln!("[bpm] cache hit for {path}: {cached}");
        return Ok(cached);
    }

    player.init()?;
    let path_for_bass = path.clone();
    let bpm = player.with_bass(move |bass| detect_bpm_with_bass(bass, &path_for_bass))?;
    cache_put(&path, bpm);
    Ok(bpm)
}

// ── Decode / multi-window pull ───────────────────────────────────────────────

fn open_decode(bass: &BassLibrary, path: &str) -> Result<(u32, &'static str), String> {
    let attempts: [(u32, &str); 3] = [
        (
            bass::BASS_STREAM_DECODE | bass::BASS_SAMPLE_FLOAT | bass::BASS_SAMPLE_MONO,
            "float+mono",
        ),
        (
            bass::BASS_STREAM_DECODE | bass::BASS_SAMPLE_FLOAT,
            "float",
        ),
        (bass::BASS_STREAM_DECODE, "default-16bit"),
    ];
    let mut last_err = String::from("Failed to open decode stream");
    for (flags, label) in attempts {
        match bass.stream_create_file(path, flags) {
            Ok(h) if h != 0 => {
                eprintln!("[bpm] open_decode ok ({label}) for {path}");
                return Ok((h, label));
            }
            Ok(_) => last_err = format!("{label}: {}", bass.last_error_string()),
            Err(e) => last_err = format!("{label}: {e}"),
        }
    }
    Err(last_err)
}

struct DecodeInfo {
    sample_rate: u32,
    chans: usize,
    is_float: bool,
    total_secs: f64,
}

fn decode_info(bass: &BassLibrary, handle: u32) -> Result<DecodeInfo, String> {
    let info = bass.channel_get_info(handle)?;
    let sample_rate = if info.freq > 0 { info.freq } else { 44100 };
    let chans = info.chans.max(1) as usize;
    let is_float = (info.flags & bass::BASS_SAMPLE_FLOAT) != 0;
    let total_secs = {
        let len = bass.channel_get_length(handle, bass::BASS_POS_BYTE);
        if len == 0 || len == u64::MAX {
            0.0
        } else {
            bass.channel_bytes2seconds(handle, len).max(0.0)
        }
    };
    Ok(DecodeInfo {
        sample_rate,
        chans,
        is_float,
        total_secs,
    })
}

/// Pull up to `max_mono` mono samples from the current stream position.
fn pull_mono(
    bass: &BassLibrary,
    handle: u32,
    info: &DecodeInfo,
    max_mono: usize,
) -> Result<Vec<f32>, String> {
    let mut mono = Vec::with_capacity(max_mono);
    let mut float_chunk = vec![0f32; 8192 * info.chans];
    let mut i16_chunk = vec![0i16; 8192 * info.chans];

    // Bound iterations by remaining samples (not a magic fixed pull count).
    let chunk_frames = 8192usize;
    let max_pulls = (max_mono / chunk_frames).saturating_add(8).max(8);

    for _ in 0..max_pulls {
        if mono.len() >= max_mono {
            break;
        }
        let frames = if info.is_float {
            let n = bass.channel_get_data_f32(handle, &mut float_chunk)?;
            if n == 0 {
                break;
            }
            let frames = n / info.chans;
            let need = (max_mono - mono.len()).min(frames);
            for i in 0..need {
                if info.chans == 1 {
                    mono.push(float_chunk[i]);
                } else {
                    let mut acc = 0.0f32;
                    for c in 0..info.chans {
                        acc += float_chunk[i * info.chans + c];
                    }
                    mono.push(acc / info.chans as f32);
                }
            }
            frames
        } else {
            let n = bass.channel_get_data_i16(handle, &mut i16_chunk)?;
            if n == 0 {
                break;
            }
            let frames = n / info.chans;
            let need = (max_mono - mono.len()).min(frames);
            for i in 0..need {
                if info.chans == 1 {
                    mono.push(i16_chunk[i] as f32 / 32768.0);
                } else {
                    let mut acc = 0.0f32;
                    for c in 0..info.chans {
                        acc += i16_chunk[i * info.chans + c] as f32;
                    }
                    mono.push((acc / info.chans as f32) / 32768.0);
                }
            }
            frames
        };
        if frames == 0 {
            break;
        }
    }
    Ok(mono)
}

fn window_starts_secs(total_secs: f64) -> Vec<f64> {
    if total_secs <= WINDOW_SECS + MIN_START_SKIP_SECS {
        // Short file — single window from the start (tiny skip if possible).
        let start = if total_secs > WINDOW_SECS + 1.0 {
            MIN_START_SKIP_SECS.min(total_secs * 0.05)
        } else {
            0.0
        };
        return vec![start];
    }
    WINDOW_FRACS
        .iter()
        .map(|f| {
            let raw = total_secs * f;
            let max_start = (total_secs - WINDOW_SECS).max(0.0);
            raw.clamp(MIN_START_SKIP_SECS, max_start)
        })
        .collect()
}

fn detect_bpm_with_bass(bass: &BassLibrary, path: &str) -> Result<f32, String> {
    let (handle, _mode) = open_decode(bass, path)?;

    let result = (|| {
        let info = decode_info(bass, handle)?;
        let max_mono = ((WINDOW_SECS * info.sample_rate as f64) as usize).max(info.sample_rate as usize / 2);
        let starts = window_starts_secs(info.total_secs);

        let mut estimates: Vec<f32> = Vec::with_capacity(starts.len());
        for (i, start) in starts.iter().enumerate() {
            let pos = bass.channel_seconds2bytes(handle, *start);
            if let Err(e) = bass.channel_set_position(handle, pos, bass::BASS_POS_BYTE) {
                eprintln!("[bpm] seek window {i} @{start:.1}s failed: {e}");
                continue;
            }
            let mono = pull_mono(bass, handle, &info, max_mono)?;
            if mono.len() < info.sample_rate as usize / 2 {
                eprintln!(
                    "[bpm] window {i} too short ({} samples @ {} Hz)",
                    mono.len(),
                    info.sample_rate
                );
                continue;
            }
            match estimate_bpm(&mono, info.sample_rate) {
                Some(bpm) => {
                    eprintln!("[bpm] window {i} @{start:.1}s → {bpm}");
                    estimates.push(bpm);
                }
                None => eprintln!("[bpm] window {i} @{start:.1}s → no estimate"),
            }
        }

        if estimates.is_empty() {
            return Err(
                "Could not detect a stable BPM — try tapping instead".to_string(),
            );
        }

        let bpm = vote_bpm(&estimates).ok_or_else(|| {
            "Could not detect a stable BPM — try tapping instead".to_string()
        })?;
        Ok(bpm)
    })();

    if let Err(e) = bass.channel_free(handle) {
        eprintln!("[bpm] channel_free failed for {path}: {e}");
    }
    match &result {
        Ok(bpm) => eprintln!("[bpm] detect ok for {path}: {bpm}"),
        Err(e) => eprintln!("[bpm] detect failed for {path}: {e}"),
    }
    result
}

/// Cluster window estimates and pick the strongest bin (median of that bin).
fn vote_bpm(estimates: &[f32]) -> Option<f32> {
    if estimates.is_empty() {
        return None;
    }
    if estimates.len() == 1 {
        return Some(estimates[0]);
    }

    // Greedy clustering by proximity.
    let mut used = vec![false; estimates.len()];
    let mut best_cluster: Vec<f32> = Vec::new();

    for i in 0..estimates.len() {
        if used[i] {
            continue;
        }
        let mut cluster = vec![estimates[i]];
        used[i] = true;
        for j in (i + 1)..estimates.len() {
            if used[j] {
                continue;
            }
            if (estimates[j] - estimates[i]).abs() <= VOTE_CLUSTER_BPM {
                cluster.push(estimates[j]);
                used[j] = true;
            }
        }
        // Also merge members within tolerance of cluster mean (second pass).
        let mean = cluster.iter().sum::<f32>() / cluster.len() as f32;
        for j in 0..estimates.len() {
            if used[j] {
                continue;
            }
            if (estimates[j] - mean).abs() <= VOTE_CLUSTER_BPM {
                cluster.push(estimates[j]);
                used[j] = true;
            }
        }
        if cluster.len() > best_cluster.len()
            || (cluster.len() == best_cluster.len()
                && cluster_spread(&cluster) < cluster_spread(&best_cluster))
        {
            best_cluster = cluster;
        }
    }

    if best_cluster.is_empty() {
        return None;
    }
    best_cluster.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = best_cluster[best_cluster.len() / 2];
    Some(snap_bpm(mid))
}

fn cluster_spread(c: &[f32]) -> f32 {
    if c.is_empty() {
        return f32::MAX;
    }
    let min = c.iter().cloned().fold(f32::INFINITY, f32::min);
    let max = c.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    max - min
}

// ── Core analysis (per window) ───────────────────────────────────────────────

/// Envelope → autocorrelation + IOI histogram BPM estimate for one mono window.
pub fn estimate_bpm(mono: &[f32], sample_rate: u32) -> Option<f32> {
    if mono.len() < sample_rate as usize / 2 || sample_rate < 8000 {
        return None;
    }

    let onset = compute_onset_envelope(mono)?;
    let hop_rate = sample_rate as f32 / HOP as f32;
    let min_lag = ((hop_rate * 60.0 / MAX_BPM).floor() as usize).max(2);
    let max_lag = ((hop_rate * 60.0 / MIN_BPM).ceil() as usize).min(onset.len() / 2);
    if max_lag <= min_lag + 2 {
        return None;
    }

    let (corr_scores, best_corr) = compute_autocorrelation(&onset, min_lag, max_lag)?;
    let hist_sm = compute_ioi_histogram(&onset, min_lag, max_lag);
    let hist_peak = hist_sm.iter().cloned().fold(0.0f32, f32::max).max(1e-9);

    let mut lag = score_candidates(
        hop_rate,
        min_lag,
        max_lag,
        &corr_scores,
        best_corr,
        &hist_sm,
        hist_peak,
    )?;
    lag = walk_octave_down(lag, hop_rate, max_lag, &corr_scores);
    refine_and_fold(lag, hop_rate, min_lag, max_lag, &corr_scores, &hist_sm, hist_peak, best_corr)
}

fn compute_energy_envelope(mono: &[f32]) -> Option<Vec<f32>> {
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
        None
    } else {
        Some(energy)
    }
}

fn compute_onset_envelope(mono: &[f32]) -> Option<Vec<f32>> {
    let energy = compute_energy_envelope(mono)?;
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
    Some(onset)
}

fn compute_autocorrelation(
    onset: &[f32],
    min_lag: usize,
    max_lag: usize,
) -> Option<(Vec<f32>, f32)> {
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
        None
    } else {
        Some((corr_scores, best_corr))
    }
}

fn compute_ioi_histogram(onset: &[f32], min_lag: usize, max_lag: usize) -> Vec<f32> {
    let mut peaks: Vec<usize> = Vec::new();
    for i in 2..onset.len().saturating_sub(2) {
        if onset[i] >= ONSET_PEAK_THRESH
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
    hist_sm
}

fn score_candidates(
    hop_rate: f32,
    min_lag: usize,
    max_lag: usize,
    corr_scores: &[f32],
    best_corr: f32,
    hist_sm: &[f32],
    hist_peak: f32,
) -> Option<usize> {
    let mut lag_peaks: Vec<usize> = Vec::new();
    for lag in (min_lag + 1)..max_lag {
        if corr_scores[lag] >= corr_scores[lag - 1]
            && corr_scores[lag] >= corr_scores[lag + 1]
            && corr_scores[lag] > best_corr * CORR_PEAK_REL
        {
            lag_peaks.push(lag);
        }
    }
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
        let lag_half = lag / 2;
        let half = if lag_half >= min_lag && lag % 2 == 0 {
            corr_scores[lag_half] / best_corr
        } else {
            0.0
        };

        let bpm = 60.0 * hop_rate / lag as f32;
        let prior = tempo_prior(bpm);
        let double_penalty = if half > corr_n * DOUBLE_TEMPO_HALF_RATIO {
            DOUBLE_TEMPO_PENALTY
        } else {
            1.0
        };
        let bar_bonus = 1.0 + BAR_BONUS_SCALE * two_beat;
        let score = (corr_n * W_CORR + hist_n * W_HIST + two_beat * W_TWO_BEAT)
            * prior
            * double_penalty
            * bar_bonus;

        if score > best_score {
            best_score = score;
            chosen_lag = lag;
        }
    }

    if best_score < WEAK_SCORE_MIN && best_corr < WEAK_CORR_MIN {
        None
    } else {
        Some(chosen_lag)
    }
}

fn walk_octave_down(
    mut lag: usize,
    hop_rate: f32,
    max_lag: usize,
    corr_scores: &[f32],
) -> usize {
    // Only step to a slower period when that tempo stays musically plausible.
    // Pure click trains have strong corr at 2×lag (→ half BPM); without a floor
    // this collapses 160 → 80.
    loop {
        let lag2 = lag * 2;
        if lag2 > max_lag {
            break;
        }
        let slower_bpm = 60.0 * hop_rate / lag2 as f32;
        if slower_bpm < SWEET_BPM_LO {
            break;
        }
        let c1 = corr_scores[lag];
        let c2 = corr_scores[lag2];
        if c2 >= c1 * OCTAVE_DOWN_RATIO {
            lag = lag2;
            continue;
        }
        break;
    }
    lag
}

fn refine_and_fold(
    lag_i: usize,
    hop_rate: f32,
    min_lag: usize,
    max_lag: usize,
    corr_scores: &[f32],
    hist_sm: &[f32],
    hist_peak: f32,
    best_corr: f32,
) -> Option<f32> {
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
    let refined_lag = (lag_i as f32 + frac).max(min_lag as f32);

    let mut bpm = fold_bpm_range(60.0 * hop_rate / refined_lag)?;
    bpm = snap_bpm(bpm);

    if !(SWEET_BPM_LO..=SWEET_BPM_HI).contains(&bpm) {
        if let Some(alt) = best_bpm_in_range(
            SWEET_BPM_LO,
            SWEET_BPM_HI,
            hop_rate,
            min_lag,
            max_lag,
            corr_scores,
            hist_sm,
            hist_peak,
            best_corr,
        ) {
            let lag_cur = bpm_to_lag(bpm, hop_rate, min_lag, max_lag);
            let lag_alt = bpm_to_lag(alt, hop_rate, min_lag, max_lag);
            let c_cur = corr_scores.get(lag_cur).copied().unwrap_or(0.0);
            let c_alt = corr_scores.get(lag_alt).copied().unwrap_or(0.0);
            if c_alt >= c_cur * SWEET_ALT_CORR_RATIO || c_alt >= best_corr * SWEET_ALT_BEST_RATIO {
                bpm = snap_bpm(alt);
            }
        }
    }

    if bpm > HIGH_BPM_CUTOFF {
        if let Some(half) = fold_bpm_range(bpm * 0.5) {
            // Avoid collapsing real 160–167 tracks to 80–83.
            if half >= HIGH_BPM_HALF_MIN {
                let lag_h = bpm_to_lag(half, hop_rate, min_lag, max_lag);
                let lag_b = bpm_to_lag(bpm, hop_rate, min_lag, max_lag);
                let c_h = corr_scores.get(lag_h).copied().unwrap_or(0.0);
                let c_b = corr_scores.get(lag_b).copied().unwrap_or(0.0);
                if c_h >= c_b * HIGH_BPM_HALF_RATIO {
                    bpm = snap_bpm(half);
                }
            }
        }
    }

    Some(bpm)
}

// ── Small helpers ────────────────────────────────────────────────────────────

fn parabolic_offset(ym1: f32, y0: f32, yp1: f32) -> f32 {
    let denom = 2.0 * (2.0 * y0 - yp1 - ym1);
    if denom.abs() < 1e-12 {
        return 0.0;
    }
    ((yp1 - ym1) / denom).clamp(-0.5, 0.5)
}

fn snap_bpm(bpm: f32) -> f32 {
    let nearest_int = bpm.round();
    if (bpm - nearest_int).abs() <= SNAP_INT_TOL {
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

fn bpm_to_lag(bpm: f32, hop_rate: f32, min_lag: usize, max_lag: usize) -> usize {
    let lag = (60.0 * hop_rate / bpm).round() as usize;
    lag.clamp(min_lag, max_lag)
}

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
    let lag_lo = bpm_to_lag(hi, hop_rate, min_lag, max_lag);
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

fn tempo_prior(bpm: f32) -> f32 {
    let z = (bpm - PRIOR_CENTER) / PRIOR_SIGMA;
    let g = (-0.5 * z * z).exp();
    PRIOR_FLOOR + PRIOR_SCALE * g
}

// ── Tests ────────────────────────────────────────────────────────────────────

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
        assert!((est - 124.0).abs() < 1.0, "expected ~124, got {est}");
    }

    #[test]
    fn prefers_160_over_80_for_fast_clicks() {
        // Strong 160 BPM click train — should not collapse to 80.
        let sr = 44100u32;
        let mono = synth_clicks(sr, 160.0, 25.0);
        let est = estimate_bpm(&mono, sr).expect("bpm");
        assert!(
            (est - 160.0).abs() < 3.0,
            "expected ~160 (not half), got {est}"
        );
    }

    #[test]
    fn silence_returns_none() {
        let sr = 44100u32;
        let mono = vec![0.0f32; sr as usize * 5];
        assert!(estimate_bpm(&mono, sr).is_none());
    }

    #[test]
    fn short_buffer_returns_none() {
        let sr = 44100u32;
        let mono = vec![0.1f32; 1000];
        assert!(estimate_bpm(&mono, sr).is_none());
    }

    #[test]
    fn low_sample_rate_returns_none() {
        let mono = synth_clicks(4000, 120.0, 10.0);
        assert!(estimate_bpm(&mono, 4000).is_none());
    }

    #[test]
    fn snap_prefers_integer() {
        assert_eq!(snap_bpm(123.8), 124.0);
        assert_eq!(snap_bpm(123.9), 124.0);
        assert_eq!(snap_bpm(122.2), 122.0);
        assert_eq!(snap_bpm(122.6), 122.5);
    }

    #[test]
    fn vote_picks_majority_cluster() {
        let v = vote_bpm(&[122.5, 124.0, 124.0, 179.0]).unwrap();
        assert!((v - 124.0).abs() < 1.0, "vote got {v}");
    }

    #[test]
    fn window_starts_short_file() {
        let w = window_starts_secs(20.0);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn window_starts_long_file() {
        let w = window_starts_secs(200.0);
        assert_eq!(w.len(), 3);
        assert!(w[0] < w[1] && w[1] < w[2]);
    }
}
