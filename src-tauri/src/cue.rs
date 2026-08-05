// CUE sheet parser — expands album image files into virtual tracks.

use cue_rw::{CUEFile, CUETrack, CUETimeStamp};
use num_rational::Rational32;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::library::MusicFile;
use crate::metadata;

// ── Short-lived path caches (mtime-invalidated) ───────────────────────────────
// repair_track / resolve_playback hit the same album CUE once per track; cache
// companion lookup + full expand so a 15-track multi-file album pays once.

struct CompanionCacheEntry {
    parent_mtime: Option<SystemTime>,
    cue: Option<PathBuf>,
}

struct ExpandCacheEntry {
    cue_mtime: Option<SystemTime>,
    resolve_covers: bool,
    tracks: Vec<MusicFile>,
}

fn companion_cache() -> &'static Mutex<HashMap<String, CompanionCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CompanionCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn expand_cache() -> &'static Mutex<HashMap<String, ExpandCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, ExpandCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn path_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn path_cache_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Files only, sorted — one `read_dir` reused by resolve / companion lookups.
fn list_files_in_dir(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
            if is_file {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    files
}

fn canonicalize_or(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

pub const CUE_PATH_MARKER: &str = "#cue:";

pub fn is_cue_track_path(path: &str) -> bool {
    path.contains(CUE_PATH_MARKER)
}

pub fn is_cue_sheet_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackTarget {
    pub audio_path: String,
    pub cue_start: Option<f64>,
    pub cue_end: Option<f64>,
}

pub fn parse_virtual_cue_path(path: &str) -> Option<(String, u32)> {
    let marker_pos = path.rfind(CUE_PATH_MARKER)?;
    let audio = path[..marker_pos].to_string();
    let track_no = path[marker_pos + CUE_PATH_MARKER.len()..].parse().ok()?;
    if audio.is_empty() || track_no == 0 {
        return None;
    }
    Some((audio, track_no))
}

/// Find a CUE sheet that sits next to an audio image.
///
/// Supports both common layouts:
/// - `album.cue` beside `album.flac`
/// - `album.flac.cue` beside `album.flac` (Exact Audio Copy and similar)
/// - multi-file rips: any `*.cue` in the same folder that references this audio
pub fn companion_cue_for_audio(audio_path: &Path) -> Option<PathBuf> {
    let parent = audio_path.parent()?;
    let key = path_cache_key(audio_path);
    let parent_mtime = path_mtime(parent);

    {
        let cache = companion_cache().lock();
        if let Some(entry) = cache.get(&key) {
            if entry.parent_mtime == parent_mtime {
                return entry.cue.clone();
            }
        }
    }

    let cue = companion_cue_for_audio_uncached(audio_path, parent);
    companion_cache().lock().insert(
        key,
        CompanionCacheEntry {
            parent_mtime,
            cue: cue.clone(),
        },
    );
    cue
}

fn companion_cue_for_audio_uncached(audio_path: &Path, parent: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::with_capacity(2);
    if let Some(stem) = audio_path.file_stem() {
        candidates.push(audio_path.with_file_name(format!("{}.cue", stem.to_string_lossy())));
    }
    if let Some(name) = audio_path.file_name() {
        candidates.push(audio_path.with_file_name(format!("{}.cue", name.to_string_lossy())));
    }

    for cue_path in &candidates {
        if cue_path.is_file() {
            return Some(cue_path.clone());
        }
    }

    // One directory listing for case-insensitive name match + multi-file FILE scan.
    let listing = list_files_in_dir(parent);

    // Case-insensitive match on Windows (FAT/NTFS folder listings can differ in case).
    #[cfg(windows)]
    {
        let targets: Vec<String> = candidates
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
            .collect();
        if !targets.is_empty() {
            for path in &listing {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if targets.iter().any(|t| t == &name) {
                    return Some(path.clone());
                }
            }
        }
    }

    // Multi-file album: `Не пара.cue` next to `01. ….m4a` — match by FILE list inside the sheet.
    let audio_name = audio_path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())?;
    let audio_stem = audio_path
        .file_stem()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let mut cue_paths: Vec<PathBuf> = listing
        .iter()
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
        })
        .cloned()
        .collect();
    cue_paths.sort();

    for cue_path in cue_paths {
        if let Some((cue, _)) = parse_cue_file(&cue_path) {
            for file_name in &cue.files {
                let fname = file_name.to_lowercase();
                let fstem = Path::new(file_name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if fname == audio_name
                    || fstem == audio_stem
                    || fname.starts_with(&audio_stem)
                    || audio_name.starts_with(&fstem)
                {
                    return Some(cue_path);
                }
                // Resolved path match (ALAC → m4a) — reuse folder listing.
                if let Some(resolved) =
                    resolve_audio_file_with_listing(parent, file_name, &listing)
                {
                    if same_path_key(&resolved, audio_path) {
                        return Some(cue_path);
                    }
                }
            }
        }
    }

    None
}

