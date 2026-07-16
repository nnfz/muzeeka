// Audio metadata reader — ID3, Vorbis, FLAC, MP4, etc. via lofty.
// Falls back to the `id3` crate for MP3 files with tricky unsynchronisation tags.

use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::read_from_path;
use lofty::tag::{Accessor, Tag, TagType};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static COVER_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
static PLAYLIST_COVER_DIR: OnceLock<PathBuf> = OnceLock::new();

const PLAYLIST_COVER_SIZE: u32 = 256;
const MAX_PLAYLIST_GIF_BYTES: u64 = 20 * 1024 * 1024;

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif"];
const COVER_NAMES: &[&str] = &[
    "cover", "folder", "front", "album", "albumart", "artwork", "albumartsmall",
];
const THUMB_SIZE: u32 = 96;

#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub genre: Option<String>,
    pub cover_path: Option<String>,
    pub cover_path_full: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct CoverPaths {
    thumb: Option<String>,
    full: Option<String>,
}

/// Initialize the on-disk cover art cache under the app data directory.
pub fn init_cover_cache(app_data_dir: PathBuf) {
    let covers = app_data_dir.join("covers");
    let _ = fs::create_dir_all(&covers);
    let _ = COVER_CACHE_DIR.set(covers);

    let playlist_covers = app_data_dir.join("playlist_covers");
    let _ = fs::create_dir_all(&playlist_covers);
    let _ = PLAYLIST_COVER_DIR.set(playlist_covers);
}

fn clean_tag_value(value: &str) -> String {
    value.trim().to_string()
}

/// Strip yt-dlp video id suffix like ` [2351315453]` from titles / filenames.
pub fn strip_ytdlp_id_suffix(value: &str) -> String {
    let trimmed = value.trim();
    let Some(open) = trimmed.rfind(" [") else {
        return trimmed.to_string();
    };

    if !trimmed.ends_with(']') {
        return trimmed.to_string();
    }

    let inside = &trimmed[open + 2..trimmed.len() - 1];
    if inside.is_empty() || !inside.chars().all(|c| c.is_ascii_digit()) {
        return trimmed.to_string();
    }

    trimmed[..open].trim().to_string()
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

fn filename_stem(path: &Path, fallback: &str) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(clean_tag_value)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| clean_tag_value(fallback))
}

fn cache_key(path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.to_string_lossy().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(crate) fn mime_from_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        _ => "image/jpeg",
    }
}

fn is_cover_cache_path(path: &str) -> bool {
    let Some(cache_dir) = COVER_CACHE_DIR.get() else {
        return false;
    };
    Path::new(path).starts_with(cache_dir)
}

