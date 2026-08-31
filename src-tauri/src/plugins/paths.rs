use std::path::PathBuf;

use tauri::{AppHandle, Manager};

/// One plugins folder. Drop a directory with `plugin.json` in here.
///
/// Dev uses the repo `plugins/` so you can open it in the tree.
/// A packaged build uses `plugins/` next to the exe.
pub fn plugins_dir(app: &AppHandle) -> PathBuf {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("plugins");
    if repo.is_dir() {
        return dunce_canonicalize(&repo);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let dir = parent.join("plugins");
            let _ = std::fs::create_dir_all(&dir);
            return dir;
        }
    }

    if let Ok(resource) = app.path().resource_dir() {
        let bundled = resource.join("plugins");
        let _ = std::fs::create_dir_all(&bundled);
        return bundled;
    }

    repo
}

pub fn plugin_data_dir(app: &AppHandle, plugin_id: &str) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {e}"))?
        .join("plugin-data")
        .join(plugin_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create plugin data dir: {e}"))?;
    Ok(dir)
}

pub fn registry_state_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create app data dir: {e}"))?;
    Ok(dir.join("plugins.json"))
}

fn dunce_canonicalize(path: &std::path::Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(p) => {
            let raw = p.to_string_lossy();
            PathBuf::from(raw.strip_prefix(r"\\?\").unwrap_or(raw.as_ref()))
        }
        Err(_) => path.to_path_buf(),
    }
}