fn same_path_key(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn expanded_track_for_audio(audio_path: &str, track_no: u32) -> Option<MusicFile> {
    let cue_path = companion_cue_for_audio(Path::new(audio_path))?;
    expand_cue_file_cached(&cue_path, true)
        .into_iter()
        .nth(track_no.saturating_sub(1) as usize)
}

/// Expand with mtime cache — shared by repair_track / resolve_playback per album CUE.
fn expand_cue_file_cached(cue_path: &Path, resolve_covers: bool) -> Vec<MusicFile> {
    let key = path_cache_key(cue_path);
    let cue_mtime = path_mtime(cue_path);

    {
        let cache = expand_cache().lock();
        if let Some(entry) = cache.get(&key) {
            if entry.cue_mtime == cue_mtime && entry.resolve_covers == resolve_covers {
                return entry.tracks.clone();
            }
        }
    }

    let tracks = expand_cue_file_impl(cue_path, resolve_covers);
    expand_cache().lock().insert(
        key,
        ExpandCacheEntry {
            cue_mtime,
            resolve_covers,
            tracks: tracks.clone(),
        },
    );
    tracks
}

/// Fill missing CUE metadata on a playlist track loaded from disk.
/// Always refreshes INDEX bounds / duration from the companion sheet when possible
/// so multi-file lengths match the CUE (not the full container file).
pub fn repair_track(track: &mut MusicFile) -> bool {
    if is_cue_track_path(&track.path) {
        if let Some((audio, track_no)) = parse_virtual_cue_path(&track.path) {
            if let Some(expanded) = expanded_track_for_audio(&audio, track_no) {
                let before = (
                    track.path.clone(),
                    track.cue_start_secs,
                    track.cue_end_secs,
                    track.duration_secs,
                );
                // Prefer full expanded entry (path + INDEX bounds + duration).
                *track = expanded;
                return before
                    != (
                        track.path.clone(),
                        track.cue_start_secs,
                        track.cue_end_secs,
                        track.duration_secs,
                    );
            } else if track.audio_path.is_none() {
                track.audio_path = Some(audio);
                return true;
            }
        }
        return false;
    }

    if is_cue_sheet_path(&track.path) {
        // Caller should expand full sheet; single-track fallback only.
        if let Some(expanded) = expand_cue_file(Path::new(&track.path)).into_iter().next() {
            *track = expanded;
            return true;
        }
    }
    false
}

/// Resolve the real audio file and optional CUE segment for playback.
pub fn resolve_playback(
    track_path: &str,
    audio_path: Option<&str>,
    cue_start: Option<f64>,
    cue_end: Option<f64>,
) -> Result<PlaybackTarget, String> {
    if let Some((audio, track_no)) = parse_virtual_cue_path(track_path) {
        if !Path::new(&audio).is_file() {
            return Err(format!("Audio file not found for CUE track: {audio}"));
        }

        let resolved_from_args = audio_path
            .filter(|value| !value.is_empty() && Path::new(value).is_file())
            .map(str::to_string);

        if let Some(expanded) = expanded_track_for_audio(&audio, track_no) {
            let resolved_audio = resolved_from_args
                .or(expanded.audio_path.clone())
                .unwrap_or(audio);

            // Explicit caller bounds win (mix preview / seek into a segment).
            // Fall back to INDEX times from the sheet when the caller did not override.
            return Ok(PlaybackTarget {
                audio_path: resolved_audio,
                cue_start: cue_start.or(expanded.cue_start_secs),
                cue_end: cue_end.or(expanded.cue_end_secs),
            });
        }

        // Companion .cue missing/unreadable, but playlist already has segment bounds.
        if cue_start.is_some() {
            return Ok(PlaybackTarget {
                audio_path: resolved_from_args.unwrap_or(audio),
                cue_start,
                cue_end,
            });
        }

        return Err(format!(
            "Failed to resolve CUE track #{track_no} for {audio} (no companion .cue / .flac.cue and no cue times)"
        ));
    }

    if let Some(audio) = audio_path.filter(|value| !value.is_empty()) {
        if Path::new(audio).is_file() {
            return Ok(PlaybackTarget {
                audio_path: audio.to_string(),
                cue_start,
                cue_end,
            });
        }
    }

    if is_cue_sheet_path(track_path) {
        let expanded = expand_cue_file(Path::new(track_path))
            .into_iter()
            .next()
            .ok_or_else(|| "CUE sheet does not contain playable tracks".to_string())?;

        return Ok(PlaybackTarget {
            audio_path: expanded.audio_path.ok_or_else(|| {
                "CUE sheet is missing a valid audio file reference".to_string()
            })?,
            cue_start: expanded.cue_start_secs,
            cue_end: expanded.cue_end_secs,
        });
    }

    if Path::new(track_path).is_file() {
        return Ok(PlaybackTarget {
            audio_path: track_path.to_string(),
            // Honour optional segment bounds (mix preview / partial play).
            cue_start,
            cue_end,
        });
    }

    Err(format!("Can't open audio file: {track_path}"))
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn timestamp_secs(ts: CUETimeStamp) -> f64 {
    let rational: Rational32 = ts.into();
    *rational.numer() as f64 / *rational.denom() as f64
}

fn track_index_start(track: &CUETrack) -> Option<f64> {
    track
        .indices
        .iter()
        .find(|(idx, _)| *idx == 1)
        .map(|(_, ts)| timestamp_secs(*ts))
}

/// Read a CUE sheet as text. EAC/Foobar sheets are often Windows-1251 (Cyrillic),
/// not UTF-8 — plain `read_to_string` rejects those and the import silently drops.
fn read_cue_text(cue_path: &Path) -> Option<String> {
    let bytes = fs::read(cue_path).ok()?;
    if bytes.is_empty() {
        return None;
    }

    // UTF-8 (with optional BOM)
    if let Ok(text) = std::str::from_utf8(&bytes) {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        return Some(text.to_string());
    }

    // Windows-1251 — very common for Russian EAC rips
    {
        let (cow, _enc, had_errors) = encoding_rs::WINDOWS_1251.decode(&bytes);
        if !had_errors || cow.contains("FILE") || cow.contains("TRACK") {
            return Some(cow.into_owned());
        }
    }

    // Windows-1252 / Latin-1 fallback (Western EAC)
    {
        let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
        if cow.contains("FILE") || cow.contains("TRACK") {
            return Some(cow.into_owned());
        }
    }

    // Last resort: lossy UTF-8 so we at least try to parse
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Exact Audio Copy multi-file CUE layout puts the next track's INDEX 01 *after*
/// the next FILE line, outside any TRACK block. cue-rw rejects that as InvalidTag.
/// Rewrite to a conventional shape: each FILE owns its TRACK with INDEX 01.
///
/// Before:
/// ```text
/// FILE "01.flac" WAVE
///   TRACK 01 AUDIO
///     INDEX 01 00:00:00
///   TRACK 02 AUDIO
///     INDEX 00 03:00:00
/// FILE "02.flac" WAVE
///     INDEX 01 00:00:00
/// ```
///
/// After:
/// ```text
/// FILE "01.flac" WAVE
///   TRACK 01 AUDIO
///     INDEX 01 00:00:00
/// FILE "02.flac" WAVE
///   TRACK 02 AUDIO
///     INDEX 01 00:00:00
/// ```
fn normalize_eac_multifile_cue(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return content.to_string();
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    // Track header lines (TRACK + body) waiting for their FILE + INDEX 01.
    let mut pending_track: Option<Vec<String>> = None;
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.starts_with("FILE ") {
            out.push(line.to_string());
            i += 1;

            // Orphan INDEX/FLAGS lines that belong to the pending TRACK.
            let mut orphans: Vec<String> = Vec::new();
            while i < lines.len() {
                let next = lines[i];
                let next_trim = next.trim();
                // Still indented body, but not a new TRACK / top-level tag.
                if next.starts_with("  ")
                    && !next_trim.starts_with("TRACK ")
                    && !next.starts_with("FILE ")
                    && !next_trim.starts_with("REM ")
                    && !next_trim.starts_with("TITLE ")
                    && !next_trim.starts_with("PERFORMER ")
                    && !next_trim.starts_with("CATALOG ")
                {
                    orphans.push(next.to_string());
                    i += 1;
                } else {
                    break;
                }
            }

            if let Some(mut track_lines) = pending_track.take() {
                // Drop INDEX 00 (pregap of previous file) — start of this file is INDEX 01.
                track_lines.retain(|l| {
                    let t = l.trim();
                    !t.starts_with("INDEX 00") && !t.starts_with("INDEX 0 ")
                });
                // Prefer INDEX 01 from the orphan block after FILE.
                let has_idx1 = orphans
                    .iter()
                    .any(|l| l.trim().starts_with("INDEX 01") || l.trim().starts_with("INDEX 1 "));
                if has_idx1 {
                    track_lines.retain(|l| {
                        let t = l.trim();
                        !t.starts_with("INDEX 01") && !t.starts_with("INDEX 1 ")
                    });
                    for o in &orphans {
                        let t = o.trim();
                        if t.starts_with("INDEX 01") || t.starts_with("INDEX 1 ") {
                            // Normalize indent under TRACK.
                            track_lines.push(format!("    {t}"));
                        }
                    }
                } else if !track_lines
                    .iter()
                    .any(|l| l.trim().starts_with("INDEX 01") || l.trim().starts_with("INDEX 1 "))
                {
                    // No index at all — assume start of file.
                    track_lines.push("    INDEX 01 00:00:00".to_string());
                }
                out.extend(track_lines);
            } else {
                // No pending track — keep orphans as-is (unusual but non-destructive).
                out.extend(orphans);
            }
            continue;
        }

        if trimmed.starts_with("TRACK ") {
            // Flush previous pending without a dedicated FILE (single-image cues).
            if let Some(track_lines) = pending_track.take() {
                out.extend(track_lines);
            }

            let mut track_lines = vec![line.to_string()];
            i += 1;
            while i < lines.len() {
                let next = lines[i];
                let next_trim = next.trim();
                if next.starts_with("FILE ")
                    || next_trim.starts_with("TRACK ")
                    || (!next.starts_with(' ') && !next.starts_with('\t') && !next_trim.is_empty())
                {
                    break;
                }
                // Indented track body.
                if next.starts_with("  ") || next.starts_with('\t') {
                    track_lines.push(next.to_string());
                    i += 1;
                } else if next_trim.is_empty() {
                    i += 1;
                } else {
                    break;
                }
            }

            let has_idx1 = track_lines.iter().any(|l| {
                let t = l.trim();
                t.starts_with("INDEX 01") || t.starts_with("INDEX 1 ")
            });
            // Only INDEX 00 → wait for next FILE's INDEX 01 (EAC multi-file).
            if !has_idx1 {
                pending_track = Some(track_lines);
            } else {
                out.extend(track_lines);
            }
            continue;
        }

        // Top-level metadata / REM / blank.
        out.push(line.to_string());
        i += 1;
    }

    if let Some(track_lines) = pending_track {
        out.extend(track_lines);
    }

    let mut result = out.join("\n");
    if content.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn parse_cue_file(cue_path: &Path) -> Option<(CUEFile, PathBuf)> {
    let raw = read_cue_text(cue_path)?;
    parse_cue_content(&raw, cue_path)
}

fn parse_cue_content(raw: &str, cue_path: &Path) -> Option<(CUEFile, PathBuf)> {
    let content = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let normalized = normalize_eac_multifile_cue(content);
    let cue: CUEFile = normalized.as_str().try_into().ok()?;
    let cue_dir = cue_path.parent()?.to_path_buf();
    Some((cue, cue_dir))
}

/// Audio extensions we may substitute when CUE says `.ALAC` / `.WAV` but disk has `.m4a` etc.
const AUDIO_EXT_FALLBACKS: &[&str] = &[
    "m4a", "mp4", "alac", "flac", "wav", "aiff", "aif", "ape", "wv", "opus", "ogg", "mp3", "wma",
];

fn resolve_audio_file_with_listing(
    cue_dir: &Path,
    file_name: &str,
    listing: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(path) = resolve_audio_file_direct(cue_dir, file_name) {
        return Some(path);
    }
    resolve_audio_file_from_listing(file_name, listing)
}

/// Exact path + extension substitution (no `read_dir`).
fn resolve_audio_file_direct(cue_dir: &Path, file_name: &str) -> Option<PathBuf> {
    let candidate = cue_dir.join(file_name);
    if candidate.is_file() {
        return Some(canonicalize_or(candidate));
    }

    // Same stem, different extension (EAC often writes FILE "...ALAC" ALAC while
    // the rip is actually ".m4a" / ".flac").
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.to_string());

    for ext in AUDIO_EXT_FALLBACKS {
        let alt = cue_dir.join(format!("{stem}.{ext}"));
        if alt.is_file() {
            return Some(canonicalize_or(alt));
        }
    }

    None
}

/// Case-insensitive / prefix heuristics against a pre-read folder listing.
fn resolve_audio_file_from_listing(file_name: &str, listing: &[PathBuf]) -> Option<PathBuf> {
    let target = file_name.to_lowercase();
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.to_string());
    let stem_lower = stem.to_lowercase();

    // Case-insensitive exact name.
    for path in listing {
        if path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .as_deref()
            == Some(target.as_str())
        {
            return Some(canonicalize_or(path.clone()));
        }
    }

    // Case-insensitive stem match.
    for path in listing {
        let Some(entry_stem) = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()) else {
            continue;
        };
        if entry_stem != stem_lower {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if AUDIO_EXT_FALLBACKS.iter().any(|a| *a == ext) || !ext.is_empty() {
            return Some(canonicalize_or(path.clone()));
        }
    }

    // Leading track-number prefix: "01. Title.ALAC" → any "01.*" audio in the folder.
    if let Some((num, _)) = stem.split_once(|c: char| !c.is_ascii_digit()) {
        if !num.is_empty() {
            let prefix = format!("{num}.");
            let prefix_lower = prefix.to_lowercase();
            let mut matches: Vec<PathBuf> = listing
                .iter()
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().to_lowercase().starts_with(&prefix_lower))
                        .unwrap_or(false)
                })
                .filter(|p| {
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    AUDIO_EXT_FALLBACKS.iter().any(|a| *a == ext)
                })
                .cloned()
                .collect();
            matches.sort();
            if let Some(path) = matches.into_iter().next() {
                return Some(canonicalize_or(path));
            }
        }
    }

    None
}

