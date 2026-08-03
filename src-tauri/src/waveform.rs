//! Full-file waveform peaks for the Mix transition editor.
//!
//! A track is decoded through BASS exactly once and folded into a fine,
//! fixed-resolution peak array that is cached in memory (keyed by path +
//! mtime + size). Every subsequent request for that track — any zoom level,
//! any time range, any bin count — is served by slicing/pooling the cached
//! array, with zero further BASS calls.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use crate::bass::{self, BassLibrary};
use crate::player::Player;

const DEFAULT_BINS: usize = 1200;
const MIN_BINS: usize = 64;
const MAX_BINS: usize = 4096;

const CACHE_HOP_SECS: f64 = 1.0 / 220.0;
const CACHE_CAPACITY: usize = 24;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveformPeaks {
    pub peaks: Vec<f32>,
    pub range_start_secs: f64,
    pub range_end_secs: f64,
    pub duration_secs: f64,
    pub bins: usize,
}

#[derive(Clone, PartialEq, Eq)]
struct CacheKey {
    path: String,
    mtime_millis: u64,
    size: u64,
}

struct FineWaveform {
    hop_secs: f64,
    total_secs: f64,
    peaks: Vec<f32>,
}

#[derive(Clone)]
pub struct WaveformCache {
    inner: Arc<Mutex<CacheInner>>,
}

struct CacheInner {
    map: HashMap<String, (CacheKey, Arc<FineWaveform>)>,
    order: VecDeque<String>,
}

impl Default for WaveformCache {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheInner {
                map: HashMap::new(),
                order: VecDeque::new(),
            })),
        }
    }
}

impl WaveformCache {
    fn get(&self, key: &CacheKey) -> Option<Arc<FineWaveform>> {
        let inner = self.inner.lock().ok()?;
        let (existing_key, fine) = inner.map.get(&key.path)?;
        if existing_key == key {
            Some(fine.clone())
        } else {
            None
        }
    }

    fn put(&self, key: CacheKey, fine: Arc<FineWaveform>) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if !inner.map.contains_key(&key.path) {
            inner.order.push_back(key.path.clone());
        }
        inner.map.insert(key.path.clone(), (key, fine));
        while inner.order.len() > CACHE_CAPACITY {
            if let Some(oldest) = inner.order.pop_front() {
                inner.map.remove(&oldest);
            } else {
                break;
            }
        }
    }
}

fn cache_key_for(path: &str) -> Result<CacheKey, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Cannot stat {path}: {e}"))?;
    let mtime_millis = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(CacheKey {
        path: path.to_string(),
        mtime_millis,
        size: meta.len(),
    })
}

/// Generate waveform peaks for an on-disk audio file (or CUE segment).
pub fn peaks_for_path(
    player: &Player,
    cache: &WaveformCache,
    path: &str,
    bins: Option<usize>,
    start_secs: Option<f64>,
    end_secs: Option<f64>,
) -> Result<WaveformPeaks, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("Empty audio path".into());
    }
    if !Path::new(&path).is_file() {
        return Err(format!("File not found: {path}"));
    }

    let bins = bins.unwrap_or(DEFAULT_BINS).clamp(MIN_BINS, MAX_BINS);
    let key = cache_key_for(&path)?;

    let fine = if let Some(fine) = cache.get(&key) {
        fine
    } else {
        player.init()?;
        let path_for_bass = path.clone();
        let fine = player.with_bass(move |bass| decode_fine_waveform(bass, &path_for_bass))?;
        let fine = Arc::new(fine);
        cache.put(key, fine.clone());
        fine
    };

    if fine.total_secs <= 0.05 {
        return Err("Track is too short for a waveform".into());
    }

    let start = start_secs
        .filter(|s| s.is_finite() && *s >= 0.0)
        .unwrap_or(0.0)
        .clamp(0.0, fine.total_secs);
    let end = end_secs
        .filter(|s| s.is_finite() && *s > start)
        .unwrap_or(fine.total_secs)
        .clamp(start + 0.05, fine.total_secs);
    let duration = (end - start).max(0.05);

    let mut peaks = pool_peaks(&fine, start, end, bins);

    let max = peaks.iter().cloned().fold(0.0f32, f32::max);
    if max > 1e-6 {
        let inv = 1.0 / max;
        for p in &mut peaks {
            *p = (*p * inv).clamp(0.0, 1.0);
        }
    }
    fill_gaps(&mut peaks);

    Ok(WaveformPeaks {
        peaks,
        range_start_secs: start,
        range_end_secs: end,
        duration_secs: duration,
        bins,
    })
}

