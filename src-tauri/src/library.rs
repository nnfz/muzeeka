// Music library scanner
//
// Uses walkdir for fast recursive directory traversal.
// Filters by common audio file extensions (including tracker/chiptune via plugins)
// and reads tags via lofty (falls back to filename for formats without tags).

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use walkdir::WalkDir;

use crate::cue;
use crate::m3u;
use crate::metadata;

/// Payload for `library:scan-progress` (commands + native drop import).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryScanProgress {
    pub current: usize,
    pub total: usize,
    pub label: String,
}

/// Throttled progress callback (~200 updates max per scan). Shared by
/// `library_scan`, `library_scan_paths`, and drop-handler import.
pub fn make_throttled_scan_progress<F>(
    emit: F,
) -> impl Fn(usize, usize, &Path) + Send + Sync + 'static
where
    F: Fn(usize, usize, &str) + Send + Sync + 'static,
{
    let last_emitted = Mutex::new(0usize);
    move |current, total, path| {
        let step = (total / 200).max(1);
        if current > 0 && current < total && current % step != 0 {
            return;
        }
        let Ok(mut last) = last_emitted.lock() else {
            return;
        };
        if current > 0 && current < *last {
            return;
        }
        *last = current;
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Scanning music...");
        emit(current, total, label);
    }
}

/// Supported audio file extensions (lowercase).
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "wav", "aac", "m4a", "wma", "opus", "ape",
    // Tracker / chiptune / module formats (supported via BASS plugins like basszxtune or similar)
    "mod", "s3m", "xm", "it", "ay", "ym", "vgm", "vgz", "nsf", "nsfe",
    "gbs", "hes", "sap", "kss", "pt2", "pt3", "stc", "stp", "asc", "sqt", "psg",
];

/// A discovered music file with embedded metadata when available.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicFile {
    /// Full path to the file.
    pub path: String,
    /// File name without directory.
    pub file_name: String,
    /// File extension (lowercase, no dot).
    pub extension: String,
    /// File size in bytes.
    pub size: u64,
    /// Track title from tags, or filename stem as fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Primary artist from tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Album from tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Duration in seconds from audio properties.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    /// Release year from tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    /// Track number from tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    /// Genre from tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// Cached cover art path on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_path: Option<String>,
    /// Full-resolution cover art path (original file or uncropped cache).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_path_full: Option<String>,
    /// Underlying audio file for CUE sheet tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_path: Option<String>,
    /// Start offset in seconds for CUE sheet tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cue_start_secs: Option<f64>,
    /// End offset in seconds for CUE sheet tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cue_end_secs: Option<f64>,
}

fn clean_path_string(path_str: &str) -> String {
    path_str
        .trim()
        .trim_matches('\0')
        .chars()
        .filter(|c| *c != '\0')
        .collect()
}

fn strip_trailing_separator(mut path: PathBuf) -> PathBuf {
    while path.components().count() > 1 {
        match path.file_name().and_then(|s| s.to_str()) {
            Some("") | None => {
                path.pop();
            }
            _ => break,
        }
    }
    path
}

fn resolve_dropped_path(path_str: &str) -> Option<PathBuf> {
    let cleaned = clean_path_string(path_str);
    if cleaned.is_empty() {
        return None;
    }

    let path = strip_trailing_separator(PathBuf::from(&cleaned));

    if fs::metadata(&path).is_ok() {
        return Some(path);
    }

    fs::canonicalize(&path).ok()
}