fn end_secs_for_track(
    tracks: &[(usize, &CUETrack)],
    index: usize,
    file_id: usize,
    audio_duration: Option<f64>,
) -> Option<f64> {
    if let Some((_, next_track)) = tracks.get(index + 1) {
        if tracks[index + 1].0 == file_id {
            return track_index_start(next_track);
        }
    }

    audio_duration
}

/// Parse `INDEX 00 mm:ss:ff` / `INDEX 01 mm:ss:ff` → seconds.
fn parse_index_line_secs(line: &str) -> Option<(u8, f64)> {
    let t = line.trim();
    if !t.starts_with("INDEX ") {
        return None;
    }
    let mut parts = t.split_whitespace();
    let _ = parts.next()?; // INDEX
    let idx: u8 = parts.next()?.parse().ok()?;
    let ts = parts.next()?;
    let mut hms = ts.split(':');
    let mm: f64 = hms.next()?.parse().ok()?;
    let ss: f64 = hms.next()?.parse().ok()?;
    let ff: f64 = hms.next()?.parse().ok()?; // frames @ 75/s
    Some((idx, mm * 60.0 + ss + ff / 75.0))
}

/// EAC multi-file sheets put `INDEX 00` of track N under FILE N-1 — that time is the
/// true CD length of track N-1 (often a few seconds shorter than the m4a/ALAC file).
/// Returns one optional end (seconds from file start) per TRACK in sheet order.
fn extract_multifile_index00_ends(content: &str) -> Vec<Option<f64>> {
    let mut ends: Vec<Option<f64>> = Vec::new();
    let mut track_count = 0usize;

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("TRACK ") {
            track_count += 1;
            ends.push(None);
            continue;
        }
        if let Some((0, secs)) = parse_index_line_secs(t) {
            // INDEX 00 on TRACK k ends TRACK k-1 (1-based): ends[k-2]
            if track_count >= 2 {
                let prev = track_count - 2;
                if prev < ends.len() && ends[prev].is_none() {
                    ends[prev] = Some(secs);
                }
            }
        }
    }
    ends
}

