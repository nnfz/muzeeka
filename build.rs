use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    let icon = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("src")
        .join("icons")
        .join("icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    let rc_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("muzeeka-icon.rc");
    let icon_rc = icon.to_string_lossy().replace('\\', "/");
    // Resource id 1 is what GPUI loads for the window class and the taskbar.
    fs::write(&rc_path, format!("1 ICON \"{icon_rc}\"\n")).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
