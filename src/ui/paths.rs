//! Path keys shared with the Tauri library database.
//!
//! UI paths stay absolute. SQLite stores a root plus a relative path, and
//! `path_key` is the stable lookup (`r{id}:{rel}` or the absolute key).

use std::path::{Component, Path, PathBuf};

fn strip_extended_prefix(path: &str) -> &str {
    let trimmed = path.trim();
    trimmed
        .strip_prefix(r"\\?\")
        .or_else(|| trimmed.strip_prefix("//?/"))
        .unwrap_or(trimmed)
}

pub fn is_stream_url(path: &str) -> bool {
    let path = path.trim();
    path.len() > 8
        && (path.as_bytes()[..7].eq_ignore_ascii_case(b"http://")
            || path.as_bytes()[..8].eq_ignore_ascii_case(b"https://"))
}

/// Split `file#cue:N` into the file and the `#cue:N` suffix.
pub fn split_cue(path: &str) -> (&str, Option<&str>) {
    match path.find("#cue:") {
        Some(index) => (&path[..index], Some(&path[index..])),
        None => (path, None),
    }
}

/// Equality key: strip `\\?\`, use `\` on Windows, lowercase.
pub fn path_key(path: &str) -> String {
    if is_stream_url(path) {
        return path.trim().to_lowercase();
    }
    let without_prefix = strip_extended_prefix(path);
    if cfg!(windows) {
        without_prefix.replace('/', "\\").to_lowercase()
    } else {
        without_prefix.to_string()
    }
}

pub fn storage_path_key(root_id: Option<i64>, rel_path: &str) -> String {
    match root_id {
        Some(id) => format!("r{id}:{}", path_key(rel_path)),
        None => path_key(rel_path),
    }
}

/// Collapse `.` / `..` without touching the filesystem. Stream URLs stay as-is.
pub fn normalize_display_path(path: &str) -> String {
    if is_stream_url(path) {
        return path.trim().to_string();
    }
    let (base, cue) = split_cue(path);
    let stripped = strip_extended_prefix(base);
    let mut out = PathBuf::new();
    for component in Path::new(stripped).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    let text = out.to_string_lossy().to_string();
    match cue {
        Some(suffix) => format!("{text}{suffix}"),
        None => text,
    }
}

/// Relative path of `absolute` under `root`, keeping a `#cue:` suffix.
pub fn to_relative(root: &str, absolute: &str) -> Option<String> {
    let (abs_base, cue) = split_cue(absolute);
    let root_key = path_key(root);
    let abs_key = path_key(abs_base);
    if abs_key == root_key {
        return Some(cue.unwrap_or("").to_string());
    }
    let prefix = if root_key.ends_with('\\') || root_key.ends_with('/') {
        root_key
    } else if cfg!(windows) {
        format!("{root_key}\\")
    } else {
        format!("{root_key}/")
    };
    if !abs_key.starts_with(&prefix) {
        return None;
    }
    let root_raw = strip_extended_prefix(root).trim_end_matches(['/', '\\']);
    let abs_raw = strip_extended_prefix(abs_base);
    let mut rel = abs_raw[root_raw.len().min(abs_raw.len())..].to_string();
    while rel.starts_with(['/', '\\']) {
        rel.remove(0);
    }
    if cfg!(windows) {
        rel = rel.replace('/', "\\");
    }
    Some(match cue {
        Some(suffix) => format!("{rel}{suffix}"),
        None => rel,
    })
}

/// Folder or file name, without a `\\?\` prefix or a trailing slash.
pub fn basename(path: &str) -> String {
    let mut cleaned = strip_extended_prefix(path.trim());
    cleaned = cleaned.trim_end_matches(['/', '\\']);
    if cleaned.is_empty() {
        return String::new();
    }
    cleaned
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(cleaned)
        .to_string()
}

/// Prefix used to group a dropped folder with the files scanned from it.
pub fn normalize_prefix(path: &str) -> String {
    let (base, _) = split_cue(path);
    path_key(base).trim_end_matches(['\\', '/']).to_string()
}

/// `\\?\` form so a folder walk is not cut off at 260 characters.
/// Nested album folders regularly pass that, and then the whole subtree is skipped.
pub fn extended_path(path: &Path) -> PathBuf {
    if !cfg!(windows) {
        return path.to_path_buf();
    }
    let text = path.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.starts_with(r"\\?\") {
        return PathBuf::from(trimmed);
    }
    if let Some(rest) = trimmed
        .strip_prefix(r"\\")
        .or_else(|| trimmed.strip_prefix("//"))
    {
        return PathBuf::from(format!(r"\\?\UNC\{}", rest.trim_start_matches(['\\', '/'])));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    PathBuf::from(format!(r"\\?\{}", absolute.to_string_lossy()))
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
        assert_eq!(path_key(&format!(r"{root}\{rel}")), path_key(abs));
    }

    #[test]
    fn relative_handles_extended_prefix() {
        let root = r"Z:\torrent\music";
        let abs = r"\\?\Z:\torrent\music\Album\01.flac";
        let rel = to_relative(root, abs).expect("under root");
        assert_eq!(rel.replace('/', "\\").to_lowercase(), r"album\01.flac");
        assert_eq!(path_key(abs), path_key(r"Z:\torrent\music\Album\01.flac"));
    }

    #[test]
    fn relative_keeps_cue_suffix() {
        let root = r"\\?\Z:\torrent\music";
        let abs = r"\\?\Z:\torrent\music\Nested\track.flac#cue:2";
        let rel = to_relative(root, abs).expect("under root");
        assert!(rel
            .replace('/', "\\")
            .to_lowercase()
            .starts_with(r"nested\track.flac"));
        assert!(rel.ends_with("#cue:2"));
    }

    #[test]
    fn basename_strips_prefix_and_slash() {
        assert_eq!(basename(r"\\?\C:\Music\Album\"), "Album");
        assert_eq!(basename(r"C:\Music\song.mp3"), "song.mp3");
    }
}
