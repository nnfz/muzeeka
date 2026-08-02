// Audio metadata reader — ID3, Vorbis, FLAC, MP4, etc. via lofty.
// Falls back to the `id3` crate for MP3 files with tricky unsynchronisation tags.

use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, TaggedFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::read_from_path;
use lofty::tag::{Accessor, Tag, TagType};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use id3::TagLike;

static COVER_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
static PLAYLIST_COVER_DIR: OnceLock<PathBuf> = OnceLock::new();
/// Bundled ffmpeg binary (for GIF → animated WebP). Set once at app startup.
static FFMPEG_BIN: OnceLock<Option<PathBuf>> = OnceLock::new();

const PLAYLIST_COVER_SIZE: u32 = 256;
const MAX_PLAYLIST_GIF_BYTES: u64 = 20 * 1024 * 1024;

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff"];
const COVER_NAMES: &[&str] = &[
    "cover", "folder", "front", "album", "albumart", "artwork", "albumartsmall",
];
/// List / transport cover.
const THUMB_SIZE: u32 = 96;
/// Fullscreen cover — capped so retina UI stays sharp without multi‑MB assets.
/// (Old unlimited lossless dumps hit 10–15MB and felt like “cover never loads”.)
const FULL_SIZE: u32 = 720;
/// Soft size budget for full covers. Lossless 720 WebP of detailed art often
/// lands at 900KB–1MB; we used to *delete* those and fall back to 96px thumbs
/// in fullscreen. Full covers are now lossy JPEG so they stay under this.
const MAX_FULL_CACHE_BYTES: u64 = 800 * 1024;
/// Prefer ffmpeg downscale for huge embeds (e.g. 4500² / 7MB APIC) so we never
/// hold an 80MB+ RGBA buffer on the cover-resolve path (UI freeze while audio plays).
const HUGE_COVER_BYTES: usize = 512 * 1024;

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
    /// Content-addressed cover id (no directory, no suffix). Prefer this in SQLite.
    pub cover_id: Option<String>,
}

/// Content-addressed cover pair written under the covers/ cache directory.
#[derive(Debug, Clone, Default)]
pub struct CoverPaths {
    pub thumb: Option<String>,
    pub full: Option<String>,
    /// Stable content hash (`c-{id}-thumb.webp` / `c-{id}-full.jpg`). Stored in SQLite.
    pub id: Option<String>,
}

/// Initialize the on-disk cover art cache under the app data directory.
pub fn init_cover_cache(app_data_dir: PathBuf) {
    let covers = app_data_dir.join("covers");
    let _ = fs::create_dir_all(&covers);
    let _ = COVER_CACHE_DIR.set(covers);

    let playlist_covers = app_data_dir.join("playlist_covers");
    let _ = fs::create_dir_all(&playlist_covers);
    let _ = PLAYLIST_COVER_DIR.set(playlist_covers);

    // Legacy: per-track `t-*.ref` pointers. Track→cover mapping lives in SQLite now.
    purge_legacy_track_refs();
}

/// Delete obsolete `t-{hash}-*.ref` sidecars (path→content_id). Safe to call repeatedly.
fn purge_legacy_track_refs() {
    let Some(dir) = COVER_CACHE_DIR.get() else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with("t-") && name.ends_with(".ref") {
            let _ = fs::remove_file(entry.path());
        }
    }
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
        Some("tif") | Some("tiff") => "image/tiff",
        _ => "image/jpeg",
    }
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
    } else if data.len() >= 4
        && (data[..4] == *b"II*\0" || data[..4] == *b"MM\0*")
    {
        // TIFF little-endian (II) or big-endian (MM)
        "image/tiff".to_string()
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

/// Preferred on-disk path for newly written full covers (lossy JPEG).
fn content_full_path(content_id: &str) -> Option<PathBuf> {
    let cache_dir = COVER_CACHE_DIR.get()?;
    Some(cache_dir.join(format!("c-{content_id}-full.jpg")))
}

/// Full cover candidates: new JPEG first, then legacy lossless WebP.
fn content_full_candidates(content_id: &str) -> Vec<PathBuf> {
    let Some(cache_dir) = COVER_CACHE_DIR.get() else {
        return Vec::new();
    };
    vec![
        cache_dir.join(format!("c-{content_id}-full.jpg")),
        cache_dir.join(format!("c-{content_id}-full.jpeg")),
        cache_dir.join(format!("c-{content_id}-full.webp")),
    ]
}

fn find_ok_full(content_id: &str) -> Option<PathBuf> {
    content_full_candidates(content_id)
        .into_iter()
        .find(|p| full_cache_is_ok(p))
}

fn find_any_full(content_id: &str) -> Option<PathBuf> {
    content_full_candidates(content_id)
        .into_iter()
        .find(|p| p.is_file() && fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false))
}