fn is_audio_extension(ext: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

fn is_cue_extension(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("cue")
}

/// Pre-normalized path keys for O(1) CUE-covered audio checks.
fn covered_audio_keys(covered: &[String]) -> HashSet<String> {
    covered.iter().map(|entry| path_key(entry)).collect()
}

fn is_covered_audio(path: &Path, covered_keys: &HashSet<String>) -> bool {
    let canonical = fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string();
    covered_keys.contains(&path_key(&canonical))
}

fn is_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

fn path_key(path: &str) -> String {
    #[cfg(windows)]
    {
        path.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

fn apply_metadata(file: &mut MusicFile, meta: metadata::TrackMetadata) {
    file.title = meta.title;
    file.artist = meta.artist;
    file.album = meta.album;
    file.duration_secs = meta.duration_secs;
    file.year = meta.year;
    file.track_number = meta.track_number;
    file.genre = meta.genre;
    file.cover_path = meta.cover_path;
    file.cover_path_full = meta.cover_path_full;
}

fn music_file_from_path(
    path: &Path,
    read_tags: bool,
    resolve_covers: bool,
) -> Option<MusicFile> {
    if !is_regular_file(path) {
        return None;
    }

    let ext = path.extension().and_then(|e| e.to_str())?;
    if !is_audio_extension(ext) {
        return None;
    }

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let full_path = fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string();

    let mut file = MusicFile {
        path: full_path,
        file_name: filename.clone(),
        extension: ext.to_lowercase(),
        size,
        title: None,
        artist: None,
        album: None,
        duration_secs: None,
        year: None,
        track_number: None,
        genre: None,
        cover_path: None,
        cover_path_full: None,
        audio_path: None,
        cue_start_secs: None,
        cue_end_secs: None,
    };

    if read_tags {
        let meta = if resolve_covers {
            metadata::read_metadata(path, &filename)
        } else {
            metadata::read_metadata_fast(path, &filename)
        };
        apply_metadata(&mut file, meta);
    }

    Some(file)
}

fn dedupe_files(files: Vec<MusicFile>) -> Vec<MusicFile> {
    let mut results = Vec::with_capacity(files.len());
    let mut seen = HashSet::new();

    for file in files {
        let key = path_key(&file.path);
        if seen.insert(key) {
            results.push(file);
        }
    }

    results
}

fn collect_paths_from_directory(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .flatten()
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .collect()
}

fn companion_cue_path(audio_path: &Path) -> Option<PathBuf> {
    cue::companion_cue_for_audio(audio_path)
}

fn collect_cue_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut cue_paths: Vec<PathBuf> = paths
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(is_cue_extension)
        })
        .cloned()
        .collect();

    // A directory scan already includes every sibling .cue file in `paths`.
    // Avoid reparsing all sheets once per audio file while looking for a
    // multi-file companion; explicit single-file imports still use the lookup.
    if !cue_paths.is_empty() {
        return cue_paths;
    }

    for path in paths {
        let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !is_audio_extension(ext) {
            continue;
        }
        if let Some(cue_path) = companion_cue_path(path) {
            if !cue_paths.iter().any(|existing| existing == &cue_path) {
                cue_paths.push(cue_path);
            }
        }
    }

    cue_paths
}

type ScanProgressCallback<'a> = &'a (dyn Fn(usize, usize, &Path) + Sync);

fn build_files_from_paths(
    paths: Vec<PathBuf>,
    read_tags: bool,
    resolve_covers: bool,
    skip_cue: bool,
    progress: Option<ScanProgressCallback<'_>>,
) -> Vec<MusicFile> {
    let cue_paths = if skip_cue { vec![] } else { collect_cue_paths(&paths) };
    let covered_keys = covered_audio_keys(&cue::covered_audio_paths(&cue_paths));
    let audio_paths: Vec<&PathBuf> = paths
        .iter()
        .filter(|path| {
            let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
                return false;
            };
            is_audio_extension(ext) && !is_covered_audio(path, &covered_keys)
        })
        .collect();
    let total = audio_paths.len() + cue_paths.len();
    let completed = AtomicUsize::new(0);

    if let Some(report) = progress {
        report(0, total, Path::new(""));
    }

    let mut files: Vec<MusicFile> = audio_paths
        .par_iter()
        .filter_map(|path| {
            let file = music_file_from_path(path, read_tags, resolve_covers);
            if let Some(report) = progress {
                let current = completed.fetch_add(1, Ordering::Relaxed) + 1;
                report(current, total, path);
            }
            file
        })
        .collect();

    for cue_path in cue_paths {
        if resolve_covers {
            files.extend(cue::expand_cue_file(&cue_path));
        } else {
            files.extend(cue::expand_cue_file_fast(&cue_path));
        }
        if let Some(report) = progress {
            let current = completed.fetch_add(1, Ordering::Relaxed) + 1;
            report(current, total, &cue_path);
        }
    }

    for file in &mut files {
        cue::repair_track(file);
    }

    files
}

