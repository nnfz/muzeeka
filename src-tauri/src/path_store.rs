//! Portable library path storage: library roots + relative paths.
//!
//! UI / player always see absolute paths. SQLite stores:
//! - `library_roots.path` — absolute root (can be relocated when drive letter changes)
//! - `tracks.rel_path` — path relative to root, or absolute if `root_id` is NULL
//! - `tracks.path_key` — stable lookup key (`r{id}:{rel}` or absolute key)

use std::path::{Component, Path, PathBuf};

/// Strip Windows extended-length prefix (`\\?\` / `//?/`) without changing case.
fn strip_extended_path_prefix(path: &str) -> &str {
    let trimmed = path.trim();
    trimmed
        .strip_prefix(r"\\?\")
        .or_else(|| trimmed.strip_prefix("//?/"))
        .unwrap_or(trimmed)
}

/// Normalize for equality / DB keys (Windows: strip `\\?\`, `\` separators, lowercase).
pub fn path_key(path: &str) -> String {
    let without_prefix = strip_extended_path_prefix(path);
    if cfg!(windows) {
        without_prefix.replace('/', "\\").to_lowercase()
    } else {
        without_prefix.to_string()
    }
}

/// Split `file#cue:N` → (file, Some("#cue:N")) or (path, None).
pub fn split_cue_suffix(path: &str) -> (&str, Option<&str>) {
    if let Some(idx) = path.find("#cue:") {
        (&path[..idx], Some(&path[idx..]))
    } else {
        (path, None)
    }
}

pub fn storage_path_key(root_id: Option<i64>, rel_path: &str) -> String {
    match root_id {
        Some(id) => format!("r{id}:{}", path_key(rel_path)),
        None => path_key(rel_path),
    }
}

/// Join root + relative (preserves `#cue:` suffix on rel).
pub fn join_root(root: &str, rel: &str) -> String {
    let (rel_base, cue) = split_cue_suffix(rel);
    let root = root.trim_end_matches(['/', '\\']);
    let rel_base = rel_base.trim_start_matches(['/', '\\']);
    let joined = if rel_base.is_empty() {
        root.to_string()
    } else if cfg!(windows) {
        format!("{root}\\{rel_base}")
    } else {
        format!("{root}/{rel_base}")
    };
    match cue {
        Some(suffix) => format!("{joined}{suffix}"),
        None => joined,
    }
}

/// If `absolute` is under `root`, return relative path (with cue suffix preserved).
pub fn to_relative(root: &str, absolute: &str) -> Option<String> {
    let (abs_base, cue) = split_cue_suffix(absolute);

    // Compare with normalized keys (handles `\\?\` + case).
    let root_key = path_key(root);
    let abs_key = path_key(abs_base);
    if abs_key == root_key {
        return Some(cue.unwrap_or("").to_string());
    }
    let prefix = if root_key.ends_with('\\') || root_key.ends_with('/') {
        root_key.clone()
    } else if cfg!(windows) {
        format!("{root_key}\\")
    } else {
        format!("{root_key}/")
    };
    if !abs_key.starts_with(&prefix) {
        return None;
    }

    // Slice raw strings *after* stripping `\\?\` from both — otherwise
    // root without prefix + canonicalize abs with `\\?\` shifts root_len by 4
    // and produces garbage relatives (e.g. `ic\Album\01.flac`).
    let root_raw = strip_extended_path_prefix(root).trim_end_matches(['/', '\\']);
    let abs_raw = strip_extended_path_prefix(abs_base);
    let root_len = root_raw.len();
    let mut rel = abs_raw[root_len.min(abs_raw.len())..].to_string();
    while rel.starts_with(['/', '\\']) {
        rel.remove(0);
    }
    // Normalize separators for storage consistency
    if cfg!(windows) {
        rel = rel.replace('/', "\\");
    }
    Some(match cue {
        Some(suffix) => format!("{rel}{suffix}"),
        None => rel,
    })
}

/// Collapse `.` / `..` and normalize separators (best-effort, no filesystem access).
pub fn normalize_display_path(path: &str) -> String {
    let (base, cue) = split_cue_suffix(path);
    let stripped = strip_extended_path_prefix(base);
    let path = Path::new(stripped);
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    let s = out.to_string_lossy().to_string();
    match cue {
        Some(suffix) => format!("{s}{suffix}"),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_roundtrip_windows_style() {
        let root = r"Z:\torrent\music";
        let abs = r"Z:\torrent\music\Album\01.flac";
        let rel = to_relative(root, abs).expect("under root");
        assert_eq!(rel.replace('/', "\\").to_lowercase(), r"album\01.flac");
        let back = join_root(root, &rel);
        assert_eq!(path_key(&back), path_key(abs));
    }

    #[test]
    fn relative_handles_extended_path_prefix_on_abs() {
        // User root without `\\?\`, canonicalize-style abs with it (common on Windows).
        let root = r"Z:\torrent\music";
        let abs = r"\\?\Z:\torrent\music\Album\01.flac";
        let rel = to_relative(root, abs).expect("under root with extended prefix");
        assert_eq!(
            rel.replace('/', "\\").to_lowercase(),
            r"album\01.flac",
            "got relative {rel:?}"
        );
        assert_eq!(path_key(&join_root(root, &rel)), path_key(abs));
    }

    #[test]
    fn relative_handles_extended_prefix_on_both() {
        let root = r"\\?\Z:\torrent\music";
        let abs = r"\\?\Z:\torrent\music\Nested\track.flac#cue:2";
        let rel = to_relative(root, abs).expect("under extended root");
        assert!(rel.replace('/', "\\").to_lowercase().starts_with(r"nested\track.flac"));
        assert!(rel.ends_with("#cue:2"));
    }

    #[test]
    fn cue_suffix_preserved() {
        let root = r"Z:\lib";
        let abs = r"Z:\lib\disc.flac#cue:3";
        let rel = to_relative(root, abs).unwrap();
        assert!(rel.contains("#cue:3"));
        assert_eq!(path_key(&join_root(root, &rel)), path_key(abs));
    }

    #[test]
    fn storage_key_stable_under_root() {
        let k1 = storage_path_key(Some(1), r"Album\a.flac");
        let k2 = storage_path_key(Some(1), r"album/a.flac");
        assert_eq!(k1, k2);
    }
}
