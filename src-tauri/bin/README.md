# Bundled tools (yt-dlp + ffmpeg)

Place executables here:

| Tool | Windows | macOS / Linux |
|------|---------|---------------|
| yt-dlp | `yt-dlp.exe` + `_internal/` folder | `yt-dlp` |
| ffmpeg | `ffmpeg.exe` | `ffmpeg` |

Optional but recommended on Windows: `ffprobe.exe` (same folder).

Downloads:
- yt-dlp: https://github.com/yt-dlp/yt-dlp/releases — use the **PyInstaller** zip (`yt-dlp_win.zip`), extract **both** `yt-dlp.exe` and the `_internal` folder into this directory.
- ffmpeg: https://www.gyan.dev/ffmpeg/builds/ (or https://ffmpeg.org/download.html)

The build script recursively copies everything from this folder (including `_internal/`) to `target/debug/bin/`.
They are also bundled into the installer via Tauri resources.

The app passes `--ffmpeg-location` pointing at this folder, so ffmpeg does **not** need to be on system PATH.