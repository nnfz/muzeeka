use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BASS_CORE_URL: &str = "https://www.un4seen.com/files/bass24.zip";
const BASS_DOWNLOADS: &[(&str, &str)] = &[
    ("bass_fx.dll", "https://www.un4seen.com/files/z/0/bass_fx24.zip"),
    ("bassalac.dll", "https://www.un4seen.com/files/bassalac24.zip"),
    ("bassape.dll", "https://www.un4seen.com/files/bassape24.zip"),
    ("bassflac.dll", "https://www.un4seen.com/files/bassflac24.zip"),
    ("basshls.dll", "https://www.un4seen.com/files/basshls24.zip"),
    ("bassmidi.dll", "https://www.un4seen.com/files/bassmidi24.zip"),
    ("bassmix.dll", "https://www.un4seen.com/files/bassmix24.zip"),
    ("bassopus.dll", "https://www.un4seen.com/files/bassopus24.zip"),
    ("basswasapi.dll", "https://www.un4seen.com/files/basswasapi24.zip"),
    ("basswma.dll", "https://www.un4seen.com/files/basswm24.zip"),
    ("basswv.dll", "https://www.un4seen.com/files/basswv24.zip"),
];

const TOOL_DOWNLOADS: &[(&str, &str)] = &[
    ("yt-dlp.exe", "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"),
    ("spotdl.exe", "github_win32:spotDL/spotify-downloader"),
];

const FFMPEG_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

fn run_powershell(script: &str) -> Result<(), String> {
    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .status()
        .map_err(|error| format!("Failed to start PowerShell: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("PowerShell exited with status {status}"))
    }
}

fn escape_ps(value: &str) -> String {
    value.replace('\'', "''")
}

fn ensure_download(url: &str, destination: &Path) -> Result<(), String> {
    if destination.is_file() {
        return Ok(());
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;
    }

    let destination_ps = escape_ps(&destination.display().to_string());
    
    let script = if let Some(repo) = url.strip_prefix("github_win32:") {
        let repo_ps = escape_ps(repo);
        format!(
            "$ErrorActionPreference='Stop'; [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/{repo_ps}/releases/latest'; $asset = $release.assets | Where-Object {{ $_.name -like '*win32.exe' }} | Select-Object -First 1; if (-not $asset) {{ throw 'Windows executable asset not found' }}; Invoke-WebRequest -Uri $asset.browser_download_url -OutFile '{destination_ps}'"
        )
    } else {
        let url_ps = escape_ps(url);
        format!(
            "$ErrorActionPreference='Stop'; [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -Uri '{url_ps}' -OutFile '{destination_ps}'"
        )
    };

    run_powershell(&script)
}

fn extract_selected(zip_path: &Path, destination: &Path, wanted_names: &[&str]) -> Result<(), String> {
    if wanted_names.iter().all(|name| destination.join(name).is_file()) {
        return Ok(());
    }

    fs::create_dir_all(destination)
        .map_err(|error| format!("Failed to create {}: {error}", destination.display()))?;

    let zip_path = escape_ps(&zip_path.display().to_string());
    let destination = escape_ps(&destination.display().to_string());
    let wanted = wanted_names
        .iter()
        .map(|name| format!("'{}'", escape_ps(name)))
        .collect::<Vec<_>>()
        .join(", ");

    let script = format!(
        r#"$ErrorActionPreference='Stop';
$zip = '{zip_path}';
$dest = '{destination}';
$tmp = Join-Path $env:TEMP ('muzeeka-' + [guid]::NewGuid().ToString());
New-Item -ItemType Directory -Path $tmp | Out-Null;
Expand-Archive -LiteralPath $zip -DestinationPath $tmp -Force;
$wanted = @({wanted});
Get-ChildItem -LiteralPath $tmp -Recurse -File | Where-Object {{ $wanted -contains $_.Name }} | ForEach-Object {{ Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $dest $_.Name) -Force }};
Remove-Item -LiteralPath $tmp -Recurse -Force;"#
    );

    run_powershell(&script)
}

fn ensure_native_assets(manifest_dir: &Path) -> Result<(), String> {
    if !cfg!(windows) {
        return Ok(());
    }

    let bass_dir = manifest_dir.join("bass");
    let bin_dir = manifest_dir.join("bin");

    let bass_required = [
        "bass.dll",
        "bass_fx.dll",
        "bassalac.dll",
        "bassape.dll",
        "bassflac.dll",
        "basshls.dll",
        "bassmidi.dll",
        "bassmix.dll",
        "bassopus.dll",
        "basswasapi.dll",
        "basswma.dll",
        "basswv.dll",
    ];

    if !bass_required.iter().all(|name| bass_dir.join(name).is_file()) {
        let temp_dir = env::temp_dir().join("muzeeka-bass");
        let core_zip = temp_dir.join("bass24.zip");
        ensure_download(BASS_CORE_URL, &core_zip)?;
        extract_selected(&core_zip, &bass_dir, &["bass.dll"])?;

        for (file_name, url) in BASS_DOWNLOADS {
            let zip_path = temp_dir.join(file_name.replace(".dll", ".zip"));
            ensure_download(url, &zip_path)?;
            extract_selected(&zip_path, &bass_dir, &[*file_name])?;
        }
    }

    let tool_targets = ["yt-dlp.exe", "spotdl.exe", "ffmpeg.exe", "ffprobe.exe", "ffplay.exe"];
    let ffmpeg_ready = tool_targets.iter().all(|name| bin_dir.join(name).is_file());
    if !ffmpeg_ready {
        let temp_dir = env::temp_dir().join("muzeeka-tools");
        fs::create_dir_all(&temp_dir)
            .map_err(|error| format!("Failed to create {}: {error}", temp_dir.display()))?;

        for (file_name, url) in TOOL_DOWNLOADS {
            let destination = bin_dir.join(file_name);
            ensure_download(url, &destination)?;
        }

        let ffmpeg_zip = temp_dir.join("ffmpeg-release-essentials.zip");
        ensure_download(FFMPEG_URL, &ffmpeg_zip)?;
        extract_selected(
            &ffmpeg_zip,
            &bin_dir,
            &["ffmpeg.exe", "ffprobe.exe", "ffplay.exe"],
        )?;
    }

    Ok(())
}

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
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    if let Err(error) = ensure_native_assets(&manifest_dir) {
        panic!("{error}");
    }

    tauri_build::build();

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