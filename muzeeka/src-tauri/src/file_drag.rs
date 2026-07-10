use std::path::{Path, PathBuf};

use tauri::Window;

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

fn canonical_file(path: &Path) -> Option<PathBuf> {
    if !path.is_file() {
        return None;
    }
    std::fs::canonicalize(path).ok().or_else(|| Some(path.to_path_buf()))
}

fn drag_preview_image(icon_path: Option<&str>, fallback: &Path) -> Result<drag::Image, String> {
    if let Some(icon) = icon_path {
        let path = PathBuf::from(icon);
        if is_image_path(&path) {
            if let Some(abs) = canonical_file(&path) {
                return Ok(drag::Image::File(abs));
            }
        }
    }

    if is_image_path(fallback) {
        if let Some(abs) = canonical_file(fallback) {
            return Ok(drag::Image::File(abs));
        }
    }

    Ok(drag::Image::Raw(
        include_bytes!("../icons/32x32.png").to_vec(),
    ))
}

/// Start a native OS drag with local file paths (e.g. drag to Telegram or Explorer).
#[tauri::command]
pub fn start_file_drag(
    window: Window,
    paths: Vec<String>,
    icon_path: Option<String>,
) -> Result<(), String> {
    let files: Vec<PathBuf> = paths
        .iter()
        .filter_map(|path| canonical_file(Path::new(path)))
        .collect();

    if files.is_empty() {
        return Err("File not found".into());
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