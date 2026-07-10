use std::path::{Path, PathBuf};

use tauri::WebviewWindow;

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "ico"
            )
        })
}

fn strip_cue_marker(path_str: &str) -> &str {
    const CUE_MARKER: &str = "#cue:";
    path_str
        .split_once(CUE_MARKER)
        .map(|(base, _)| base)
        .unwrap_or(path_str)
}

fn resolve_drag_file(path_str: &str) -> Option<PathBuf> {
    let trimmed = strip_cue_marker(path_str.trim());
    if trimmed.is_empty() {
        return None;
    }

    let path = PathBuf::from(trimmed);
    if !path.is_file() {
        return None;
    }

    std::fs::canonicalize(&path).ok().or(Some(path))
}

fn canonical_image(path_str: &str) -> Option<PathBuf> {
    let path = PathBuf::from(path_str);
    if !is_image_path(&path) || !path.is_file() {
        return None;
    }
    std::fs::canonicalize(&path).ok().or(Some(path))
}

fn drag_preview_image(icon_path: Option<&str>, fallback: &Path) -> Result<drag::Image, String> {
    if let Some(icon) = icon_path {
        if let Some(abs) = canonical_image(icon) {
            return Ok(drag::Image::File(abs));
        }
    }

    if is_image_path(fallback) {
        if let Ok(abs) = std::fs::canonicalize(fallback) {
            return Ok(drag::Image::File(abs));
        }
    }

    Ok(drag::Image::Raw(
        include_bytes!("../icons/32x32.png").to_vec(),
    ))
}

/// Start a native OS drag with local file paths (e.g. drag to Telegram).
#[tauri::command]
pub fn start_file_drag(
    window: WebviewWindow,
    paths: Vec<String>,
    icon_path: Option<String>,
) -> Result<(), String> {
    let files: Vec<PathBuf> = paths
        .iter()
        .filter_map(|path| resolve_drag_file(path))
        .collect();

    if files.is_empty() {
        return Err(format!(
            "File not found: {}",
            paths.first().map(String::as_str).unwrap_or("")
        ));
    }

    let item = drag::DragItem::Files(files.clone());
    let image = drag_preview_image(icon_path.as_deref(), &files[0])?;

    drag::start_drag(
        &window,
        item,
        image,
        |_result, _pos| {},
        drag::Options::default(),
    )
    .map_err(|e| e.to_string())
}