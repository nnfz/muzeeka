#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackTarget {
    pub audio_path: String,
    pub cue_start: Option<f64>,
    pub cue_end: Option<f64>,
}

pub fn is_cue_track_path(path: &str) -> bool {
    path.contains("#cue:")
}

pub fn is_cue_sheet_path(path: &str) -> bool {
    path.to_lowercase().ends_with(".cue")
}

pub fn is_stream_url(path: &str) -> bool {
    let p = path.trim();
    p.len() > 8
        && (p.as_bytes()[..7].eq_ignore_ascii_case(b"http://")
            || p.as_bytes()[..8].eq_ignore_ascii_case(b"https://"))
}

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