fn collect_from_directory(
    root: &Path,
    results: &mut Vec<MusicFile>,
    seen: &mut HashSet<String>,
    progress: Option<ScanProgressCallback<'_>>,
) {
    let paths = collect_paths_from_directory(root);
    // Fast path scan: no ID3 / no covers / skip CUE expand here.
    // Tags + covers are filled later via `library_fetch_metadata` (lazy UI).
    // `skip_cue=true` keeps folder walks cheap; explicit .cue drops still expand
    // in `scan_paths_impl`.
    for file in build_files_from_paths(paths, false, false, true, progress) {
        let key = path_key(&file.path);
        if seen.insert(key) {
            results.push(file);
        }
    }
}

/// Scan a directory recursively for music files.
#[allow(dead_code)]
pub fn scan_directory(dir: &str) -> Result<Vec<MusicFile>, String> {
    scan_directory_impl(dir, None)
}

pub fn scan_directory_with_progress(
    dir: &str,
    progress: ScanProgressCallback<'_>,
) -> Result<Vec<MusicFile>, String> {
    scan_directory_impl(dir, Some(progress))
}

fn scan_directory_impl(
    dir: &str,
    progress: Option<ScanProgressCallback<'_>>,
) -> Result<Vec<MusicFile>, String> {
    let root = resolve_dropped_path(dir).ok_or_else(|| format!("Directory does not exist: {}", dir))?;

    if !is_directory(&root) {
        return Err(format!("Path is not a directory: {}", dir));
    }

    let mut results = Vec::new();
    let mut seen = HashSet::new();
    collect_from_directory(&root, &mut results, &mut seen, progress);
    Ok(results)
}

/// Scan dropped paths — individual audio files and folders (recursive).
#[allow(dead_code)]
pub fn scan_paths(paths: &[String]) -> Result<Vec<MusicFile>, String> {
    scan_paths_impl(paths, None)
}

pub fn scan_paths_with_progress(
    paths: &[String],
    progress: ScanProgressCallback<'_>,
) -> Result<Vec<MusicFile>, String> {
    scan_paths_impl(paths, Some(progress))
}

fn scan_paths_impl(
    paths: &[String],
    progress: Option<ScanProgressCallback<'_>>,
) -> Result<Vec<MusicFile>, String> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    for (input_index, path_str) in paths.iter().enumerate() {
        let Some(path) = resolve_dropped_path(path_str) else {
            continue;
        };

        if is_directory(&path) {
            collect_from_directory(&path, &mut results, &mut seen, progress);
            continue;
        }

        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(is_cue_extension)
        {
            for file in cue::expand_cue_file_fast(&path) {
                let key = path_key(&file.path);
                if seen.insert(key) {
                    results.push(file);
                }
            }
            if let Some(report) = progress {
                report(input_index + 1, paths.len(), &path);
            }
            continue;
        }

        if m3u::is_m3u_path(&path) {
            // Expand entries, but never follow nested .m3u (avoids cycles).
            let nested: Vec<String> = m3u::expand_m3u_paths(&path)
                .into_iter()
                .filter(|p| !m3u::is_m3u_path(p))
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if !nested.is_empty() {
                let nested_files = scan_paths_impl(&nested, None)?;
                for file in nested_files {
                    let key = path_key(&file.path);
                    if seen.insert(key) {
                        results.push(file);
                    }
                }
            }
            if let Some(report) = progress {
                report(input_index + 1, paths.len(), &path);
            }
            continue;
        }

        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(is_audio_extension)
        {
            if let Some(file) = music_file_from_path(&path, false, false) {
                let key = path_key(&file.path);
                if seen.insert(key) {
                    results.push(file);
                }
                if let Some(report) = progress {
                    report(input_index + 1, paths.len(), &path);
                }
                continue;
            }
        }

        // Fallback: some Windows folder drops may not report as directories via metadata.
        if fs::read_dir(&path).is_ok() {
            collect_from_directory(&path, &mut results, &mut seen, progress);
        }
    }

    Ok(results)
}