fn remove_other_fulls(content_id: &str, keep: &Path) {
    for path in content_full_candidates(content_id) {
        if path != keep && path.is_file() {
            let _ = fs::remove_file(path);
        }
    }
}

fn full_cache_is_ok(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(m) => m.is_file() && m.len() > 0 && m.len() <= MAX_FULL_CACHE_BYTES,
        Err(_) => false,
    }
}

/// Ensure content-addressed full + thumb exist for this image payload.
/// Full is always max FULL_SIZE (never dump multi‑MB originals).
fn ensure_content_cover_files(data: &[u8], _mime: &str) -> Option<(String, CoverPaths)> {
    if data.is_empty() {
        return None;
    }
    let content_id = cover_content_id(data);
    let thumb_path = content_thumb_path(&content_id)?;

    let thumb_ok = thumb_path.is_file();
    let full_ok = find_ok_full(&content_id).is_some();

    // Shared by every track with this APIC — only when both are usable.
    if thumb_ok && full_ok {
        let full_path = find_ok_full(&content_id)?;
        return Some((
            content_id.clone(),
            CoverPaths {
                thumb: Some(thumb_path.to_string_lossy().to_string()),
                full: Some(full_path.to_string_lossy().to_string()),
                id: Some(content_id),
            },
        ));
    }

    if !full_ok {
        // Prefer recompressing an existing oversized 720 full (legacy lossless WebP)
        // over re-decoding a multi‑MB APIC from the audio file.
        let recompressed = find_any_full(&content_id)
            .and_then(|oversized| image::open(oversized).ok())
            .and_then(|img| write_full_cover_image(&img, &content_id))
            .is_some();
        if !recompressed {
            // Huge embeds (4500² FLAC pictures): ffmpeg scale avoids multi‑second RGBA freezes.
            // Don't abort the whole cover set if full encode fails — thumb alone still works.
            let _ = write_full_cover_from_bytes(data, &content_id);
        }
    }

    if !thumb_path.is_file() {
        // Prefer cheap path: scale from the full we just wrote when present.
        let wrote_thumb = find_any_full(&content_id)
            .and_then(|full| image::open(full).ok())
            .map(|img| write_thumbnail_from_image(&img, &thumb_path))
            .unwrap_or(false);
        if !wrote_thumb {
            let image = decode_image_bytes(data)?;
            if !write_thumbnail_from_image(&image, &thumb_path) {
                return None;
            }
        }
    }

    if !thumb_path.is_file() {
        return None;
    }

    let full = find_ok_full(&content_id)
        .or_else(|| find_any_full(&content_id))
        .map(|p| p.to_string_lossy().to_string());

    Some((
        content_id.clone(),
        CoverPaths {
            thumb: Some(thumb_path.to_string_lossy().to_string()),
            full,
            id: Some(content_id),
        },
    ))
}

fn decode_image_bytes(data: &[u8]) -> Option<image::DynamicImage> {
    image::load_from_memory(data).ok()
}

/// Build a ≤FULL_SIZE full cover under the size budget (lossy JPEG).
fn write_full_cover_from_bytes(data: &[u8], content_id: &str) -> Option<PathBuf> {
    // Fast path for multi‑MB APIC / cover.jpg: ffmpeg resizes without a full RGBA buffer.
    if data.len() >= HUGE_COVER_BYTES {
        if let Some(ffmpeg) = ffmpeg_bin() {
            if let Some(dest) = content_full_path(content_id) {
                if encode_full_cover_ffmpeg(data, &dest, ffmpeg) && full_cache_is_ok(&dest) {
                    remove_other_fulls(content_id, &dest);
                    return Some(dest);
                }
            }
        }
    }

    let image = decode_image_bytes(data)?;
    write_full_cover_image(&image, content_id)
}

