// Audio metadata reader — ID3, Vorbis, FLAC, MP4, etc. via lofty.
// Falls back to the `id3` crate for MP3 files with tricky unsynchronisation tags.

use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::read_from_path;
use lofty::tag::{Accessor, Tag, TagType};
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

static COVER_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
static PLAYLIST_COVER_DIR: OnceLock<PathBuf> = OnceLock::new();
/// Bundled ffmpeg binary (for GIF → animated WebP). Set once at app startup.
static FFMPEG_BIN: OnceLock<Option<PathBuf>> = OnceLock::new();

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

/// Register the ffmpeg binary used for animated GIF → WebP conversion.
pub fn set_ffmpeg_bin(path: Option<PathBuf>) {
    let _ = FFMPEG_BIN.set(path);
}

fn ffmpeg_bin() -> Option<&'static Path> {
    FFMPEG_BIN.get().and_then(|p| p.as_deref())
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

fn guess_mime(data: &[u8]) -> String {
    if data.len() >= 4 && data[..4] == [0x89, b'P', b'N', b'G'] {
        "image/png".to_string()
    } else if data.len() >= 12 && data[..4] == *b"RIFF" && data[8..12] == *b"WEBP" {
        "image/webp".to_string()
    } else if data.len() >= 3 && data[..3] == [0xFF, 0xD8, 0xFF] {
        "image/jpeg".to_string()
    } else if data.len() >= 6 && (&data[..6] == b"GIF87a" || &data[..6] == b"GIF89a") {
        "image/gif".to_string()
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

/// Stable content id for cover bytes (FNV-1a 64 + length). Same APIC → same id
/// across tracks, so album art is stored once.
fn cover_content_id(data: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in data {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    // Mix length so equal-prefix images of different sizes never collide.
    hash ^= data.len() as u64;
    hash = hash.wrapping_mul(0x100000001b3);
    format!("{hash:016x}")
}

fn content_thumb_path(content_id: &str) -> Option<PathBuf> {
    let cache_dir = COVER_CACHE_DIR.get()?;
    Some(cache_dir.join(format!("c-{content_id}-thumb.webp")))
}

fn content_full_path(content_id: &str) -> Option<PathBuf> {
    let cache_dir = COVER_CACHE_DIR.get()?;
    Some(cache_dir.join(format!("c-{content_id}-full.webp")))
}

/// Tiny per-track pointer so we can resolve covers without re-parsing tags
/// when the audio file hasn't changed.
fn track_cover_ref_path(audio_path: &Path, suffix: &str) -> Option<PathBuf> {
    let cache_dir = COVER_CACHE_DIR.get()?;
    Some(cache_dir.join(format!(
        "t-{}-{suffix}.ref",
        cache_key(audio_path)
    )))
}

fn write_track_cover_ref(audio_path: &Path, suffix: &str, content_id: &str) {
    let Some(ref_path) = track_cover_ref_path(audio_path, suffix) else {
        return;
    };
    if let Some(parent) = ref_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&ref_path, content_id.as_bytes());
}

fn read_track_cover_ref(audio_path: &Path, suffix: &str) -> Option<String> {
    let ref_path = track_cover_ref_path(audio_path, suffix)?;
    if !ref_path.is_file() {
        return None;
    }

    // Invalidate if the audio file is newer than the pointer (tags changed).
    let audio_mtime = fs::metadata(audio_path).and_then(|m| m.modified()).ok();
    let ref_mtime = fs::metadata(&ref_path).and_then(|m| m.modified()).ok();
    if let (Some(audio_t), Some(ref_t)) = (audio_mtime, ref_mtime) {
        if audio_t > ref_t {
            return None;
        }
    }

    let id = fs::read_to_string(&ref_path).ok()?;
    let id = id.trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(id.to_string())
}

fn paths_for_content_id(content_id: &str) -> Option<CoverPaths> {
    let thumb = content_thumb_path(content_id)?;
    if !thumb.is_file() {
        return None;
    }
    let full = content_full_path(content_id)
        .filter(|p| p.is_file())
        .unwrap_or_else(|| thumb.clone());
    Some(CoverPaths {
        thumb: Some(thumb.to_string_lossy().to_string()),
        full: Some(full.to_string_lossy().to_string()),
    })
}

/// Ensure content-addressed full + thumb WebP exist for this image payload.
fn ensure_content_cover_files(data: &[u8], _mime: &str) -> Option<(String, CoverPaths)> {
    if data.is_empty() {
        return None;
    }
    let content_id = cover_content_id(data);
    let thumb_path = content_thumb_path(&content_id)?;
    let full_path = content_full_path(&content_id)?;

    // Both already on disk — shared by every track with this APIC.
    if thumb_path.is_file() && full_path.is_file() {
        return Some((
            content_id,
            CoverPaths {
                thumb: Some(thumb_path.to_string_lossy().to_string()),
                full: Some(full_path.to_string_lossy().to_string()),
            },
        ));
    }

    // Write full if missing
    if !full_path.is_file() {
        if data.len() >= 12 && data[..4] == *b"RIFF" && data[8..12] == *b"WEBP" {
            if let Some(parent) = full_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(&full_path, data).ok()?;
        } else {
            let image = decode_image_bytes(data)?;
            if !write_webp(&image, &full_path) {
                return None;
            }
        }
    }

    // Write thumb if missing (decode once)
    if !thumb_path.is_file() {
        let image = if full_path.is_file() {
            image::open(&full_path).ok().or_else(|| decode_image_bytes(data))
        } else {
            decode_image_bytes(data)
        }?;
        if !write_thumbnail_from_image(&image, &thumb_path) {
            return None;
        }
    }

    if !thumb_path.is_file() {
        return None;
    }

    let full = if full_path.is_file() {
        full_path.to_string_lossy().to_string()
    } else {
        thumb_path.to_string_lossy().to_string()
    };

    Some((
        content_id,
        CoverPaths {
            thumb: Some(thumb_path.to_string_lossy().to_string()),
            full: Some(full),
        },
    ))
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

fn write_thumbnail_from_image(image: &image::DynamicImage, dest: &Path) -> bool {
    write_resized_webp(image, dest, THUMB_SIZE)
}

fn write_webp(image: &image::DynamicImage, dest: &Path) -> bool {
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // image crate WebP encoder is lossless (VP8L) — smaller than PNG, no extra deps.
    image.save_with_format(dest, ImageFormat::WebP).is_ok()
}

fn write_resized_webp(image: &image::DynamicImage, dest: &Path, max_size: u32) -> bool {
    let (width, height) = image.dimensions();
    let thumb = if width <= max_size && height <= max_size {
        image.clone()
    } else {
        image.resize(max_size, max_size, FilterType::Triangle)
    };
    write_webp(&thumb, dest)
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

fn is_gif_bytes(data: &[u8]) -> bool {
    data.len() >= 6 && (&data[..6] == b"GIF87a" || &data[..6] == b"GIF89a")
}

fn is_gif_path(source: &Path) -> bool {
    if source
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"))
    {
        return true;
    }
    fs::read(source).map(|d| is_gif_bytes(&d)).unwrap_or(false)
}

fn clear_cached_playlist_covers(dir: &Path, safe_id: &str) {
    for ext in ["jpg", "jpeg", "gif", "png", "webp", "bmp"] {
        let path = dir.join(format!("{safe_id}.{ext}"));
        if path.is_file() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Convert a GIF (animated or still) to WebP. Prefers animated WebP via ffmpeg;
/// falls back to a still WebP of the first frame.
fn gif_file_to_webp(source_gif: &Path, dest_webp: &Path, max_edge: u32) -> Result<(), String> {
    if let Some(ffmpeg) = ffmpeg_bin() {
        if convert_gif_to_webp_ffmpeg(source_gif, dest_webp, ffmpeg, max_edge).is_ok() {
            if dest_webp.is_file() {
                return Ok(());
            }
        }
    }

    // Still fallback (first frame) when ffmpeg is missing or conversion fails.
    let image = image::open(source_gif).map_err(|e| format!("Failed to open GIF: {e}"))?;
    if !write_resized_webp(&image, dest_webp, max_edge) {
        return Err("Failed to write still WebP from GIF".to_string());
    }
    Ok(())
}

fn convert_gif_to_webp_ffmpeg(
    source_gif: &Path,
    dest_webp: &Path,
    ffmpeg: &Path,
    max_edge: u32,
) -> Result<(), String> {
    if let Some(parent) = dest_webp.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // Scale so the longer side ≤ max_edge; keep aspect ratio; preserve alpha when present.
    let vf = format!(
        "scale='min({max_edge},iw)':'min({max_edge},ih)':force_original_aspect_ratio=decrease:flags=lanczos"
    );

    let status = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
        ])
        .arg(source_gif)
        .args(["-vf", &vf])
        .args([
            "-c:v",
            "libwebp",
            "-lossless",
            "0",
            "-q:v",
            "80",
            "-loop",
            "0",
            "-an",
            "-vsync",
            "0",
        ])
        .arg(dest_webp)
        .status()
        .map_err(|e| format!("Failed to run ffmpeg for WebP: {e}"))?;

    if !status.success() {
        // Retry without explicit codec (some builds auto-pick libwebp_anim).
        let status2 = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(source_gif)
            .args(["-vf", &vf, "-loop", "0", "-an", "-vsync", "0"])
            .arg(dest_webp)
            .status()
            .map_err(|e| format!("Failed to run ffmpeg for WebP: {e}"))?;
        if !status2.success() || !dest_webp.is_file() {
            return Err("ffmpeg GIF→WebP conversion failed".to_string());
        }
    }

    if !dest_webp.is_file() {
        return Err("ffmpeg finished but WebP is missing".to_string());
    }
    Ok(())
}

fn gif_bytes_to_webp(gif_bytes: &[u8], dest_webp: &Path, max_edge: u32) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!(
        "muzeeka-cover-{}.gif",
        std::process::id()
    ));
    fs::write(&tmp, gif_bytes).map_err(|e| format!("Failed to write temp GIF: {e}"))?;
    let result = gif_file_to_webp(&tmp, dest_webp, max_edge);
    let _ = fs::remove_file(&tmp);
    result
}

/// Copy and resize a user-picked image into the playlist cover cache.
/// GIFs are converted to (animated) WebP.
pub fn cache_playlist_cover(playlist_id: &str, source: &Path) -> Result<String, String> {
    if !source.is_file() {
        return Err("Cover image file not found".to_string());
    }

    let safe_id = sanitized_playlist_id(playlist_id)?;
    let dir = PLAYLIST_COVER_DIR
        .get()
        .ok_or_else(|| "Playlist cover cache not initialized".to_string())?;

    clear_cached_playlist_covers(dir, &safe_id);
    let dest = dir.join(format!("{safe_id}.webp"));

    if is_gif_path(source) {
        let size = fs::metadata(source)
            .map_err(|e| format!("Failed to read cover file: {e}"))?
            .len();
        if size > MAX_PLAYLIST_GIF_BYTES {
            return Err(format!(
                "GIF is too large (max {} MB)",
                MAX_PLAYLIST_GIF_BYTES / (1024 * 1024)
            ));
        }
        gif_file_to_webp(source, &dest, PLAYLIST_COVER_SIZE)?;
        return Ok(dest.to_string_lossy().to_string());
    }

    let image = image::open(source).map_err(|e| format!("Failed to open image: {e}"))?;
    if !write_resized_webp(&image, &dest, PLAYLIST_COVER_SIZE) {
        return Err("Failed to write playlist cover".to_string());
    }

    Ok(dest.to_string_lossy().to_string())
}

/// Download a remote image and store it as the playlist cover.
pub fn cache_playlist_cover_from_url(playlist_id: &str, url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("Invalid cover URL".to_string());
    }

    let mut response = ureq::get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        )
        .header("Accept", "image/avif,image/webp,image/apng,image/*,*/*;q=0.8")
        .header("Referer", "https://vk.com/")
        .call()
        .map_err(|e| format!("Failed to download playlist cover: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Playlist cover HTTP {}",
            response.status().as_u16()
        ));
    }

    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read playlist cover: {e}"))?;

    if bytes.len() < 32 {
        return Err("Playlist cover is empty".to_string());
    }

    let safe_id = sanitized_playlist_id(playlist_id)?;
    let dir = PLAYLIST_COVER_DIR
        .get()
        .ok_or_else(|| "Playlist cover cache not initialized".to_string())?;

    clear_cached_playlist_covers(dir, &safe_id);
    let dest = dir.join(format!("{safe_id}.webp"));

    if is_gif_bytes(&bytes) {
        if bytes.len() as u64 > MAX_PLAYLIST_GIF_BYTES {
            return Err(format!(
                "GIF is too large (max {} MB)",
                MAX_PLAYLIST_GIF_BYTES / (1024 * 1024)
            ));
        }
        gif_bytes_to_webp(&bytes, &dest, PLAYLIST_COVER_SIZE)?;
        return Ok(dest.to_string_lossy().to_string());
    }

    let image =
        image::load_from_memory(&bytes).map_err(|e| format!("Failed to decode cover: {e}"))?;
    if !write_resized_webp(&image, &dest, PLAYLIST_COVER_SIZE) {
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

// ── Cover cache rebuild ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct CoverRebuildStats {
    pub cleared_files: u32,
    pub track_covers: u32,
    /// Unique full-size WebP images after dedup (c-*-full.webp).
    pub unique_images: u32,
    pub playlist_covers: u32,
    pub errors: u32,
}

