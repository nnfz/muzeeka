// Audio metadata reader — ID3, Vorbis, FLAC, MP4, etc. via lofty.

use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use lofty::file::{AudioFile, TaggedFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::read_from_path;
use lofty::tag::{Accessor, Tag};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static COVER_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

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
}

/// Initialize the on-disk cover art cache under the app data directory.
pub fn init_cover_cache(app_data_dir: PathBuf) {
    let covers = app_data_dir.join("covers");
    let _ = fs::create_dir_all(&covers);
    let _ = COVER_CACHE_DIR.set(covers);
}

fn clean_tag_value(value: &str) -> String {
    value.trim().to_string()
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

fn write_thumbnail_from_bytes(data: &[u8], dest: &Path) -> bool {
    let image = match image::load_from_memory(data) {
        Ok(image) => image,
        Err(_) => return false,
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
    let (width, height) = image.dimensions();
    let thumb = if width <= THUMB_SIZE && height <= THUMB_SIZE {
        image.clone()
    } else {
        image.resize(THUMB_SIZE, THUMB_SIZE, FilterType::Triangle)
    };

    thumb.save_with_format(dest, ImageFormat::Jpeg).is_ok()
}

fn cache_cover_bytes(audio_path: &Path, data: &[u8], mime: &str, suffix: &str) -> Option<String> {
    let cache_path = thumb_cache_path(audio_path, suffix)?;

    if !cache_path.exists() {
        if !write_thumbnail_from_bytes(data, &cache_path) {
            let fallback = COVER_CACHE_DIR.get()?.join(format!(
                "{}-{}.{}",
                cache_key(audio_path),
                suffix,
                mime_to_ext(mime)
            ));
            fs::write(&fallback, data).ok()?;
            return Some(fallback.to_string_lossy().to_string());
        }
    }

    Some(cache_path.to_string_lossy().to_string())
}

fn cache_cover_file(audio_path: &Path, source: &Path) -> Option<String> {
    let cache_path = thumb_cache_path(audio_path, "nearby")?;

    let source_modified = fs::metadata(source).and_then(|m| m.modified()).ok();
    let cache_modified = fs::metadata(&cache_path).and_then(|m| m.modified()).ok();
    let needs_refresh = !cache_path.exists()
        || match (source_modified, cache_modified) {
            (Some(src), Some(cache)) => src > cache,
            _ => true,
        };

    if needs_refresh {
        if !write_thumbnail_from_file(source, &cache_path) {
            fs::copy(source, &cache_path).ok()?;
        }
    }

    Some(cache_path.to_string_lossy().to_string())
}

fn extract_embedded_cover(tagged_file: &TaggedFile, path: &Path) -> Option<String> {
    for tag in tagged_file.tags() {
        if let Some((data, mime)) = pick_cover_picture(tag) {
            if let Some(cover_path) = cache_cover_bytes(path, data, &mime, "embedded") {
                return Some(cover_path);
            }
        }
    }

    None
}

fn extract_nearby_cover(path: &Path) -> Option<String> {
    let source = find_nearby_cover(path)?;
    cache_cover_file(path, &source)
}

fn resolve_cover(path: &Path, tagged_file: Option<&TaggedFile>) -> Option<String> {
    if let Some(tagged_file) = tagged_file {
        if let Some(cover_path) = extract_embedded_cover(tagged_file, path) {
            return Some(cover_path);
        }
    }

    extract_nearby_cover(path)
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
                meta.title = non_empty(tag.title().map(|s| clean_tag_value(&s)));
                meta.artist = non_empty(tag.artist().map(|s| clean_tag_value(&s)));
                meta.album = non_empty(tag.album().map(|s| clean_tag_value(&s)));
                meta.genre = non_empty(tag.genre().map(|s| clean_tag_value(&s)));
                meta.year = tag.date().map(|date| date.year as u32);
                meta.track_number = tag.track();
            }

            meta.cover_path = resolve_cover(path, Some(tagged_file));
        }
        Err(_) => {
            meta.title = Some(filename_stem(path, file_name));
            meta.cover_path = resolve_cover(path, None);
        }
    }

    if meta.title.is_none() {
        meta.title = Some(filename_stem(path, file_name));
    }

    meta
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