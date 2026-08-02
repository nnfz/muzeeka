# Muzeeka
**Pronunciation:** *moo-ZEE-kah* (рус. «музыка»)

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Tauri](https://img.shields.io/badge/Tauri-v2-24C8DB)](https://tauri.app/)
[![SvelteKit](https://img.shields.io/badge/SvelteKit-FF3E00?logo=svelte&logoColor=white)](https://svelte.dev/docs/kit)

<p align="center">
	<img src="logo/logo-1024.png" alt="Muzeeka logo" width="180" />
</p>

<p align="center">
	A desktop music player built with Tauri, SvelteKit, and Rust.
</p>

<p align="center">
	<a href="https://github.com/nnfz/muzeeka/releases">Releases</a> ·
	<a href="https://github.com/nnfz/muzeeka/issues">Issues</a>
</p>

> **Status:** early beta.  
> It works, but expect rough edges, incomplete features and occasional weirdness.

## What is it?

Muzeeka is a desktop audio player focused on local library playback, playlist management, and a polished UI.

**Supported platforms:** Windows only (for now).

## Features

- Local library with folder scanning, metadata & cover art
- Playlists (create, reorder, custom covers, liked tracks)
- Gapless playback powered by BASS
- Equalizer with custom presets
- Synchronized lyrics (LRC/TTML) + fullscreen lyrics view
- Search, drag-and-drop, context menus
- Download audio via yt-dlp and from VK
- Discord Rich Presence
- Built-in remote control over local network
- Playback speed control with optional pitch correction
- CUE sheet support
- Frameless modern UI with fullscreen player

## Getting Started

### Prerequisites

- Node.js
- Rust toolchain
- WebView2 on Windows

### Development

```powershell
npm install
npm run tauri dev
```

### Frontend checks

```powershell
npm run check
```

## Build

### Installer

```powershell
npm run build:installer
```

The installer is generated in:

```text
src-tauri/target/release/bundle/nsis/
```

### Portable

```powershell
npm run build:portable
```

The folder is generated in:

```text
src-tauri/target/release/
```

### Both builds

```powershell
npm run build:both
```

### Available scripts

- `npm run dev` - Vite dev server
- `npm run build` - frontend build
- `npm run check` - Svelte type and diagnostics check
- `npm run tauri` - Tauri CLI
- `npm run build:installer` - Windows installer build
- `npm run build:portable` - portable bundle build
- `npm run build:both` - run both build modes

## Contributing

Contributions are welcome. If you want to help, a good workflow is:

1. Fork or branch from the main repository
2. Install dependencies with `npm install`
3. Run `npm run tauri dev` to test your changes locally
4. Make your changes
5. Run `npm run check` and ensure `npm run tauri build` passes without errors
6. Open a pull request with a short description of what changed and why

If you are fixing a bug, please include the reproduction steps and what you verified.

## Acknowledgements

Muzeeka depends on a number of great projects and libraries:

- [BASS](https://www.un4seen.com/) - the core audio playback engine
- [FFmpeg](https://ffmpeg.org/) - multimedia processing and conversion
- [yt-dlp](https://github.com/yt-dlp/yt-dlp) - media extraction capabilities
- [Tauri](https://tauri.app/) - the desktop application framework
- [SvelteKit](https://svelte.dev/docs/kit) - application framework for the UI
- [Svelte](https://svelte.dev/) - reactive component framework
- [Vite](https://vite.dev/) - development server and build tool
- [TypeScript](https://www.typescriptlang.org/) - typed JavaScript support
- [Rust](https://www.rust-lang.org/) - systems language powering the backend
- [@tauri-apps/api](https://www.npmjs.com/package/@tauri-apps/api) - Tauri JavaScript API
- [@tauri-apps/plugin-dialog](https://www.npmjs.com/package/@tauri-apps/plugin-dialog) - native dialog integration
- [@tauri-apps/plugin-opener](https://www.npmjs.com/package/@tauri-apps/plugin-opener) - external file and URL opening
- [tauri-plugin-taskbar](https://www.npmjs.com/package/tauri-plugin-taskbar) - taskbar integration
- [@kawarp/core](https://www.npmjs.com/package/@kawarp/core) - shared audio-related utilities
- [@fontsource/inter](https://www.npmjs.com/package/@fontsource/inter) - font package used by the app
- [serde](https://serde.rs/) - Rust serialization framework
- [serde_json](https://docs.rs/serde_json/) - JSON handling in Rust
- [lofty](https://crates.io/crates/lofty) - audio metadata parsing
- [id3](https://crates.io/crates/id3) - ID3 tag support
- [rayon](https://crates.io/crates/rayon) - parallel iteration in Rust
- [image](https://crates.io/crates/image) - image decoding and processing
- [rusqlite](https://crates.io/crates/rusqlite) - SQLite access
- [axum](https://crates.io/crates/axum) - backend HTTP server components

## Legal & Third-Party Notices

Muzeeka integrates several powerful third-party libraries and CLI tools to handle media playback and extraction. These tools are governed by their own respective licenses:

- **[BASS Audio Library](https://www.un4seen.com/)**: Audio playback is powered by the BASS library. BASS is a product of Un4seen Developments Ltd. It is free for non-commercial use. If you intend to distribute or use Muzeeka commercially, you must obtain a separate commercial license from Un4seen Developments.
- **[FFmpeg](https://ffmpeg.org/)**: This software uses the code of FFmpeg to handle audio processing, licensed under the LGPLv2.1 / GPLv3. FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project.
- **[yt-dlp](https://github.com/yt-dlp/yt-dlp)**: Used for media extraction. Released into the public domain (Unlicense).

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for the full text. 

*Note: The MIT license applies only to the source code of Muzeeka itself, not to the pre-compiled third-party binaries (such as BASS or FFmpeg) required to run the application.*