fn clear_dir_files(dir: &Path) -> u32 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && fs::remove_file(&path).is_ok() {
            n += 1;
        }
    }
    n
}

/// Wipe track cover cache, re-extract from audio tags, convert playlist GIFs → WebP.
/// Mutates `playlist_cover_updates` with playlist_id → new cover path.
pub fn rebuild_cover_cache(
    track_paths: &[String],
    playlist_covers: &[(String, Option<String>)],
) -> Result<(CoverRebuildStats, Vec<(String, Option<String>)>), String> {
    let mut stats = CoverRebuildStats {
        cleared_files: 0,
        track_covers: 0,
        unique_images: 0,
        playlist_covers: 0,
        errors: 0,
    };

    if let Some(dir) = COVER_CACHE_DIR.get() {
        stats.cleared_files += clear_dir_files(dir);
        let _ = fs::create_dir_all(dir);
    }

    // Unique real audio paths (cue virtual paths → audio file).
    let mut unique: HashSet<PathBuf> = HashSet::new();
    for raw in track_paths {
        let path = if crate::cue::is_cue_track_path(raw) {
            crate::cue::parse_virtual_cue_path(raw)
                .map(|(audio, _)| PathBuf::from(audio))
                .unwrap_or_else(|| PathBuf::from(raw))
        } else {
            PathBuf::from(raw)
        };
        if path.is_file() {
            unique.insert(path);
        }
    }

    for path in &unique {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("track");
        let meta = read_metadata(path, file_name);
        if meta.cover_path.is_some() || meta.cover_path_full.is_some() {
            stats.track_covers += 1;
        }
    }

    // Count shared content images after rebuild.
    if let Some(dir) = COVER_CACHE_DIR.get() {
        if let Ok(entries) = fs::read_dir(dir) {
            stats.unique_images = entries
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("c-") && n.ends_with("-full.webp"))
                })
                .count() as u32;
        }
    }

    // Playlist covers: convert legacy gif/jpg/png → webp, refresh paths.
    let mut cover_updates: Vec<(String, Option<String>)> = Vec::new();
    let pl_dir = PLAYLIST_COVER_DIR.get();

    for (playlist_id, old_path) in playlist_covers {
        let Ok(safe_id) = sanitized_playlist_id(playlist_id) else {
            stats.errors += 1;
            cover_updates.push((playlist_id.clone(), old_path.clone()));
            continue;
        };

        let Some(dir) = pl_dir else {
            cover_updates.push((playlist_id.clone(), old_path.clone()));
            continue;
        };

        // Prefer existing file referenced by playlist; else look for any cached extension.
        let mut source: Option<PathBuf> = old_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.is_file());

        if source.is_none() {
            for ext in ["webp", "gif", "jpg", "jpeg", "png", "bmp"] {
                let candidate = dir.join(format!("{safe_id}.{ext}"));
                if candidate.is_file() {
                    source = Some(candidate);
                    break;
                }
            }
        }

        let Some(source) = source else {
            cover_updates.push((playlist_id.clone(), None));
            continue;
        };

        let dest = dir.join(format!("{safe_id}.webp"));
        let is_already_webp = source
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("webp"))
            && source == dest;

        let ok = if is_gif_path(&source) {
            gif_file_to_webp(&source, &dest, PLAYLIST_COVER_SIZE).is_ok()
        } else if is_already_webp {
            true
        } else {
            match image::open(&source) {
                Ok(img) => write_resized_webp(&img, &dest, PLAYLIST_COVER_SIZE),
                Err(_) => false,
            }
        };

        if ok && dest.is_file() {
            // Drop legacy non-webp siblings for this playlist.
            for ext in ["jpg", "jpeg", "gif", "png", "bmp"] {
                let legacy = dir.join(format!("{safe_id}.{ext}"));
                if legacy != dest && legacy.is_file() {
                    let _ = fs::remove_file(legacy);
                    stats.cleared_files += 1;
                }
            }
            stats.playlist_covers += 1;
            cover_updates.push((
                playlist_id.clone(),
                Some(dest.to_string_lossy().to_string()),
            ));
        } else {
            stats.errors += 1;
            cover_updates.push((playlist_id.clone(), old_path.clone()));
        }
    }

    Ok((stats, cover_updates))
}