/// Read or refresh metadata for existing file paths.
pub fn fetch_metadata(paths: &[String]) -> Result<Vec<MusicFile>, String> {
    let mut results: Vec<MusicFile> = Vec::new();

    // CUE virtual tracks: re-expand from the companion sheet so duration /
    // INDEX bounds stay correct (never overwrite with full-container length).
    for path_str in paths {
        if !cue::is_cue_track_path(path_str) {
            continue;
        }
        let mut track = MusicFile {
            path: path_str.clone(),
            file_name: path_str
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(path_str)
                .to_string(),
            extension: String::new(),
            size: 0,
            title: None,
            artist: None,
            album: None,
            duration_secs: None,
            year: None,
            track_number: None,
            genre: None,
            cover_path: None,
            cover_path_full: None,
            audio_path: None,
            cue_start_secs: None,
            cue_end_secs: None,
        };
        if cue::repair_track(&mut track) {
            // Fill covers from the audio file without clobbering CUE duration.
            if let Some(audio) = track.audio_path.clone() {
                let audio_path = Path::new(&audio);
                if audio_path.is_file() {
                    let meta = metadata::read_metadata(
                        audio_path,
                        audio_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(""),
                    );
                    if track.cover_path.is_none() {
                        track.cover_path = meta.cover_path;
                    }
                    if track.cover_path_full.is_none() {
                        track.cover_path_full = meta.cover_path_full;
                    }
                    if track.title.is_none() {
                        track.title = meta.title;
                    }
                    if track.artist.is_none() {
                        track.artist = meta.artist;
                    }
                    if track.album.is_none() {
                        track.album = meta.album;
                    }
                }
            }
            results.push(track);
        }
    }

    let resolved: Vec<PathBuf> = paths
        .iter()
        .filter(|path_str| !cue::is_cue_track_path(path_str))
        .filter_map(|path_str| resolve_dropped_path(path_str))
        .collect();

    results.extend(build_files_from_paths(resolved, true, true, true, None));
    Ok(dedupe_files(results))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_test_file(path: &Path, bytes: &[u8]) {
        let mut file = fs::File::create(path).expect("create test file");
        file.write_all(bytes).expect("write test file");
    }

    #[test]
    fn scan_paths_accepts_directory_with_nested_audio() {
        let base = std::env::temp_dir().join(format!("muzeeka-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let nested = base.join("nested");
        fs::create_dir_all(&nested).expect("create nested dir");
        write_test_file(&nested.join("track.mp3"), &[1, 2, 3]);

        let files = scan_paths(&[base.to_string_lossy().to_string()]).expect("scan paths");
        assert_eq!(files.len(), 1);
        assert!(files[0].file_name.ends_with("track.mp3"));
        // Directory scan is path-only; titles come from `fetch_metadata` later.
        assert!(files[0].title.is_none());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn fetch_metadata_fills_title_from_filename_fallback() {
        let base = std::env::temp_dir().join(format!("muzeeka-meta-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create meta dir");
        let track = base.join("My Cool Track.mp3");
        write_test_file(&track, &[1, 2, 3]);

        let path = track.to_string_lossy().to_string();
        let files = fetch_metadata(&[path.clone()]).expect("fetch metadata");
        assert_eq!(files.len(), 1);
        assert!(
            files[0].title.as_deref() == Some("My Cool Track"),
            "expected filename stem title, got {:?}",
            files[0].title
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_paths_accepts_directory_with_trailing_separator() {
        let base = std::env::temp_dir().join(format!("muzeeka-scan-trail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");
        write_test_file(&base.join("one.flac"), &[9]);

        let mut dir = base.to_string_lossy().to_string();
        if cfg!(windows) {
            dir.push('\\');
        } else {
            dir.push('/');
        }

        let files = scan_paths(&[dir]).expect("scan trailing separator");
        assert_eq!(files.len(), 1);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_directory_reports_progress_while_reading_files() {
        let base = std::env::temp_dir().join(format!("muzeeka-progress-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create progress dir");
        write_test_file(&base.join("one.mp3"), &[1]);
        write_test_file(&base.join("two.flac"), &[2]);

        let updates = std::sync::Mutex::new(Vec::new());
        let files = scan_directory_with_progress(&base.to_string_lossy(), &|current, total, path| {
            updates.lock().expect("lock progress").push((
                current,
                total,
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string(),
            ));
        })
        .expect("scan with progress");

        let updates = updates.into_inner().expect("progress updates");
        assert_eq!(files.len(), 2);
        assert_eq!(updates.first().map(|item| (item.0, item.1)), Some((0, 2)));
        assert_eq!(updates.iter().map(|item| item.0).max(), Some(2));
        assert!(updates.iter().all(|item| item.1 == 2));

        let _ = fs::remove_dir_all(&base);
    }
}
