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
use walkdir::WalkDir;

use crate::cue;
use crate::metadata;

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

fn is_covered_audio(path: &Path, covered: &[String]) -> bool {
    let canonical = fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string();

    covered.iter().any(|entry| path_key(entry) == path_key(&canonical))
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

fn music_file_from_path(path: &Path, read_tags: bool) -> Option<MusicFile> {
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
        apply_metadata(&mut file, metadata::read_metadata(path, &filename));
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

fn build_files_from_paths(paths: Vec<PathBuf>, read_tags: bool) -> Vec<MusicFile> {
    let cue_paths = collect_cue_paths(&paths);
    let covered = cue::covered_audio_paths(&cue_paths);
    let mut files: Vec<MusicFile> = paths
        .par_iter()
        .filter(|path| {
            let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
                return false;
            };
            is_audio_extension(ext) && !is_covered_audio(path, &covered)
        })
        .filter_map(|path| music_file_from_path(path, read_tags))
        .collect();

    for cue_path in cue_paths {
        files.extend(cue::expand_cue_file(&cue_path));
    }

    for file in &mut files {
        cue::repair_track(file);
    }

    files
}

fn collect_from_directory(root: &Path, results: &mut Vec<MusicFile>, seen: &mut HashSet<String>) {
    let paths = collect_paths_from_directory(root);
    for file in build_files_from_paths(paths, true) {
        let key = path_key(&file.path);
        if seen.insert(key) {
            results.push(file);
        }
    }
}

/// Scan a directory recursively for music files.
pub fn scan_directory(dir: &str) -> Result<Vec<MusicFile>, String> {
    let root = resolve_dropped_path(dir).ok_or_else(|| format!("Directory does not exist: {}", dir))?;

    if !is_directory(&root) {
        return Err(format!("Path is not a directory: {}", dir));
    }

    let mut results = Vec::new();
    let mut seen = HashSet::new();
    collect_from_directory(&root, &mut results, &mut seen);
    Ok(results)
}

/// Scan dropped paths — individual audio files and folders (recursive).
pub fn scan_paths(paths: &[String]) -> Result<Vec<MusicFile>, String> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    for path_str in paths {
        let Some(path) = resolve_dropped_path(path_str) else {
            continue;
        };

        if is_directory(&path) {
            collect_from_directory(&path, &mut results, &mut seen);
            continue;
        }

        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(is_cue_extension)
        {
            for file in cue::expand_cue_file(&path) {
                let key = path_key(&file.path);
                if seen.insert(key) {
                    results.push(file);
                }
            }
            continue;
        }

        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(is_audio_extension)
        {
            if let Some(cue_path) = companion_cue_path(&path) {
                for file in cue::expand_cue_file(&cue_path) {
                    let key = path_key(&file.path);
                    if seen.insert(key) {
                        results.push(file);
                    }
                }
                continue;
            }

            if let Some(file) = music_file_from_path(&path, true) {
                let key = path_key(&file.path);
                if seen.insert(key) {
                    results.push(file);
                }
                continue;
            }
        }

        // Fallback: some Windows folder drops may not report as directories via metadata.
        if fs::read_dir(&path).is_ok() {
            collect_from_directory(&path, &mut results, &mut seen);
        }
    }

    Ok(results)
}

/// Read or refresh metadata for existing file paths.
pub fn fetch_metadata(paths: &[String]) -> Result<Vec<MusicFile>, String> {
    let resolved: Vec<PathBuf> = paths
        .iter()
        .filter(|path_str| !cue::is_cue_track_path(path_str))
        .filter_map(|path_str| resolve_dropped_path(path_str))
        .collect();

    Ok(dedupe_files(build_files_from_paths(resolved, true)))
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
        assert!(files[0].title.is_some());

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
}