/// Re-read cover paths for a track after cache rebuild (for playlists.json update).
pub fn fresh_cover_paths_for_track(track_path: &str) -> (Option<String>, Option<String>) {
    let path = if crate::cue::is_cue_track_path(track_path) {
        if let Some((audio, _)) = crate::cue::parse_virtual_cue_path(track_path) {
            PathBuf::from(audio)
        } else {
            PathBuf::from(track_path)
        }
    } else {
        PathBuf::from(track_path)
    };

    if !path.is_file() {
        return (None, None);
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("track");
    let meta = read_metadata(&path, file_name);
    (meta.cover_path, meta.cover_path_full)
}

fn cache_cover_bytes(audio_path: &Path, data: &[u8], mime: &str, suffix: &str) -> CoverPaths {
    // Content-addressed: identical embedded art (same album) → one pair of WebP files.
    let Some((content_id, paths)) = ensure_content_cover_files(data, mime) else {
        return CoverPaths::default();
    };
    write_track_cover_ref(audio_path, suffix, &content_id);
    paths
}

fn cache_cover_file(audio_path: &Path, source: &Path) -> CoverPaths {
    // Fast path via track ref if still valid and source hasn't changed.
    if let Some(content_id) = read_track_cover_ref(audio_path, "nearby") {
        let source_mtime = fs::metadata(source).and_then(|m| m.modified()).ok();
        let ref_path = track_cover_ref_path(audio_path, "nearby");
        let ref_mtime = ref_path
            .as_ref()
            .and_then(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
        let source_fresh = match (source_mtime, ref_mtime) {
            (Some(src), Some(r)) => src <= r,
            _ => true,
        };
        if source_fresh {
            if let Some(paths) = paths_for_content_id(&content_id) {
                return paths;
            }
        }
    }

    let Ok(data) = fs::read(source) else {
        return CoverPaths::default();
    };
    let mime = mime_from_path(source);
    cache_cover_bytes(audio_path, &data, mime, "nearby")
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
    // Track pointer → shared content file (no ID3 re-parse, no re-encode).
    let content_id = read_track_cover_ref(path, "embedded")?;
    paths_for_content_id(&content_id)
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

fn mime_from_image_bytes(data: &[u8], hint: Option<&str>) -> MimeType {
    if let Some(h) = hint {
        let h = h.to_ascii_lowercase();
        if h.contains("png") {
            return MimeType::Png;
        }
        if h.contains("gif") {
            return MimeType::Gif;
        }
        if h.contains("jpeg") || h.contains("jpg") {
            return MimeType::Jpeg;
        }
        if h.contains("webp") {
            return MimeType::Unknown("image/webp".into());
        }
        if h.contains("image/") {
            return MimeType::from_str(h.split(';').next().unwrap_or(h.as_str()).trim());
        }
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return MimeType::Jpeg;
    }
    if data.len() >= 8 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        return MimeType::Png;
    }
    if data.len() >= 6 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        return MimeType::Gif;
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return MimeType::Unknown("image/webp".into());
    }
    MimeType::Jpeg
}

/// Embed cover art into the file's primary tag (ID3 APIC / similar).
pub fn write_track_cover(path: &Path, data: &[u8], mime_hint: Option<&str>) -> Result<(), String> {
    if data.is_empty() {
        return Err("Empty cover image".to_string());
    }

    // Re-encode exotic formats (webp) to JPEG so more players accept the tag.
    let (bytes, mime) = match mime_from_image_bytes(data, mime_hint) {
        MimeType::Jpeg | MimeType::Png | MimeType::Gif | MimeType::Bmp | MimeType::Tiff => {
            (data.to_vec(), mime_from_image_bytes(data, mime_hint))
        }
        other => {
            // Try decode → jpeg
            match image::load_from_memory(data) {
                Ok(img) => {
                    let mut out = Vec::new();
                    let rgb = img.to_rgb8();
                    let mut cursor = std::io::Cursor::new(&mut out);
                    image::DynamicImage::ImageRgb8(rgb)
                        .write_to(&mut cursor, ImageFormat::Jpeg)
                        .map_err(|e| format!("Failed to re-encode cover: {e}"))?;
                    (out, MimeType::Jpeg)
                }
                Err(_) => (data.to_vec(), other),
            }
        }
    };

    let mut tagged_file = read_from_path(path)
        .map_err(|e| format!("Failed to read audio file for cover: {}", e))?;

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

    tag.remove_picture_type(PictureType::CoverFront);

    let picture = Picture::unchecked(bytes)
        .pic_type(PictureType::CoverFront)
        .mime_type(mime)
        .build();
    tag.push_picture(picture);

    tagged_file
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("Failed to save cover tag: {}", e))?;

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