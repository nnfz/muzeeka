//! Hash paths for osu! audio. The player opens these files directly.

use std::fs;
use std::path::{Path, PathBuf};

const AUDIO_EXT: &[&str] = &["mp3", "ogg", "wav", "flac", "m4a", "aac", "opus", "wma"];
const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

pub fn hash_path(files_dir: &Path, hash: &str) -> PathBuf {
    let hash = hash.to_ascii_lowercase();
    let first = hash.get(..1).unwrap_or("0");
    let two = hash.get(..2).unwrap_or(first);
    files_dir.join(first).join(two).join(hash)
}

pub fn audio_extension(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit(['.', '/', '\\']).next()?;
    AUDIO_EXT
        .iter()
        .copied()
        .find(|known| ext.eq_ignore_ascii_case(known))
}

pub fn sniff_audio_ext(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0) {
        return Some("mp3");
    }
    if head.starts_with(b"OggS") {
        return Some("ogg");
    }
    if head.starts_with(b"fLaC") {
        return Some("flac");
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        return Some("wav");
    }
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        return Some("m4a");
    }
    None
}

pub fn choose_extension(filename: &str, head: &[u8]) -> Option<&'static str> {
    audio_extension(filename).or_else(|| sniff_audio_ext(head))
}

pub fn image_extension(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit(['.', '/', '\\']).next()?;
    IMAGE_EXT
        .iter()
        .copied()
        .find(|known| ext.eq_ignore_ascii_case(known))
}

pub fn sniff_image_ext(head: &[u8]) -> Option<&'static str> {
    if head.len() >= 2 && head[0] == 0xFF && head[1] == 0xD8 {
        return Some("jpg");
    }
    if head.starts_with(b"\x89PNG") {
        return Some("png");
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some("webp");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("gif");
    }
    None
}

pub fn choose_image_extension(filename: &str, head: &[u8]) -> Option<&'static str> {
    image_extension(filename).or_else(|| sniff_image_ext(head))
}

pub fn read_head(path: &Path) -> Vec<u8> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    use std::io::Read;
    let mut buf = [0u8; 16];
    let n = file.read(&mut buf).unwrap_or(0);
    buf[..n].to_vec()
}

pub fn find_data_dir(setting: &str) -> Result<PathBuf, String> {
    let setting = setting.trim().trim_matches('"').trim();
    if !setting.is_empty() {
        let path = PathBuf::from(setting);
        if looks_like_osu(&path) {
            return Ok(path);
        }
        return Err(format!(
            "в {} нет client.realm и files",
            path.display()
        ));
    }
    let mut tried = Vec::new();
    for base in [std::env::var_os("APPDATA"), std::env::var_os("LOCALAPPDATA")]
        .into_iter()
        .flatten()
    {
        let path = PathBuf::from(base).join("osu");
        if looks_like_osu(&path) {
            return Ok(path);
        }
        tried.push(path.display().to_string());
    }
    Err(format!(
        "не нашёл osu!lazer (смотрел {}). Укажите папку в Data folder",
        tried.join(", ")
    ))
}

pub fn looks_like_osu(path: &Path) -> bool {
    path.join("client.realm").is_file() && path.join("files").is_dir()
}

pub fn plugin_data_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base
        .join("com.nnfz.muzeeka")
        .join("plugin-data")
        .join("user.osu-lazer");
    fs::create_dir_all(&dir).map_err(|err| format!("не удалось создать {}: {err}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_path_uses_osu_prefixes() {
        let path = hash_path(Path::new("files"), "1a47929bdeadbeef");
        let text = path.to_string_lossy().replace('/', "\\");
        assert!(text.ends_with(r"files\1\1a\1a47929bdeadbeef"), "{text}");
    }

    #[test]
    fn sniff_recognises_common_audio() {
        assert_eq!(sniff_audio_ext(b"ID3......."), Some("mp3"));
        assert_eq!(sniff_audio_ext(b"OggSxxxx"), Some("ogg"));
        assert_eq!(sniff_audio_ext(b"fLaC...."), Some("flac"));
        assert_eq!(sniff_audio_ext(b"RIFF....WAVE"), Some("wav"));
        assert_eq!(choose_extension("song.mp3", b"not audio"), Some("mp3"));
    }

    #[test]
    fn sniff_recognises_backgrounds() {
        assert_eq!(sniff_image_ext(&[0xFF, 0xD8, 0xFF, 0xD9]), Some("jpg"));
        assert_eq!(sniff_image_ext(b"\x89PNG\r\n"), Some("png"));
        assert_eq!(sniff_image_ext(b"RIFF....WEBP"), Some("webp"));
        assert_eq!(sniff_image_ext(b"GIF89a"), Some("gif"));
        assert_eq!(choose_image_extension("bg.JPG", b"not image"), Some("jpg"));
        assert!(sniff_image_ext(b"ID3audio").is_none());
    }
}