fn write_full_cover_image(image: &image::DynamicImage, content_id: &str) -> Option<PathBuf> {
    let dest = content_full_path(content_id)?;
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let (width, height) = image.dimensions();
    let resized = if width <= FULL_SIZE && height <= FULL_SIZE {
        image.clone()
    } else {
        image.resize(FULL_SIZE, FULL_SIZE, FilterType::Triangle)
    };

    // Quality ladder until we fit the soft budget (detailed 720 art as lossless WebP did not).
    for quality in [88_u8, 78, 68, 55] {
        if write_jpeg(&resized, &dest, quality) && full_cache_is_ok(&dest) {
            remove_other_fulls(content_id, &dest);
            return Some(dest);
        }
    }

    // Keep whatever we produced rather than leaving fullscreen on a 96px thumb.
    if dest.is_file() && fs::metadata(&dest).map(|m| m.len() > 0).unwrap_or(false) {
        remove_other_fulls(content_id, &dest);
        return Some(dest);
    }
    None
}

fn write_jpeg(image: &image::DynamicImage, dest: &Path, quality: u8) -> bool {
    use image::codecs::jpeg::JpegEncoder;
    use std::io::BufWriter;

    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let rgb = image.to_rgb8();
    let Ok(file) = fs::File::create(dest) else {
        return false;
    };
    let mut encoder = JpegEncoder::new_with_quality(BufWriter::new(file), quality);
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .is_ok()
}

fn encode_full_cover_ffmpeg(data: &[u8], dest: &Path, ffmpeg: &Path) -> bool {
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let tmp_in = dest.with_extension("src-tmp");
    if fs::write(&tmp_in, data).is_err() {
        return false;
    }

    // Even dimensions help some JPEG encoders; scale longer side ≤ FULL_SIZE.
    let vf = format!(
        "scale='min({FULL_SIZE},iw)':'min({FULL_SIZE},ih)':force_original_aspect_ratio=decrease"
    );

    let mut cmd = Command::new(ffmpeg);
    configure_ffmpeg_command(&mut cmd);
    let ok = cmd
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
        ])
        .arg(&tmp_in)
        .args(["-vf", &vf, "-frames:v", "1", "-q:v", "3"])
        .arg(dest)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let _ = fs::remove_file(&tmp_in);
    ok && dest.is_file()
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
    for ext in ["jpg", "jpeg", "gif", "png", "webp", "bmp", "tif", "tiff"] {
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
        match convert_gif_to_webp_ffmpeg(source_gif, dest_webp, ffmpeg, max_edge) {
            Ok(()) if dest_webp.is_file() => {
                eprintln!(
                    "[cover] GIF → animated WebP OK: {}",
                    source_gif.display()
                );
                return Ok(());
            }
            Ok(()) => {
                eprintln!(
                    "[cover] ffmpeg finished but WebP missing, falling back to first frame: {}",
                    source_gif.display()
                );
            }
            Err(e) => {
                eprintln!(
                    "[cover] ffmpeg GIF→WebP failed ({}), falling back to first frame: {}",
                    e,
                    source_gif.display()
                );
            }
        }
    } else {
        eprintln!(
            "[cover] ffmpeg not available, GIF will lose animation: {}",
            source_gif.display()
        );
    }

    // Still fallback (first frame) when ffmpeg is missing or conversion fails.
    let image = image::open(source_gif).map_err(|e| format!("Failed to open GIF: {e}"))?;
    if !write_resized_webp(&image, dest_webp, max_edge) {
        return Err("Failed to write still WebP from GIF".to_string());
    }
    Ok(())
}

fn is_webp_file(path: &Path) -> bool {
    let Ok(data) = fs::read(path) else {
        return false;
    };
    // RIFF....WEBP
    data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP"
}

