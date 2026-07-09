use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn copy_bin_tree(src: &Path, dst: &Path) -> usize {
    if !src.is_dir() {
        return 0;
    }

    let _ = fs::create_dir_all(dst);
    let mut copied = 0usize;

    let entries = match fs::read_dir(src) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().eq_ignore_ascii_case("readme.md") {
            continue;
        }

        let target = dst.join(&name);
        if path.is_dir() {
            copied += copy_bin_tree(&path, &target);
        } else if fs::copy(&path, &target).is_ok() {
            copied += 1;
        }
    }

    copied
}

fn main() {
    tauri_build::build();

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let bass_src = manifest_dir.join("bass");
    if !bass_src.join("bass.dll").is_file() {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let profile_dir = out_dir
        .join("../../../")
        .canonicalize()
        .unwrap_or_else(|_| out_dir.join("../../../"));
    let bass_dst = profile_dir.join("bass");

    let _ = fs::create_dir_all(&bass_dst);
    let mut copied = 0usize;

    if let Ok(entries) = fs::read_dir(&bass_src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("dll")) {
                let dst = bass_dst.join(entry.file_name());
                if fs::copy(&path, &dst).is_ok() {
                    copied += 1;
                }
            }
        }
    }

    println!("cargo:rerun-if-changed={}", bass_src.display());
    println!(
        "cargo:warning=Copied {copied} BASS DLL(s) to {}",
        bass_dst.display()
    );

    // Copy bundled tools (yt-dlp, ffmpeg, etc.) next to the executable.
    let bin_src = manifest_dir.join("bin");
    let bin_dst = profile_dir.join("bin");

    if bin_src.is_dir() {
        let copied_bin = copy_bin_tree(&bin_src, &bin_dst);
        if copied_bin > 0 {
            println!(
                "cargo:warning=Copied {copied_bin} file(s) from bin/ to {}",
                bin_dst.display()
            );
        }
    }

    println!("cargo:rerun-if-changed={}", bin_src.display());
}