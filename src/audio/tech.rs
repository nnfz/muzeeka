//! Stream and file details for the track properties window.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::ItemKey;

/// Technical audio and file details for Properties → Details.
#[derive(Debug, Clone, Default)]
pub struct AudioTechInfo {
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u8>,
    pub bit_depth: Option<u8>,
    pub duration_secs: Option<f64>,
    pub total_samples: Option<u64>,
    pub codec: Option<String>,
    pub encoding: Option<String>,
    pub tool: Option<String>,
    pub audio_md5: Option<String>,
    pub embedded_cuesheet: Option<bool>,
    pub file_size: Option<u64>,
    pub modified_unix: Option<i64>,
    pub created_unix: Option<i64>,
    pub file_name: Option<String>,
    pub folder_name: Option<String>,
    pub file_path: Option<String>,
    pub play_count: Option<u32>,
    pub first_played_unix: Option<i64>,
    pub last_played_unix: Option<i64>,
}

fn system_time_unix(time: SystemTime) -> Option<i64> {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => Some(duration.as_secs() as i64),
        Err(error) => Some(-(error.duration().as_secs() as i64)),
    }
}

fn file_type_codec(kind: lofty::file::FileType) -> (&'static str, &'static str) {
    use lofty::file::FileType;
    match kind {
        FileType::Flac => ("FLAC", "lossless"),
        FileType::Wav => ("WAV", "lossless"),
        FileType::Aiff => ("AIFF", "lossless"),
        FileType::Ape => ("Monkey's Audio", "lossless"),
        FileType::WavPack => ("WavPack", "lossless"),
        FileType::Mpeg => ("MP3", "lossy"),
        FileType::Aac => ("AAC", "lossy"),
        FileType::Mp4 => ("MP4", "lossy"),
        FileType::Opus => ("Opus", "lossy"),
        FileType::Vorbis => ("Vorbis", "lossy"),
        FileType::Speex => ("Speex", "lossy"),
        FileType::Mpc => ("Musepack", "lossy"),
        FileType::Custom(_) | _ => ("Unknown", "unknown"),
    }
}

fn fill_fs_fields(path: &Path, info: &mut AudioTechInfo) {
    info.file_path = Some(path.to_string_lossy().to_string());
    info.file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string());
    info.folder_name = path
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .filter(|folder| !folder.is_empty());
    if let Ok(meta) = fs::metadata(path) {
        info.file_size = Some(meta.len());
        if let Ok(modified) = meta.modified() {
            info.modified_unix = system_time_unix(modified);
        }
        if let Ok(created) = meta.created() {
            info.created_unix = system_time_unix(created);
        }
    }
}

/// Probe stream and filesystem details. Cover art is not read.
pub fn read_audio_tech_info(path: &Path) -> AudioTechInfo {
    let mut info = AudioTechInfo::default();
    fill_fs_fields(path, &mut info);

    let Ok(tagged_file) = (Probe::open(path).and_then(|probe| {
        probe
            .options(ParseOptions::new().read_cover_art(false))
            .read()
    })) else {
        return info;
    };

    let (codec, encoding) = file_type_codec(tagged_file.file_type());
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_uppercase());
    info.codec = Some(match (codec, ext.as_deref()) {
        ("MP4", Some("M4A")) => "ALAC/AAC".to_string(),
        ("MP4", Some(ext)) => ext.to_string(),
        ("MP3", Some("MP2")) => "MP2".to_string(),
        _ => codec.to_string(),
    });
    info.encoding = Some(encoding.to_string());

    let props = tagged_file.properties();
    info.bitrate_kbps = props
        .audio_bitrate()
        .filter(|&rate| rate > 0)
        .or_else(|| props.overall_bitrate().filter(|&rate| rate > 0));
    info.sample_rate_hz = props.sample_rate().filter(|&rate| rate > 0);
    info.channels = props.channels().filter(|&channels| channels > 0);
    info.bit_depth = props.bit_depth().filter(|&depth| depth > 0);

    let duration = props.duration();
    if !duration.is_zero() {
        let secs = duration.as_secs_f64();
        info.duration_secs = Some(secs);
        if let Some(rate) = info.sample_rate_hz {
            let samples = (secs * f64::from(rate)).round();
            if samples.is_finite() && samples > 0.0 {
                info.total_samples = Some(samples as u64);
            }
        }
    }

    if let Some(tag) = tagged_file.primary_tag().or_else(|| tagged_file.first_tag()) {
        info.tool = tag
            .get_string(ItemKey::EncoderSoftware)
            .or_else(|| tag.get_string(ItemKey::EncodedBy))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let has_cue = tag.items().any(|item| {
            let key = format!("{:?}", item.key()).to_ascii_uppercase();
            if key.contains("CUESHEET") || key.contains("CUE_SHEET") {
                return true;
            }
            match item.value() {
                lofty::tag::ItemValue::Text(text) | lofty::tag::ItemValue::Locator(text) => {
                    let upper = text.to_ascii_uppercase();
                    upper.contains("FILE \"") && upper.contains("TRACK ") && upper.contains("INDEX ")
                }
                _ => false,
            }
        });
        info.embedded_cuesheet = Some(has_cue);
    } else {
        info.embedded_cuesheet = Some(false);
    }

    if matches!(tagged_file.file_type(), lofty::file::FileType::Flac) {
        if let Ok(mut file) = fs::File::open(path) {
            if let Ok(flac) = lofty::flac::FlacFile::read_from(
                &mut file,
                ParseOptions::new().read_cover_art(false),
            ) {
                let signature = flac.properties().signature();
                if signature != 0 {
                    info.audio_md5 = Some(format!("{signature:032X}"));
                }
            }
        }
    }

    info
}