fn mime_to_ext(mime: &str) -> &str {
    match mime {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

fn guess_mime(data: &[u8]) -> String {
    if data.len() >= 4 && data[..4] == [0x89, b'P', b'N', b'G'] {
        "image/png".to_string()
    } else if data.len() >= 3 && data[..3] == [0xFF, 0xD8, 0xFF] {
        "image/jpeg".to_string()
    } else {
        "image/jpeg".to_string()
    }
}

fn is_image_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
}

fn find_nearby_cover(audio_path: &Path) -> Option<PathBuf> {
    let dir = audio_path.parent()?;
    let stem = audio_path.file_stem()?.to_str()?;

    for ext in IMAGE_EXTENSIONS {
        let sidecar = dir.join(format!("{stem}.{ext}"));
        if is_image_file(&sidecar) {
            return Some(sidecar);
        }
    }

    for name in COVER_NAMES {
        for ext in IMAGE_EXTENSIONS {
            let candidate = dir.join(format!("{name}.{ext}"));
            if is_image_file(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

fn pick_cover_picture(tag: &Tag) -> Option<(&[u8], String)> {
    let picture = tag
        .get_picture_type(PictureType::CoverFront)
        .or_else(|| tag.pictures().first())?;

    let data = picture.data();
    if data.is_empty() {
        return None;
    }

    let mime = picture
        .mime_type()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| guess_mime(data));

    Some((data, mime))
}

fn thumb_cache_path(audio_path: &Path, suffix: &str) -> Option<PathBuf> {
    let cache_dir = COVER_CACHE_DIR.get()?;
    Some(
        cache_dir.join(format!(
            "{}-{}-thumb.jpg",
            cache_key(audio_path),
            suffix
        )),
    )
}

fn full_cache_path(audio_path: &Path, suffix: &str, ext: &str) -> Option<PathBuf> {
    let cache_dir = COVER_CACHE_DIR.get()?;
    Some(
        cache_dir.join(format!(
            "{}-{}-full.{}",
            cache_key(audio_path),
            suffix,
            ext
        )),
    )
}

fn cache_full_cover_bytes(
    audio_path: &Path,
    data: &[u8],
    mime: &str,
    suffix: &str,
) -> Option<String> {
    let ext = mime_to_ext(mime);
    let cache_path = full_cache_path(audio_path, suffix, ext)?;

    if cache_path.exists() {
        return Some(cache_path.to_string_lossy().to_string());
    }

    fs::write(&cache_path, data).ok()?;
    Some(cache_path.to_string_lossy().to_string())
}

fn decode_image_bytes(data: &[u8]) -> Option<image::DynamicImage> {
    image::load_from_memory(data).ok()
}

#[allow(dead_code)]
fn write_thumbnail_from_bytes(data: &[u8], dest: &Path) -> bool {
    let Some(image) = decode_image_bytes(data) else {
        return false;
    };
    write_thumbnail_from_image(&image, dest)
}

fn write_thumbnail_from_file(source: &Path, dest: &Path) -> bool {
    let image = match image::open(source) {
        Ok(image) => image,
        Err(_) => return false,
    };
    write_thumbnail_from_image(&image, dest)
}

fn write_thumbnail_from_image(image: &image::DynamicImage, dest: &Path) -> bool {
    write_resized_jpeg(image, dest, THUMB_SIZE)
}

fn write_resized_jpeg(image: &image::DynamicImage, dest: &Path, max_size: u32) -> bool {
    let (width, height) = image.dimensions();
    let thumb = if width <= max_size && height <= max_size {
        image.clone()
    } else {
        image.resize(max_size, max_size, FilterType::Triangle)
    };

    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }

    thumb.save_with_format(dest, ImageFormat::Jpeg).is_ok()
}

fn sanitized_playlist_id(playlist_id: &str) -> Result<String, String> {
    let safe: String = playlist_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() {
        Err("Invalid playlist id".to_string())
    } else {
        Ok(safe)
    }
}

fn is_animated_gif(source: &Path) -> bool {
    if source
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"))
    {
        return true;
    }

    let Ok(data) = fs::read(source) else {
        return false;
    };
    data.len() >= 6 && (&data[..6] == b"GIF87a" || &data[..6] == b"GIF89a")
}

fn clear_cached_playlist_covers(dir: &Path, safe_id: &str) {
    for ext in ["jpg", "jpeg", "gif", "png", "webp", "bmp"] {
        let path = dir.join(format!("{safe_id}.{ext}"));
        if path.is_file() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Copy and resize a user-picked image into the playlist cover cache.
/// GIFs are copied as-is so animation is preserved.
pub fn cache_playlist_cover(playlist_id: &str, source: &Path) -> Result<String, String> {
    if !source.is_file() {
        return Err("Cover image file not found".to_string());
    }

    let safe_id = sanitized_playlist_id(playlist_id)?;
    let dir = PLAYLIST_COVER_DIR
        .get()
        .ok_or_else(|| "Playlist cover cache not initialized".to_string())?;

    clear_cached_playlist_covers(dir, &safe_id);

    if is_animated_gif(source) {
        let size = fs::metadata(source)
            .map_err(|e| format!("Failed to read cover file: {e}"))?
            .len();
        if size > MAX_PLAYLIST_GIF_BYTES {
            return Err(format!(
                "GIF is too large (max {} MB)",
                MAX_PLAYLIST_GIF_BYTES / (1024 * 1024)
            ));
        }

        let dest = dir.join(format!("{safe_id}.gif"));
        fs::copy(source, &dest).map_err(|e| format!("Failed to copy GIF cover: {e}"))?;
        return Ok(dest.to_string_lossy().to_string());
    }

    let dest = dir.join(format!("{safe_id}.jpg"));
    let image = image::open(source).map_err(|e| format!("Failed to open image: {e}"))?;
    if !write_resized_jpeg(&image, &dest, PLAYLIST_COVER_SIZE) {
        return Err("Failed to write playlist cover".to_string());
    }

    Ok(dest.to_string_lossy().to_string())
}

/// Remove a cached custom playlist cover file.
pub fn remove_playlist_cover_file(playlist_id: &str) -> Result<(), String> {
    let safe_id = sanitized_playlist_id(playlist_id)?;
    let Some(dir) = PLAYLIST_COVER_DIR.get() else {
        return Ok(());
    };
    clear_cached_playlist_covers(dir, &safe_id);
    Ok(())
}

fn cache_cover_bytes(audio_path: &Path, data: &[u8], mime: &str, suffix: &str) -> CoverPaths {
    let mut paths = CoverPaths::default();
    let cache_path = thumb_cache_path(audio_path, suffix);

    // Fast path: thumb + full already on disk — never re-decode the embedded JPEG.
    // read_metadata is called often (Discord RPC, enrichment); decoding 1200² covers
    // every time was a free main-thread stall for tracks with large APIC frames.
    if let Some(ref thumb_path) = cache_path {
        if thumb_path.exists() {
            paths.thumb = Some(thumb_path.to_string_lossy().to_string());
            paths.full = cache_full_cover_bytes(audio_path, data, mime, suffix)
                .or_else(|| paths.thumb.clone());
            return paths;
        }
    }

    let Some(image) = decode_image_bytes(data) else {
        return paths;
    };

    if let Some(cache_path) = cache_path {
        if !write_thumbnail_from_image(&image, &cache_path) {
            return paths;
        }
        if cache_path.exists() {
            paths.thumb = Some(cache_path.to_string_lossy().to_string());
        }
    }

    paths.full = cache_full_cover_bytes(audio_path, data, mime, suffix);
    paths
}

fn cache_cover_file(audio_path: &Path, source: &Path) -> CoverPaths {
    let mut paths = CoverPaths::default();
    let cache_path = thumb_cache_path(audio_path, "nearby");

    if let Some(cache_path) = cache_path {
        let source_modified = fs::metadata(source).and_then(|m| m.modified()).ok();
        let cache_modified = fs::metadata(&cache_path).and_then(|m| m.modified()).ok();
        let needs_refresh = !cache_path.exists()
            || match (source_modified, cache_modified) {
                (Some(src), Some(cache)) => src > cache,
                _ => true,
            };

        if needs_refresh {
            if !write_thumbnail_from_file(source, &cache_path) {
                fs::copy(source, &cache_path).ok();
            }
        }

        if cache_path.exists() {
            paths.thumb = Some(cache_path.to_string_lossy().to_string());
        }
    }

    if let Ok(data) = fs::read(source) {
        let mime = mime_from_path(source);
        paths.full = cache_full_cover_bytes(audio_path, &data, mime, "nearby");
    }

    if paths.full.is_none() {
        paths.full = paths.thumb.clone();
    }

    paths
}


fn extract_embedded_cover(tagged_file: &TaggedFile, path: &Path) -> CoverPaths {
    // For MP3/AIFF files, lofty mis-applies unsynchronisation decoding on the
    // picture data — it inserts extra 0x00 bytes after every 0xFF, producing a
    // corrupt JPEG (no EOI marker, broken scan data). The `id3` crate decodes
    // unsync correctly, so try it first for ID3-bearing formats.
    if let Some(paths) = extract_embedded_cover_id3(path) {
        return paths;
    }

    // For all other formats (FLAC, OGG, MP4, …) fall back to lofty.
    for tag in tagged_file.tags() {
        if let Some((data, mime)) = pick_cover_picture(tag) {
            if data.len() < 256 {
                continue;
            }
            let paths = cache_cover_bytes(path, data, &mime, "embedded");
            if paths.thumb.is_some() || paths.full.is_some() {
                return paths;
            }
        }
    }

    CoverPaths::default()
}

/// Extract cover art from an MP3/ID3 file using the `id3` crate as a fallback.

fn extract_embedded_cover_id3(path: &Path) -> Option<CoverPaths> {
    // Only attempt for files that could carry ID3 tags.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("mp3") | Some("aiff") | Some("aif") => {}
        _ => return None,
    }

    // Cached cover from a previous read — skip re-parsing ID3 + re-decoding JPEG.
    if let Some(paths) = existing_embedded_cover_cache(path) {
        return Some(paths);
    }

    let tag = id3::Tag::read_from_path(path).ok()?;
    let pic = tag
        .pictures()
        .find(|p| p.picture_type == id3::frame::PictureType::CoverFront)
        .or_else(|| tag.pictures().next())?;

    if pic.data.is_empty() {
        return None;
    }

    let mime = if pic.mime_type.is_empty() {
        guess_mime(&pic.data)
    } else {
        pic.mime_type.clone()
    };

    let paths = cache_cover_bytes(path, &pic.data, &mime, "embedded");
    if paths.thumb.is_some() || paths.full.is_some() {
        Some(paths)
    } else {
        None
    }
}

fn existing_embedded_cover_cache(path: &Path) -> Option<CoverPaths> {
    let thumb = thumb_cache_path(path, "embedded").filter(|p| p.is_file())?;
    let full = ["jpg", "jpeg", "png", "webp", "bmp", "gif"]
        .into_iter()
        .find_map(|ext| full_cache_path(path, "embedded", ext).filter(|p| p.is_file()));

    Some(CoverPaths {
        thumb: Some(thumb.to_string_lossy().to_string()),
        full: full
            .map(|p| p.to_string_lossy().to_string())
            .or_else(|| Some(thumb.to_string_lossy().to_string())),
    })
}

fn extract_nearby_cover(path: &Path) -> CoverPaths {
    let source = match find_nearby_cover(path) {
        Some(source) => source,
        None => return CoverPaths::default(),
    };
    cache_cover_file(path, &source)
}

fn resolve_cover_paths(path: &Path, tagged_file: Option<&TaggedFile>) -> CoverPaths {
    if let Some(tagged_file) = tagged_file {
        let embedded = extract_embedded_cover(tagged_file, path);
        if embedded.thumb.is_some() || embedded.full.is_some() {
            return embedded;
        }
    } else {
        // No lofty tag at all — still try id3 fallback for MP3.
        if let Some(paths) = extract_embedded_cover_id3(path) {
            if paths.thumb.is_some() || paths.full.is_some() {
                return paths;
            }
        }
    }

    extract_nearby_cover(path)
}

/// Resolve a full-resolution cover path for an audio file (creates cache if needed).
pub fn resolve_full_cover(path: &Path) -> Option<String> {
    let tagged_file = read_from_path(path).ok();
    let paths = match tagged_file.as_ref() {
        Some(tagged_file) => resolve_cover_paths(path, Some(tagged_file)),
        None => resolve_cover_paths(path, None),
    };

    if let Some(ref full) = paths.full {
        if is_cover_cache_path(full) {
            return paths.full;
        }
    }

    paths.thumb.or(paths.full)
}

/// Read a cover image from disk and return a data URL (for paths outside the asset scope).
pub fn cover_data_url(path: &Path) -> Result<Option<String>, String> {
    if !path.is_file() {
        return Ok(None);
    }

    let data = fs::read(path).map_err(|e| format!("Failed to read cover: {e}"))?;
    let mime = mime_from_path(path);
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    Ok(Some(format!(
        "data:{mime};base64,{}",
        STANDARD.encode(data)
    )))
}

/// Read tags and audio properties from a file. Falls back to the filename when tags are missing.
pub fn read_metadata(path: &Path, file_name: &str) -> TrackMetadata {
    let mut meta = TrackMetadata::default();

    let tagged_file = read_from_path(path);

    match &tagged_file {
        Ok(tagged_file) => {
            let duration = tagged_file.properties().duration();
            if !duration.is_zero() {
                meta.duration_secs = Some(duration.as_secs_f64());
            }

            let tag = tagged_file
                .primary_tag()
                .or_else(|| tagged_file.first_tag());

            if let Some(tag) = tag {
                meta.title = non_empty(
                    tag.title()
                        .map(|s| strip_ytdlp_id_suffix(&clean_tag_value(&s))),
                );
                meta.artist = non_empty(tag.artist().map(|s| clean_tag_value(&s)));
                meta.album = non_empty(tag.album().map(|s| clean_tag_value(&s)));
                meta.genre = non_empty(tag.genre().map(|s| clean_tag_value(&s)));
                meta.year = tag.date().map(|date| date.year as u32);
                meta.track_number = tag.track();
            }

            let covers = resolve_cover_paths(path, Some(tagged_file));
            meta.cover_path = covers.thumb;
            meta.cover_path_full = covers.full;
        }
        Err(_) => {
            meta.title = Some(strip_ytdlp_id_suffix(&filename_stem(path, file_name)));
            let covers = resolve_cover_paths(path, None);
            meta.cover_path = covers.thumb;
            meta.cover_path_full = covers.full;
        }
    }

    if meta.title.is_none() {
        meta.title = Some(strip_ytdlp_id_suffix(&filename_stem(path, file_name)));
    }

    meta
}

/// Write title and/or artist into the file's primary tag (creates ID3v2 for MP3 when missing).
pub fn write_track_tags(
    path: &Path,
    title: Option<&str>,
    artist: Option<&str>,
) -> Result<(), String> {
    let title = title
        .map(strip_ytdlp_id_suffix)
        .filter(|s| !s.is_empty());
    let artist = artist
        .map(clean_tag_value)
        .filter(|s| !s.is_empty());

    if title.is_none() && artist.is_none() {
        return Ok(());
    }

    let mut tagged_file = read_from_path(path)
        .map_err(|e| format!("Failed to read audio file for tagging: {}", e))?;

    if tagged_file.primary_tag_mut().is_none() {
        let tag_type = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag())
            .map(|tag| tag.tag_type())
            .unwrap_or(TagType::Id3v2);
        tagged_file.insert_tag(Tag::new(tag_type));
    }

    let tag = tagged_file
        .primary_tag_mut()
        .ok_or_else(|| "No writable tag slot".to_string())?;

    if let Some(title) = title {
        tag.set_title(title);
    }
    if let Some(artist) = artist {
        tag.set_artist(artist);
    }

    tagged_file
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("Failed to save tags: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_bytes(path: &Path, bytes: &[u8]) {
        let mut file = fs::File::create(path).expect("create file");
        file.write_all(bytes).expect("write file");
    }

    #[test]
    fn strip_ytdlp_id_suffix_removes_trailing_video_id() {
        assert_eq!(
            strip_ytdlp_id_suffix("авиасейлс - на морозе [2351315453]"),
            "авиасейлс - на морозе"
        );
        assert_eq!(strip_ytdlp_id_suffix("plain title"), "plain title");
    }

    #[test]
    fn find_nearby_cover_prefers_sidecar_image() {
        let base = std::env::temp_dir().join(format!("muzeeka-cover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");

        let audio = base.join("song.mp3");
        write_bytes(&audio, &[1, 2, 3]);
        write_bytes(&base.join("cover.jpg"), &[0xFF, 0xD8, 0xFF, 1]);
        write_bytes(&base.join("song.png"), &[0x89, b'P', b'N', b'G']);

        let cover = find_nearby_cover(&audio).expect("sidecar cover");
        assert_eq!(cover.file_name().unwrap().to_str().unwrap(), "song.png");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn find_nearby_cover_falls_back_to_folder_art() {
        let base = std::env::temp_dir().join(format!("muzeeka-cover-folder-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");

        let audio = base.join("song.flac");
        write_bytes(&audio, &[1, 2, 3]);
        write_bytes(&base.join("folder.jpg"), &[0xFF, 0xD8, 0xFF, 1]);

        let cover = find_nearby_cover(&audio).expect("folder cover");
        assert_eq!(cover.file_name().unwrap().to_str().unwrap(), "folder.jpg");

        let _ = fs::remove_dir_all(&base);
    }
}