// CUE sheet parsing module — STUB
//
// TODO: Port from src-tauri/src/cue.rs (1351 lines)

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackTarget {
    pub audio_path: String,
    pub cue_start: Option<f64>,
    pub cue_end: Option<f64>,
}

/// Check if path is a virtual CUE track path (contains #cue: marker)
pub fn is_cue_track_path(path: &str) -> bool {
    path.contains("#cue:")
}

/// Check if path is a CUE sheet file
pub fn is_cue_sheet_path(path: &str) -> bool {
    path.to_lowercase().ends_with(".cue")
}

/// Check if path is an HTTP/HTTPS stream URL
pub fn is_stream_url(path: &str) -> bool {
    let p = path.trim();
    p.len() > 8
        && (p.as_bytes()[..7].eq_ignore_ascii_case(b"http://")
            || p.as_bytes()[..8].eq_ignore_ascii_case(b"https://"))
}

/// Resolve playback target from track metadata
///
/// For now, returns a simple passthrough. Full implementation will:
/// - Parse virtual CUE paths (#cue: marker)
/// - Resolve audio file paths
/// - Apply CUE INDEX time ranges
pub fn resolve_playback(
    track_path: &str,
    audio_path: &str,
    cue_start: Option<f64>,
    cue_end: Option<f64>,
) -> PlaybackTarget {
    PlaybackTarget {
        audio_path: if !audio_path.is_empty() {
            audio_path.to_string()
        } else {
            track_path.to_string()
        },
        cue_start,
        cue_end,
    }
}
