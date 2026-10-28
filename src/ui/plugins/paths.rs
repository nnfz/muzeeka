use std::path::PathBuf;

use crate::ui::catalog;

pub fn plugins_dir() -> PathBuf {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins");
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

    repo
}

pub fn plugin_data_dir(plugin_id: &str) -> Result<PathBuf, String> {
    let dir = catalog::app_data_dir().join("plugin-data").join(plugin_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create plugin data dir: {e}"))?;
    Ok(dir)
}

pub fn registry_state_path() -> Result<PathBuf, String> {
    let dir = catalog::app_data_dir();
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
