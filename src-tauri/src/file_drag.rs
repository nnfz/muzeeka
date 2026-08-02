use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::ImageFormat;
use tauri::{Manager, Window};

use crate::drop_handler::{ExportDragContext, ExportDragState};
use crate::metadata;

/// Built-in drag-ghost fallback (compiled into the binary once).
static APP_ICON_PNG: &[u8] = include_bytes!("../icons/32x32.png");

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "ico" | "tif" | "tiff"
            )
        })
}

fn canonical_file(path: &Path) -> Option<PathBuf> {
    if !path.is_file() {
        return None;
    }
    std::fs::canonicalize(path)
        .ok()
        .or_else(|| Some(path.to_path_buf()))
}

/// Ghost size under the cursor (px). Shell drag previews look huge above ~64.
const DRAG_GHOST_SIZE: u32 = 48;

/// Encode an on-disk image as PNG bytes for the Windows drag ghost.
///
/// Covers are stored as WebP; WIC (used by the `drag` crate) often fails to
/// decode WebP → empty ghost → shell falls back to the app icon. Re-encoding
/// through the `image` crate fixes that.
fn image_path_to_png_bytes(path: &Path) -> Option<Vec<u8>> {
    let path = canonical_file(path)?;
    let img = image::open(&path).ok()?;
    // Exact small square — `thumbnail` only shrinks if larger, still can leave
    // long sides big; resize forces a compact OS drag icon.
    let thumb = img.resize_exact(
        DRAG_GHOST_SIZE,
        DRAG_GHOST_SIZE,
        image::imageops::FilterType::Triangle,
    );
    let mut buf = Vec::new();
    thumb
        .write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .ok()?;
    if buf.is_empty() {
        return None;
    }
    Some(buf)
}

/// Build the OS drag preview: prefer track cover (any format we can decode),
/// then embedded art from the audio file, then app icon.
fn drag_preview_image(icon_path: Option<&str>, audio_file: &Path) -> drag::Image {
    // 1) Explicit cover path from the UI (thumb or full).
    if let Some(icon) = icon_path.map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(icon);
        if path.is_file() {
            if let Some(png) = image_path_to_png_bytes(&path) {
                return drag::Image::Raw(png);
            }
        }
    }

    // 2) Resolve / extract cover from the audio file itself.
    if let Some(cover) = metadata::resolve_list_cover(audio_file) {
        let path = PathBuf::from(&cover);
        if let Some(png) = image_path_to_png_bytes(&path) {
            return drag::Image::Raw(png);
        }
    }
    if let Some(cover) = metadata::resolve_full_cover(audio_file) {
        let path = PathBuf::from(&cover);
        if let Some(png) = image_path_to_png_bytes(&path) {
            return drag::Image::Raw(png);
        }
    }

    // 3) If the file itself is an image (rare), use it.
    if is_image_path(audio_file) {
        if let Some(png) = image_path_to_png_bytes(audio_file) {
            return drag::Image::Raw(png);
        }
    }

    drag::Image::Raw(APP_ICON_PNG.to_vec())
}

/// Ensures `finish_export` runs on every exit path after `register_export`.
struct FinishExportGuard<'a> {
    state: &'a ExportDragState,
}

impl Drop for FinishExportGuard<'_> {
    fn drop(&mut self) {
        self.state.finish_export();
    }
}

/// Start a native OS drag with local file paths (e.g. drag to Telegram or Explorer).
///
/// `drag::start_drag` blocks until the OS drag ends — run it on the blocking pool
/// so a long drag does not pin a Tauri command worker.
#[tauri::command]
pub async fn start_file_drag(
    window: Window,
    paths: Vec<String>,
    icon_path: Option<String>,
    track_paths: Option<Vec<String>>,
    source_playlist_id: Option<String>,
    is_copy: Option<bool>,
) -> Result<(), String> {
    let files: Vec<PathBuf> = paths
        .iter()
        .filter_map(|path| canonical_file(Path::new(path)))
        .collect();

    if files.is_empty() {
        return Err("File not found".into());
    }

    tauri::async_runtime::spawn_blocking(move || {
        let export_state = window.state::<ExportDragState>();
        let context = track_paths
            .filter(|tracks| !tracks.is_empty())
            .map(|track_paths| ExportDragContext {
                track_paths,
                source_playlist_id,
                is_copy: is_copy.unwrap_or(false),
            });
        export_state.register_export(&files, context);
        // Always clear suppress state — even if preview/start_drag fails later.
        let _finish = FinishExportGuard {
            state: &export_state,
        };

        let item = drag::DragItem::Files(files.clone());
        let image = drag_preview_image(icon_path.as_deref(), &files[0]);

        drag::start_drag(
            &window,
            item,
            image,
            |_result, _pos| {},
            drag::Options::default(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("Drag task failed: {e}"))?
}
