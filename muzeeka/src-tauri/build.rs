use std::env;
use std::fs;
use std::path::PathBuf;

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
}