/// Decode the whole file once into a fine, fixed-hop peak array.
fn decode_fine_waveform(bass: &BassLibrary, path: &str) -> Result<FineWaveform, String> {
    let (handle, _mode) = open_decode(bass, path)?;

    let result = (|| {
        let info = decode_info(bass, handle)?;
        if info.total_secs <= 0.05 {
            return Ok(FineWaveform {
                hop_secs: CACHE_HOP_SECS,
                total_secs: info.total_secs,
                peaks: Vec::new(),
            });
        }

        let hop_secs = CACHE_HOP_SECS;
        let fine_bins = ((info.total_secs / hop_secs).ceil() as usize).max(1);
        let mut fine = vec![0.0f32; fine_bins];

        let mut float_chunk = vec![0f32; 8192 * info.chans];
        let mut i16_chunk = vec![0i16; 8192 * info.chans];

        let max_pulls = ((info.total_secs * info.sample_rate as f64) as usize / 4096)
            .saturating_add(128)
            .max(128);

        for _ in 0..max_pulls {
            let pos_bytes = bass.channel_get_position(handle, bass::BASS_POS_BYTE);
            let pos_secs = bass.channel_bytes2seconds(handle, pos_bytes);
            if pos_secs >= info.total_secs - 1e-4 {
                break;
            }

            let n = if info.is_float {
                bass.channel_get_data_f32(handle, &mut float_chunk)?
            } else {
                bass.channel_get_data_i16(handle, &mut i16_chunk)?
            };
            if n == 0 {
                break;
            }

            let frames = n / info.chans;
            if frames == 0 {
                break;
            }

            let end_pos_bytes = bass.channel_get_position(handle, bass::BASS_POS_BYTE);
            let end_pos_secs = bass.channel_bytes2seconds(handle, end_pos_bytes);
            let chunk_dur =
                (end_pos_secs - pos_secs).max(1.0 / info.sample_rate as f64 * frames as f64);
            let frame_step = chunk_dur / frames as f64;

            for i in 0..frames {
                let t = pos_secs + i as f64 * frame_step;
                if t < 0.0 || t > info.total_secs {
                    continue;
                }
                let amp = if info.is_float {
                    frame_amp_f32(&float_chunk[i * info.chans..(i + 1) * info.chans])
                } else {
                    frame_amp_i16(&i16_chunk[i * info.chans..(i + 1) * info.chans])
                };
                let idx = ((t / hop_secs) as usize).min(fine_bins - 1);
                if amp > fine[idx] {
                    fine[idx] = amp;
                }
            }

            if end_pos_secs >= info.total_secs {
                break;
            }
        }

        Ok(FineWaveform {
            hop_secs,
            total_secs: info.total_secs,
            peaks: fine,
        })
    })();

    if let Err(e) = bass.channel_free(handle) {
        eprintln!("[waveform] channel_free failed for {path}: {e}");
    }
    result
}

/// Pool (or interpolate) the cached fine peak array down/up into `bins` covering [start, end].
fn pool_peaks(fine: &FineWaveform, start: f64, end: f64, bins: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; bins];
    if fine.peaks.is_empty() {
        return out;
    }
    let last_idx = fine.peaks.len() - 1;
    let duration = (end - start).max(1e-6);

    for b in 0..bins {
        let t0 = start + (b as f64 / bins as f64) * duration;
        let t1 = start + ((b + 1) as f64 / bins as f64) * duration;
        let idx0 = ((t0 / fine.hop_secs).floor() as isize).clamp(0, last_idx as isize) as usize;
        let idx1 = ((t1 / fine.hop_secs).ceil() as isize).clamp(0, last_idx as isize) as usize;

        if idx1 > idx0 + 1 {
            let mut peak = 0.0f32;
            for v in &fine.peaks[idx0..=idx1] {
                if *v > peak {
                    peak = *v;
                }
            }
            out[b] = peak;
        } else {
            let mid = (t0 + t1) * 0.5;
            out[b] = sample_fine_interp(fine, mid);
        }
    }
    out
}

fn sample_fine_interp(fine: &FineWaveform, t: f64) -> f32 {
    let last_idx = fine.peaks.len() - 1;
    let pos = (t / fine.hop_secs).clamp(0.0, last_idx as f64);
    let i0 = pos.floor() as usize;
    let i1 = (i0 + 1).min(last_idx);
    let frac = (pos - i0 as f64) as f32;
    fine.peaks[i0] * (1.0 - frac) + fine.peaks[i1] * frac
}

fn frame_amp_f32(frame: &[f32]) -> f32 {
    let mut acc = 0.0f32;
    for &s in frame {
        acc += s.abs();
    }
    acc / frame.len() as f32
}

fn frame_amp_i16(frame: &[i16]) -> f32 {
    let mut acc = 0.0f32;
    for &s in frame {
        acc += (s as f32).abs();
    }
    (acc / frame.len() as f32) / 32768.0
}

fn fill_gaps(peaks: &mut [f32]) {
    if peaks.is_empty() {
        return;
    }
    let mut last = 0.0f32;
    for p in peaks.iter_mut() {
        if *p > 0.0 {
            last = *p;
        } else if last > 0.0 {
            *p = last * 0.15;
        }
    }
    let mut last = 0.0f32;
    for p in peaks.iter_mut().rev() {
        if *p > 0.0 {
            last = *p;
        } else if last > 0.0 {
            *p = last * 0.15;
        }
    }
}

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
            Ok(h) if h != 0 => return Ok((h, label)),
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