/// True if the WebP contains an animation (ANIM chunk). Still WebP is OK for
/// single-frame GIFs, but multi-frame GIF→still means conversion lost animation.
fn is_animated_webp(path: &Path) -> bool {
    let Ok(data) = fs::read(path) else {
        return false;
    };
    // Chunk fourCCs are ASCII; a simple search is enough for our small covers.
    data.windows(4).any(|w| w == b"ANIM")
}

fn configure_ffmpeg_command(cmd: &mut Command) {
    crate::process_util::hide_console(cmd);
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

    // Scale longer side ≤ max_edge, force even dims (needed for yuva420p), keep alpha.
    // format=yuva420p avoids the slower RGB→YUV path inside libwebp for lossy encode.
    let vf = format!(
        "scale='min({max_edge},iw)':'min({max_edge},ih)':force_original_aspect_ratio=decrease:flags=lanczos,scale=trunc(iw/2)*2:trunc(ih/2)*2:flags=lanczos,format=yuva420p"
    );

    // Prefer the dedicated animated encoder; fall back to libwebp / auto-select.
    // Note: modern ffmpeg (post-vsync removal) needs -fps_mode, not -vsync.
    let attempts: &[(&str, &[&str])] = &[
        (
            "libwebp_anim",
            &[
                "-c:v",
                "libwebp_anim",
                "-lossless",
                "0",
                "-quality",
                "80",
                "-compression_level",
                "4",
                "-loop",
                "0",
                "-an",
                "-fps_mode",
                "passthrough",
            ],
        ),
        (
            "libwebp",
            &[
                "-c:v",
                "libwebp",
                "-lossless",
                "0",
                "-quality",
                "80",
                "-compression_level",
                "4",
                "-loop",
                "0",
                "-an",
                "-fps_mode",
                "passthrough",
            ],
        ),
        (
            "auto",
            &[
                "-lossless",
                "0",
                "-quality",
                "80",
                "-loop",
                "0",
                "-an",
                "-fps_mode",
                "passthrough",
            ],
        ),
        // Last resort for older ffmpeg without -fps_mode / -quality on this path.
        (
            "libwebp_anim-legacy",
            &[
                "-c:v",
                "libwebp_anim",
                "-lossless",
                "0",
                "-q:v",
                "80",
                "-loop",
                "0",
                "-an",
            ],
        ),
    ];

    let mut last_err = String::from("no encoder attempts ran");

    for (name, extra) in attempts {
        let _ = fs::remove_file(dest_webp);

        let mut cmd = Command::new(ffmpeg);
        configure_ffmpeg_command(&mut cmd);
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]);
        cmd.arg(source_gif);
        cmd.args(["-vf", &vf]);
        cmd.args(*extra);
        cmd.arg(dest_webp);

        let output = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                last_err = format!("Failed to run ffmpeg for WebP: {e}");
                eprintln!("[cover] ffmpeg spawn failed ({name}): {last_err}");
                continue;
            }
        };

        if output.status.success() && is_webp_file(dest_webp) {
            let animated = is_animated_webp(dest_webp);
            eprintln!(
                "[cover] ffmpeg GIF→WebP OK codec={name} animated={animated}: {}",
                source_gif.display()
            );
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        last_err = format!(
            "codec={name} exit={} stderr={}",
            output.status,
            stderr.trim()
        );
        eprintln!("[cover] ffmpeg GIF→WebP attempt failed: {last_err}");
        let _ = fs::remove_file(dest_webp);
    }

    Err(format!("ffmpeg GIF→WebP conversion failed: {last_err}"))
}