/// Expand a .cue file into virtual `MusicFile` entries (one per TRACK).
///
/// - **Single image** (many TRACK share one FILE): `audio#cue:N` + INDEX start/end
/// - **Multi-file** (one FILE per TRACK, e.g. Не пара): plain audio path, but
///   `cue_start`/`cue_end` from INDEX so duration/gapless match the sheet (INDEX 00),
///   not the full container length of the m4a/ALAC file.
pub fn expand_cue_file(cue_path: &Path) -> Vec<MusicFile> {
    expand_cue_file_cached(cue_path, true)
}

/// Import variant: preserve all CUE identity/timing while deferring expensive
/// embedded-cover extraction until a track becomes visible.
pub fn expand_cue_file_fast(cue_path: &Path) -> Vec<MusicFile> {
    expand_cue_file_cached(cue_path, false)
}

fn expand_cue_file_impl(cue_path: &Path, resolve_covers: bool) -> Vec<MusicFile> {
    let raw = match read_cue_text(cue_path) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let content = raw.strip_prefix('\u{feff}').unwrap_or(raw.as_str());
    let index00_ends = extract_multifile_index00_ends(content);

    // Parse from already-loaded text — avoid a second disk read + normalize pass.
    let (cue, cue_dir) = match parse_cue_content(content, cue_path) {
        Some(value) => value,
        None => return Vec::new(),
    };

    if cue.files.is_empty() || cue.tracks.is_empty() {
        return Vec::new();
    }

    let album = non_empty(&cue.title);
    let album_artist = non_empty(&cue.performer);
    let track_refs: Vec<(usize, &CUETrack)> = cue
        .tracks
        .iter()
        .map(|(file_id, track)| ((*file_id), track))
        .collect();

    // How many TRACK rows reference each FILE id.
    let mut tracks_per_file: Vec<usize> = vec![0; cue.files.len()];
    for (file_id, _) in &track_refs {
        if *file_id < tracks_per_file.len() {
            tracks_per_file[*file_id] += 1;
        }
    }

    // One folder listing for all track FILE resolutions in this sheet.
    let listing = list_files_in_dir(&cue_dir);

    let mut result = Vec::with_capacity(track_refs.len());
    let mut metadata_by_audio = HashMap::<String, metadata::TrackMetadata>::new();
    let mut size_by_audio = HashMap::<String, u64>::new();

    for (index, (file_id, track)) in track_refs.iter().enumerate() {
        let file_name = match cue.files.get(*file_id) {
            Some(name) => name,
            None => continue,
        };

        let audio_path =
            match resolve_audio_file_with_listing(&cue_dir, file_name, &listing) {
                Some(path) => path,
                None => continue,
            };

        let start = match track_index_start(track) {
            Some(value) => value,
            None => continue,
        };

        let title = non_empty(&track.title).or_else(|| album.clone());
        let artist = track
            .performer
            .as_ref()
            .and_then(|value| non_empty(value))
            .or_else(|| album_artist.clone());

        let audio_path_str = audio_path.to_string_lossy().to_string();
        let audio_meta = metadata_by_audio
            .entry(audio_path_str.clone())
            .or_insert_with(|| {
                if resolve_covers {
                    metadata::read_metadata(&audio_path, file_name)
                } else {
                    metadata::read_metadata_fast(&audio_path, file_name)
                }
            })
            .clone();
        let size = *size_by_audio
            .entry(audio_path_str.clone())
            .or_insert_with(|| fs::metadata(&audio_path).map(|meta| meta.len()).unwrap_or(0));
        let ext = audio_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_lowercase();

        let display_name = title
            .clone()
            .unwrap_or_else(|| format!("Track {}", index + 1));

        // One TRACK per FILE starting at 0 → multi-file rip (separate audio per song).
        let single_file_track = tracks_per_file.get(*file_id).copied().unwrap_or(0) == 1
            && start <= 0.05;

        let (path, audio_path_field, cue_start_secs, cue_end_secs, duration_secs) =
            if single_file_track {
                // Prefer INDEX 00 length from the sheet (gapless cut), fall back to file tags.
                let cue_end = index00_ends
                    .get(index)
                    .copied()
                    .flatten()
                    .filter(|e| *e > start + 0.05)
                    .or_else(|| {
                        end_secs_for_track(
                            &track_refs,
                            index,
                            *file_id,
                            audio_meta.duration_secs,
                        )
                    })
                    .or(audio_meta.duration_secs);
                let dur = cue_end.map(|e| (e - start).max(0.0));
                (
                    // Keep a stable virtual path so playlist repair can re-bind to the sheet.
                    format!("{}{}{}", audio_path_str, CUE_PATH_MARKER, index + 1),
                    Some(audio_path_str),
                    Some(start),
                    cue_end,
                    dur,
                )
            } else {
                let end_secs = end_secs_for_track(
                    &track_refs,
                    index,
                    *file_id,
                    audio_meta.duration_secs,
                );
                let duration_secs = end_secs.map(|end| (end - start).max(0.0));
                (
                    format!("{}{}{}", audio_path_str, CUE_PATH_MARKER, index + 1),
                    Some(audio_path_str),
                    Some(start),
                    end_secs,
                    duration_secs,
                )
            };

        result.push(MusicFile {
            path,
            file_name: display_name,
            extension: ext,
            size,
            title,
            artist,
            album: album.clone(),
            duration_secs,
            year: None,
            track_number: Some((index + 1) as u32),
            genre: None,
            cover_path: audio_meta.cover_path,
            cover_path_full: audio_meta.cover_path_full,
            audio_path: audio_path_field,
            cue_start_secs,
            cue_end_secs,
        });
    }

    result
}

