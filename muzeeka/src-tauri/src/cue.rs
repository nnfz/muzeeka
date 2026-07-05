// CUE sheet parser — expands album image files into virtual tracks.

use cue_rw::{CUEFile, CUETrack, CUETimeStamp};
use num_rational::Rational32;
use std::fs;
use std::path::{Path, PathBuf};

use crate::library::MusicFile;
use crate::metadata;

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

fn companion_cue_for_audio(audio_path: &Path) -> Option<PathBuf> {
    let stem = audio_path.file_stem()?;
    let cue_path = audio_path.with_file_name(format!("{}.cue", stem.to_string_lossy()));
    if cue_path.is_file() {
        Some(cue_path)
    } else {
        None
    }
}

fn expanded_track_for_audio(audio_path: &str, track_no: u32) -> Option<MusicFile> {
    let cue_path = companion_cue_for_audio(Path::new(audio_path))?;
    expand_cue_file(&cue_path)
        .into_iter()
        .nth(track_no.saturating_sub(1) as usize)
}

fn apply_expanded_cue_track(track: &mut MusicFile, expanded: MusicFile) {
    track.audio_path = expanded.audio_path.or_else(|| track.audio_path.clone());
    track.cue_start_secs = expanded.cue_start_secs.or(track.cue_start_secs);
    track.cue_end_secs = expanded.cue_end_secs.or(track.cue_end_secs);
    if track.duration_secs.is_none() {
        track.duration_secs = expanded.duration_secs;
    }
    if track.title.is_none() {
        track.title = expanded.title;
    }
    if track.artist.is_none() {
        track.artist = expanded.artist;
    }
    if track.album.is_none() {
        track.album = expanded.album;
    }
    if track.cover_path.is_none() {
        track.cover_path = expanded.cover_path;
    }
}

/// Fill missing CUE metadata on a playlist track loaded from disk.
pub fn repair_track(track: &mut MusicFile) {
    if is_cue_track_path(&track.path) {
        if let Some((audio, track_no)) = parse_virtual_cue_path(&track.path) {
            if let Some(expanded) = expanded_track_for_audio(&audio, track_no) {
                apply_expanded_cue_track(track, expanded);
            } else if track.audio_path.is_none() {
                track.audio_path = Some(audio);
            }
        }
        return;
    }

    if is_cue_sheet_path(&track.path) {
        if let Some(expanded) = expand_cue_file(Path::new(&track.path)).into_iter().next() {
            *track = expanded;
        }
    }
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

        let expanded = expanded_track_for_audio(&audio, track_no).ok_or_else(|| {
            format!("Failed to resolve CUE track #{track_no} for {audio}")
        })?;

        let resolved_audio = audio_path
            .filter(|value| !value.is_empty() && Path::new(value).is_file())
            .map(str::to_string)
            .or(expanded.audio_path.clone())
            .unwrap_or(audio);

        return Ok(PlaybackTarget {
            audio_path: resolved_audio,
            cue_start: expanded.cue_start_secs.or(cue_start),
            cue_end: expanded.cue_end_secs.or(cue_end),
        });
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
            cue_start: None,
            cue_end: None,
        });
    }

    Err(format!("Can't open audio file: {track_path}"))
}

pub fn track_file_exists(track: &MusicFile) -> bool {
    if let Some(audio) = &track.audio_path {
        return Path::new(audio).exists();
    }
    Path::new(&track.path).exists()
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

fn parse_cue_file(cue_path: &Path) -> Option<(CUEFile, PathBuf)> {
    let content = fs::read_to_string(cue_path).ok()?;
    let cue: CUEFile = content.as_str().try_into().ok()?;
    let cue_dir = cue_path.parent()?.to_path_buf();
    Some((cue, cue_dir))
}

fn resolve_audio_file(cue_dir: &Path, file_name: &str) -> Option<PathBuf> {
    let candidate = cue_dir.join(file_name);
    if candidate.is_file() {
        return fs::canonicalize(&candidate)
            .ok()
            .or(Some(candidate));
    }

    #[cfg(windows)]
    {
        let target = file_name.to_lowercase();
        if let Ok(entries) = fs::read_dir(cue_dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().to_lowercase() == target {
                    let path = entry.path();
                    return fs::canonicalize(&path).ok().or(Some(path));
                }
            }
        }
    }

    None
}

fn end_secs_for_track(
    tracks: &[(usize, &CUETrack)],
    index: usize,
    file_id: usize,
    audio_path: &Path,
) -> Option<f64> {
    if let Some((_, next_track)) = tracks.get(index + 1) {
        if tracks[index + 1].0 == file_id {
            return track_index_start(next_track);
        }
    }

    metadata::read_metadata(audio_path, "")
        .duration_secs
}

/// Expand a .cue file into virtual `MusicFile` entries (one per TRACK).
pub fn expand_cue_file(cue_path: &Path) -> Vec<MusicFile> {
    let (cue, cue_dir) = match parse_cue_file(cue_path) {
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
        .map(|(file_id, track)| (*file_id as usize, track))
        .collect();

    let mut result = Vec::with_capacity(track_refs.len());

    for (index, (file_id, track)) in track_refs.iter().enumerate() {
        let file_name = match cue.files.get(*file_id) {
            Some(name) => name,
            None => continue,
        };

        let audio_path = match resolve_audio_file(&cue_dir, file_name) {
            Some(path) => path,
            None => continue,
        };

        let start = match track_index_start(track) {
            Some(value) => value,
            None => continue,
        };

        let end_secs = end_secs_for_track(&track_refs, index, *file_id, &audio_path);
        let duration_secs = end_secs.map(|end| (end - start).max(0.0));

        let title = non_empty(&track.title).or_else(|| album.clone());
        let artist = track
            .performer
            .as_ref()
            .and_then(|value| non_empty(value))
            .or_else(|| album_artist.clone());

        let audio_path_str = audio_path.to_string_lossy().to_string();
        let virtual_path = format!("{}{}{}", audio_path_str, CUE_PATH_MARKER, index + 1);
        let audio_meta = metadata::read_metadata(&audio_path, file_name);
        let ext = audio_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_lowercase();

        let display_name = title
            .clone()
            .unwrap_or_else(|| format!("Track {}", index + 1));

        result.push(MusicFile {
            path: virtual_path,
            file_name: display_name,
            extension: ext,
            size: fs::metadata(&audio_path).map(|meta| meta.len()).unwrap_or(0),
            title,
            artist,
            album: album.clone(),
            duration_secs,
            year: None,
            track_number: Some((index + 1) as u32),
            genre: None,
            cover_path: audio_meta.cover_path,
            audio_path: Some(audio_path_str),
            cue_start_secs: Some(start),
            cue_end_secs: end_secs,
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

        for file_name in &cue.files {
            if let Some(audio) = resolve_audio_file(&cue_dir, file_name) {
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