fn gif_bytes_to_webp(gif_bytes: &[u8], dest_webp: &Path, max_edge: u32) -> Result<(), String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "muzeeka-cover-{}-{nanos}.gif",
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

    // Count shared content images after rebuild (JPEG fulls + legacy WebP).
    if let Some(dir) = COVER_CACHE_DIR.get() {
        if let Ok(entries) = fs::read_dir(dir) {
            stats.unique_images = entries
                .flatten()
                .filter(|e| {
                    e.file_name().to_str().is_some_and(|n| {
                        n.starts_with("c-")
                            && (n.ends_with("-full.jpg")
                                || n.ends_with("-full.jpeg")
                                || n.ends_with("-full.webp"))
                    })
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

/// Re-read cover paths for a track after rebuilding the library cover cache.
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

fn cache_cover_bytes(data: &[u8], mime: &str) -> CoverPaths {
    // Content-addressed: identical APIC/cover bytes (same album) → one image pair.
    // SQLite stores only `cover_id`; thumb/full paths are reconstructed at read time.
    match ensure_content_cover_files(data, mime) {
        Some((id, mut paths)) => {
            paths.id = Some(id);
            paths
        }
        None => CoverPaths::default(),
    }
}

/// Parse `c-{16 hex}-(thumb|full).*` → content id. Also accepts a bare 16-hex id.
pub fn cover_id_from_cache_path(path: &str) -> Option<String> {
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path.trim());
    let lower = name.to_ascii_lowercase();
    if lower.len() == 16 && lower.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(lower);
    }
    // c-<id>-thumb.webp / c-<id>-full.jpg
    let rest = lower.strip_prefix("c-")?;
    let (id, suffix) = rest.split_once('-')?;
    if id.len() == 16
        && id.chars().all(|c| c.is_ascii_hexdigit())
        && (suffix.starts_with("thumb.") || suffix.starts_with("full."))
    {
        return Some(id.to_string());
    }
    // Full path containing .../c-<id>-thumb.webp
    if let Some(pos) = lower.rfind("c-") {
        let slice = &lower[pos + 2..];
        if slice.len() >= 16 {
            let id = &slice[..16];
            if id.chars().all(|c| c.is_ascii_hexdigit()) {
                let after = &slice[16..];
                if after.starts_with("-thumb.") || after.starts_with("-full.") {
                    return Some(id.to_string());
                }
            }
        }
    }
    None
}

pub fn cover_id_from_paths(thumb: Option<&str>, full: Option<&str>) -> Option<String> {
    thumb
        .and_then(cover_id_from_cache_path)
        .or_else(|| full.and_then(cover_id_from_cache_path))
}

/// Expand a stored cover_id into on-disk thumb/full paths (files may be missing).
pub fn cover_paths_for_id(cover_id: &str) -> CoverPaths {
    let id = cover_id.trim().to_ascii_lowercase();
    if id.len() != 16 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return CoverPaths::default();
    }
    let thumb = content_thumb_path(&id).filter(|p| p.is_file());
    let full = find_ok_full(&id).or_else(|| find_any_full(&id));
    CoverPaths {
        thumb: thumb.map(|p| p.to_string_lossy().to_string()),
        full: full
            .filter(|p| !is_thumb_cache_path(&p.to_string_lossy()))
            .map(|p| p.to_string_lossy().to_string()),
        id: Some(id),
    }
}

fn cache_cover_file(source: &Path) -> CoverPaths {
    let Ok(data) = fs::read(source) else {
        return CoverPaths::default();
    };
    let mime = mime_from_path(source);
    cache_cover_bytes(&data, mime)
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
            let paths = cache_cover_bytes(data, &mime);
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

    let paths = cache_cover_bytes(&pic.data, &mime);
    if paths.thumb.is_some() || paths.full.is_some() {
        Some(paths)
    } else {
        None
    }
}

fn extract_nearby_cover(path: &Path) -> CoverPaths {
    let source = match find_nearby_cover(path) {
        Some(source) => source,
        None => return CoverPaths::default(),
    };
    cache_cover_file(&source)
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

/// True when `path` is a list thumb, not a fullscreen full.
pub fn is_thumb_cache_path(path: &str) -> bool {
    path.to_ascii_lowercase().contains("-thumb.")
}

/// Extract / cache covers from an audio file (no DB). Prefer DB lookup at the command layer.
pub fn resolve_list_cover(path: &Path) -> Option<String> {
    let paths = extract_covers_for_file(path);
    paths.thumb.or(paths.full)
}

/// Extract / cache full cover from an audio file (no DB). Prefer DB lookup at the command layer.
pub fn resolve_full_cover(path: &Path) -> Option<String> {
    let paths = extract_covers_for_file(path);
    paths
        .full
        .filter(|p| !is_thumb_cache_path(p))
}

/// Read tags if needed and write content-addressed cover files. Returns thumb + full paths.
pub fn extract_covers_for_file(path: &Path) -> CoverPaths {
    let tagged_file = read_from_path(path).ok();
    match tagged_file.as_ref() {
        Some(tagged_file) => resolve_cover_paths(path, Some(tagged_file)),
        None => resolve_cover_paths(path, None),
    }
}

/// Read a cover image from disk and return a data URL (for paths outside the asset scope).
pub fn cover_data_url(path: &Path) -> Result<Option<String>, String> {
    if !path.is_file() {
        return Ok(None);
    }

    // Never base64 multi‑MB raw cover.jpg into the WebView (freezes the UI).
    let meta = fs::metadata(path).map_err(|e| format!("Failed to stat cover: {e}"))?;
    if meta.len() > MAX_FULL_CACHE_BYTES * 2 {
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
    read_metadata_impl(path, file_name, true)
}

/// Read only tags and audio properties. Cover extraction is deliberately skipped
/// during imports; visible-player components resolve covers lazily on demand.
pub fn read_metadata_fast(path: &Path, file_name: &str) -> TrackMetadata {
    read_metadata_impl(path, file_name, false)
}

fn read_metadata_impl(path: &Path, file_name: &str, resolve_covers: bool) -> TrackMetadata {
    let mut meta = TrackMetadata::default();

    let tagged_file = if resolve_covers {
        read_from_path(path)
    } else {
        Probe::open(path).and_then(|probe| {
            probe
                .options(ParseOptions::new().read_cover_art(false))
                .read()
        })
    };

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

            if resolve_covers {
                let covers = resolve_cover_paths(path, Some(tagged_file));
                meta.cover_path = covers.thumb;
                meta.cover_path_full = covers.full;
                meta.cover_id = covers.id;
            }
        }
        Err(_) => {
            meta.title = Some(strip_ytdlp_id_suffix(&filename_stem(path, file_name)));
            if resolve_covers {
                let covers = resolve_cover_paths(path, None);
                meta.cover_path = covers.thumb;
                meta.cover_path_full = covers.full;
                meta.cover_id = covers.id;
            }
        }
    }

    if meta.title.is_none() {
        meta.title = Some(strip_ytdlp_id_suffix(&filename_stem(path, file_name)));
    }

    meta
}

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

    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
    if ext.as_deref() == Some("mp3") {
        let mut tag = id3::Tag::read_from_path(path).unwrap_or_else(|_| id3::Tag::new());
        if let Some(t) = title { tag.set_title(t); }
        if let Some(a) = artist { tag.set_artist(a); }
        tag.write_to_path(path, id3::Version::Id3v23).map_err(|e| format!("id3 save error: {}", e))?;
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

pub fn write_track_cover(path: &Path, data: &[u8], mime_hint: Option<&str>) -> Result<(), String> {
    if data.is_empty() {
        return Err("Empty cover image".to_string());
    }

    let (bytes, mime) = match mime_from_image_bytes(data, mime_hint) {
        MimeType::Jpeg | MimeType::Png | MimeType::Gif | MimeType::Bmp | MimeType::Tiff => {
            (data.to_vec(), mime_from_image_bytes(data, mime_hint))
        }
        other => {
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

    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
    if ext.as_deref() == Some("mp3") {
        let mut tag = id3::Tag::read_from_path(path).unwrap_or_else(|_| id3::Tag::new());
        tag.remove_picture_by_type(id3::frame::PictureType::CoverFront);
        
        let mime_str = match mime {
            MimeType::Png => "image/png",
            MimeType::Gif => "image/gif",
            MimeType::Bmp => "image/bmp",
            MimeType::Tiff => "image/tiff",
            MimeType::Jpeg => "image/jpeg",
            MimeType::Unknown(ref s) => s.as_str(),
            _ => "image/jpeg",
        }.to_string();

        tag.add_frame(id3::frame::Picture {
            mime_type: mime_str,
            picture_type: id3::frame::PictureType::CoverFront,
            description: String::new(),
            data: bytes,
        });
        tag.write_to_path(path, id3::Version::Id3v23).map_err(|e| format!("id3 save cover error: {}", e))?;
        return Ok(());
    }

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