/// Audio files referenced by parsed CUE sheets (canonical paths).
pub fn covered_audio_paths(cue_paths: &[PathBuf]) -> Vec<String> {
    let mut covered = Vec::new();

    for cue_path in cue_paths {
        let (cue, cue_dir) = match parse_cue_file(cue_path) {
            Some(value) => value,
            None => continue,
        };

        let listing = list_files_in_dir(&cue_dir);
        for file_name in &cue.files {
            if let Some(audio) =
                resolve_audio_file_with_listing(&cue_dir, file_name, &listing)
            {
                covered.push(audio.to_string_lossy().to_string());
            }
        }
    }

    covered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_text(path: &Path, content: &str) {
        let mut file = fs::File::create(path).expect("create file");
        file.write_all(content.as_bytes()).expect("write file");
    }

    fn write_bytes(path: &Path, bytes: &[u8]) {
        let mut file = fs::File::create(path).expect("create file");
        file.write_all(bytes).expect("write bytes");
    }

    #[test]
    fn normalize_eac_multifile_moves_index01_under_track() {
        let raw = r#"PERFORMER "Artist"
TITLE "Album"
FILE "01. One.ALAC" ALAC
  TRACK 01 AUDIO
    TITLE "One"
    PERFORMER "Artist"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Two"
    PERFORMER "Artist"
    INDEX 00 03:00:00
FILE "02. Two.ALAC" ALAC
    INDEX 01 00:00:00
  TRACK 03 AUDIO
    TITLE "Three"
    PERFORMER "Artist"
    INDEX 00 02:30:00
FILE "03. Three.ALAC" ALAC
    INDEX 01 00:00:00
"#;
        let normalized = normalize_eac_multifile_cue(raw);
        let cue: CUEFile = normalized.as_str().try_into().expect("parse normalized cue");
        assert_eq!(cue.files.len(), 3);
        assert_eq!(cue.tracks.len(), 3);
        assert_eq!(cue.tracks[0].0, 0);
        assert_eq!(cue.tracks[1].0, 1);
        assert_eq!(cue.tracks[2].0, 2);
        assert!(track_index_start(&cue.tracks[1].1).is_some());
    }

    #[test]
    fn expand_real_ne_para_cue_when_present() {
        let cue = PathBuf::from(r"Z:\torrent\Потап и Настя\2008 — Не пара\Не пара.cue");
        if !cue.is_file() {
            return;
        }
        let tracks = expand_cue_file(&cue);
        assert!(
            tracks.len() >= 10,
            "expected many tracks from Не пара.cue, got {} — {:?}",
            tracks.len(),
            tracks.iter().map(|t| t.title.clone()).collect::<Vec<_>>()
        );
        // Multi-file: virtual paths + INDEX 00 ends (shorter than full m4a when sheet says so).
        let t0 = &tracks[0];
        assert!(t0.path.contains(CUE_PATH_MARKER));
        assert_eq!(t0.cue_start_secs, Some(0.0));
        // INDEX 00 01:12:42 ≈ 72.56s — not full file ~74.56s
        let end0 = t0.cue_end_secs.expect("track 1 needs INDEX 00 end");
        assert!(
            (end0 - (1.0 * 60.0 + 12.0 + 42.0 / 75.0)).abs() < 0.05,
            "track 1 end should be INDEX 00 01:12:42, got {end0}"
        );
        for t in &tracks {
            assert!(
                t.audio_path
                    .as_ref()
                    .is_some_and(|p| Path::new(p).is_file()),
                "missing audio for {:?}: {:?}",
                t.title,
                t.audio_path
            );
            eprintln!(
                "  #{} {:?} start={:?} end={:?} dur={:?} audio={}",
                t.track_number.unwrap_or(0),
                t.title,
                t.cue_start_secs,
                t.cue_end_secs,
                t.duration_secs,
                t.audio_path.as_deref().unwrap_or("?")
            );
        }
    }

    #[test]
    fn expand_multifile_cue_resolves_m4a_when_cue_says_alac() {
        let base = std::env::temp_dir().join(format!("muzeeka-cue-m4a-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");

        write_bytes(&base.join("01. One.m4a"), &[1, 2, 3]);
        write_bytes(&base.join("02. Two.m4a"), &[1, 2, 3]);
        write_text(
            &base.join("album.cue"),
            r#"PERFORMER "Artist"
TITLE "Album"
FILE "01. One.ALAC" ALAC
  TRACK 01 AUDIO
    TITLE "One"
    PERFORMER "Artist"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Two"
    PERFORMER "Artist"
    INDEX 00 03:00:00
FILE "02. Two.ALAC" ALAC
    INDEX 01 00:00:00
"#,
        );

        let tracks = expand_cue_file(&base.join("album.cue"));
        assert_eq!(tracks.len(), 2, "expected 2 expanded tracks, got {}", tracks.len());
        assert!(
            tracks[0]
                .audio_path
                .as_ref()
                .is_some_and(|p| p.to_lowercase().ends_with("01. one.m4a")),
            "track 1 audio: {:?}",
            tracks[0].audio_path
        );
        assert!(
            tracks[1]
                .audio_path
                .as_ref()
                .is_some_and(|p| p.to_lowercase().ends_with("02. two.m4a")),
            "track 2 audio: {:?}",
            tracks[1].audio_path
        );
        // INDEX 00 03:00:00 → end of track 1
        assert_eq!(tracks[0].cue_start_secs, Some(0.0));
        assert_eq!(tracks[0].cue_end_secs, Some(180.0));
        assert_eq!(tracks[0].duration_secs, Some(180.0));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn expand_cue_file_splits_tracks() {
        let base = std::env::temp_dir().join(format!("muzeeka-cue-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");

        write_bytes(&base.join("album.flac"), &[1, 2, 3]);
        write_text(
            &base.join("album.cue"),
            r#"PERFORMER "Test Artist"
TITLE "Test Album"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "First Song"
    PERFORMER "Test Artist"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Second Song"
    PERFORMER "Test Artist"
    INDEX 01 02:00:00
"#,
        );

        let tracks = expand_cue_file(&base.join("album.cue"));
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].title.as_deref(), Some("First Song"));
        assert_eq!(tracks[1].title.as_deref(), Some("Second Song"));
        assert_eq!(tracks[0].cue_start_secs, Some(0.0));
        assert_eq!(tracks[1].cue_start_secs, Some(120.0));
        assert!(tracks[0].audio_path.as_ref().unwrap().ends_with("album.flac"));
        assert!(tracks[0].path.contains(CUE_PATH_MARKER));

        let target = resolve_playback(
            &tracks[1].path,
            tracks[1].audio_path.as_deref(),
            tracks[1].cue_start_secs,
            tracks[1].cue_end_secs,
        )
        .expect("resolve cue playback");
        assert!(target.audio_path.ends_with("album.flac"));
        assert_eq!(target.cue_start, Some(120.0));

        let fallback = resolve_playback(&tracks[0].path, None, None, None).expect("fallback resolve");
        assert!(fallback.audio_path.ends_with("album.flac"));
        assert_eq!(fallback.cue_start, Some(0.0));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn companion_cue_finds_audio_dot_ext_dot_cue() {
        let base = std::env::temp_dir().join(format!("muzeeka-cue-eac-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");

        let audio = base.join("album.flac");
        write_bytes(&audio, &[1, 2, 3]);
        // Exact Audio Copy style: audio.flac.cue (not audio.cue)
        write_text(
            &base.join("album.flac.cue"),
            r#"PERFORMER "Artist"
TITLE "Album"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "One"
    PERFORMER "Artist"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Two"
    PERFORMER "Artist"
    INDEX 01 01:00:00
"#,
        );

        let companion = companion_cue_for_audio(&audio).expect("find .flac.cue");
        assert!(
            companion
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("album.flac.cue"))
        );

        let tracks = expand_cue_file(&companion);
        assert_eq!(tracks.len(), 2, "expand via .flac.cue companion");

        // Prefer expanded virtual path (may include \\?\ canonicalize prefix on Windows).
        let path = tracks[1].path.clone();
        let target = resolve_playback(
            &path,
            tracks[1].audio_path.as_deref(),
            None,
            None,
        )
        .expect("resolve via companion .flac.cue");
        assert_eq!(target.cue_start, Some(60.0));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_playback_uses_cue_sheet_when_times_missing() {
        let base = std::env::temp_dir().join(format!("muzeeka-cue-resolve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");

        write_bytes(&base.join("album.ape"), &[1, 2, 3]);
        write_text(
            &base.join("album.cue"),
            r#"PERFORMER "Artist"
TITLE "Album"
FILE "album.ape" WAVE
  TRACK 01 AUDIO
    TITLE "One"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Two"
    INDEX 01 02:59:43
"#,
        );

        let tracks = expand_cue_file(&base.join("album.cue"));
        let second = &tracks[1];

        let target = resolve_playback(
            &second.path,
            second.audio_path.as_deref(),
            None,
            None,
        )
        .expect("resolve missing cue times");

        assert!(target.audio_path.ends_with("album.ape"));
        assert!(
            (target.cue_start.unwrap() - (2.0 * 60.0 + 59.0 + 43.0 / 75.0)).abs() < 0.01
        );
        assert_eq!(target.cue_end, tracks[1].cue_end_secs);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn expand_glyantseviy_cue_splits_ten_tracks() {
        let base = std::env::temp_dir().join(format!("muzeeka-glyantseviy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");

        write_bytes(&base.join("MADDY_MURK - GLYANTSEVIY.ape"), &[1, 2, 3]);
        write_text(
            &base.join("MADDY_MURK - GLYANTSEVIY.cue"),
            r#"PERFORMER "MADDY_MURK"
TITLE "GLYANTSEVIY"
FILE "MADDY_MURK - GLYANTSEVIY.ape" WAVE
  TRACK 01 AUDIO
    TITLE "DVOROVIY VOIN"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "MI MASTERA MESHAT"
    INDEX 01 02:59:43
  TRACK 03 AUDIO
    TITLE "BEHA SEMEROCHKA MOYA"
    INDEX 01 06:01:43
  TRACK 04 AUDIO
    TITLE "MUTNIY MMM"
    INDEX 01 09:01:71
  TRACK 05 AUDIO
    TITLE "GLYANTSEVIY"
    INDEX 01 12:05:27
  TRACK 06 AUDIO
    TITLE "YUNOST"
    INDEX 01 15:05:01
  TRACK 07 AUDIO
    TITLE "PLESEN"
    INDEX 01 18:06:16
  TRACK 08 AUDIO
    TITLE "DYM"
    INDEX 01 21:05:65
  TRACK 09 AUDIO
    TITLE "LADA VESTA"
    INDEX 01 23:45:08
  TRACK 10 AUDIO
    TITLE "POLITSEISKAYA"
    INDEX 01 26:40:07
"#,
        );

        let tracks = expand_cue_file(&base.join("MADDY_MURK - GLYANTSEVIY.cue"));
        assert_eq!(tracks.len(), 10);
        assert_eq!(tracks[0].title.as_deref(), Some("DVOROVIY VOIN"));
        assert_eq!(tracks[1].title.as_deref(), Some("MI MASTERA MESHAT"));
        assert_eq!(tracks[0].cue_start_secs, Some(0.0));
        assert!(
            (tracks[1].cue_start_secs.unwrap() - (2.0 * 60.0 + 59.0 + 43.0 / 75.0)).abs() < 0.01
        );
        assert!(
            (tracks[3].cue_start_secs.unwrap() - (9.0 * 60.0 + 1.0 + 71.0 / 75.0)).abs() < 0.01
        );
        assert!(tracks[0].path.contains("#cue:1"));
        assert!(tracks[9].path.contains("#cue:10"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_real_glyantseviy_when_present() {
        let ape = r"\\?\Z:\torrent\MADDY_MURK - GLYANTSEVIY - 2025\MADDY_MURK - GLYANTSEVIY.ape";
        if !Path::new(ape).is_file() {
            return;
        }

        let path = format!("{ape}#cue:3");
        let target = resolve_playback(&path, Some(ape), None, None).expect("resolve");
        assert!(target.cue_start.unwrap() > 300.0);
    }
}
