// M3U / M3U8 playlist parser
//
// Expands playlist files into ordered local audio/CUE paths so the library
// scanner can import them like a normal multi-file drop.

use std::fs;
use std::path::{Path, PathBuf};


/// True for `.m3u` / `.m3u8` (case-insensitive).
pub fn is_m3u_extension(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("m3u") || ext.eq_ignore_ascii_case("m3u8")
}

pub fn is_m3u_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(is_m3u_extension)
}

/// Read playlist text with the same encoding fallbacks as CUE sheets.
fn read_m3u_text(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }

    if let Ok(text) = std::str::from_utf8(&bytes) {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        return Some(text.to_string());
    }

    {
        let (cow, _enc, had_errors) = encoding_rs::WINDOWS_1251.decode(&bytes);
        if !had_errors || cow.contains('#') || cow.contains('.') {
            return Some(cow.into_owned());
        }
    }

    {
        let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
        Some(cow.into_owned())
    }
}

fn strip_quotes(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 {
        let bytes = t.as_bytes();
        if (bytes[0] == b'"' && bytes[t.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[t.len() - 1] == b'\'')
        {
            return &t[1..t.len() - 1];
        }
    }
    t
}

fn is_remote_entry(entry: &str) -> bool {
    let lower = entry.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("rtsp://")
        || lower.starts_with("mms://")
}

/// Decode `file://` URIs into a local path string when possible.
fn decode_file_url(entry: &str) -> Option<String> {
    let lower = entry.to_ascii_lowercase();
    if !lower.starts_with("file:") {
        return None;
    }

    // file:///C:/Music/track.mp3  or  file://localhost/C:/...  or  file:/C:/...
    let rest = entry
        .strip_prefix("file://")
        .or_else(|| entry.strip_prefix("file:"))
        .unwrap_or(entry);

    let rest = rest
        .strip_prefix("//")
        .unwrap_or(rest)
        .strip_prefix("localhost")
        .unwrap_or(rest);

    // Percent-decode common escapes without pulling in a full URL crate.
    let mut out = String::with_capacity(rest.len());
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h1 = bytes[i + 1];
            let h2 = bytes[i + 2];
            if let (Some(a), Some(b)) = (from_hex(h1), from_hex(h2)) {
                out.push((a << 4 | b) as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }

    // Unix file URLs start with /; Windows may be /C:/...
    #[cfg(windows)]
    {
        let trimmed = out.trim_start_matches('/');
        if trimmed.len() >= 2 {
            let mut chars = trimmed.chars();
            if let (Some(drive), Some(':')) = (chars.next(), chars.next()) {
                if drive.is_ascii_alphabetic() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    Some(out)
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn resolve_entry(base_dir: &Path, raw: &str) -> Option<PathBuf> {
    let entry = strip_quotes(raw).trim();
    if entry.is_empty() || is_remote_entry(entry) {
        return None;
    }

    let path_str = if let Some(decoded) = decode_file_url(entry) {
        decoded
    } else {
        entry.to_string()
    };

    let candidate = PathBuf::from(&path_str);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    };

    // Prefer existing path; otherwise still return the joined path so the
    // scanner can try canonicalize / CUE expansion later.
    if absolute.exists() {
        return Some(absolute);
    }

    fs::canonicalize(&absolute).ok().or(Some(absolute))
}

/// One remote stream entry from a playlist: the URL and its `#EXTINF` display
/// name (station name) when present.
#[derive(Debug, Clone, PartialEq)]
pub struct M3uStream {
    pub url: String,
    pub name: Option<String>,
}

/// Extract remote HTTP(S) stream entries (internet radio) from a playlist,
/// pairing each URL with the display name from its preceding `#EXTINF` line.
/// Local file entries are ignored here (see [`expand_m3u_paths`] for those).
pub fn expand_m3u_streams(m3u_path: &Path) -> Vec<M3uStream> {
    let text = match read_m3u_text(m3u_path) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Pending display name carried from the most recent `#EXTINF:secs,Name` line.
    let mut pending_name: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            // Format: `#EXTINF:<seconds>,<display name>`.
            pending_name = rest
                .splitn(2, ',')
                .nth(1)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        let entry = strip_quotes(line).trim();
        let name = pending_name.take();
        if !(entry.to_ascii_lowercase().starts_with("http://")
            || entry.to_ascii_lowercase().starts_with("https://"))
        {
            continue;
        }
        if seen.insert(entry.to_lowercase()) {
            out.push(M3uStream {
                url: entry.to_string(),
                name,
            });
        }
    }

    out
}

/// Ordered local path entries from an M3U playlist (audio / cue paths).
/// Remote URLs and blank/comment lines are skipped.
pub fn expand_m3u_paths(m3u_path: &Path) -> Vec<PathBuf> {
    let text = match read_m3u_text(m3u_path) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let base_dir = m3u_path.parent().unwrap_or_else(|| Path::new("."));
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some(resolved) = resolve_entry(base_dir, line) else {
            continue;
        };

        let key = {
            #[cfg(windows)]
            {
                resolved.to_string_lossy().to_lowercase()
            }
            #[cfg(not(windows))]
            {
                resolved.to_string_lossy().to_string()
            }
        };
        if seen.insert(key) {
            out.push(resolved);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(path: &Path, body: &str) {
        let mut f = fs::File::create(path).expect("create");
        f.write_all(body.as_bytes()).expect("write");
    }

    #[test]
    fn expands_relative_and_skips_remote() {
        let base = std::env::temp_dir().join(format!("muzeeka-m3u-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("dir");
        write_file(&base.join("a.mp3"), "x");
        write_file(&base.join("b.flac"), "y");
        write_file(
            &base.join("list.m3u"),
            "#EXTM3U\n#EXTINF:1,A\na.mp3\nhttps://example.com/x.mp3\nb.flac\n",
        );

        let paths = expand_m3u_paths(&base.join("list.m3u"));
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("a.mp3"));
        assert!(paths[1].ends_with("b.flac"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn expands_file_url() {
        let base = std::env::temp_dir().join(format!("muzeeka-m3u-url-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("dir");
        let track = base.join("t.mp3");
        write_file(&track, "z");

        let url = format!("file:///{}", track.to_string_lossy().replace('\\', "/"));
        write_file(&base.join("list.m3u"), &format!("{url}\n"));

        let paths = expand_m3u_paths(&base.join("list.m3u"));
        assert_eq!(paths.len(), 1);

        let _ = fs::remove_dir_all(&base);
    }
}
