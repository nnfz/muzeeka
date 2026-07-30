# Muzeeka

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Tauri](https://img.shields.io/badge/Tauri-v2-24C8DB)](https://tauri.app/)
[![SvelteKit](https://img.shields.io/badge/SvelteKit-FF3E00?logo=svelte&logoColor=white)](https://svelte.dev/docs/kit)

<p align="center">
	<img src="logo/logo-1024.png" alt="Muzeeka logo" width="180" />
</p>

<p align="center">
	A desktop music player built with Tauri, SvelteKit, and Rust.
</p>

## What is it?

Muzeeka is a cross-platform desktop audio player focused on local library playback, playlist management, and a polished native-feeling UI.

## Features

- Desktop app built with Tauri + SvelteKit
- Local audio library and playlist workflows
- Modern UI with lyrics, search, drag-and-drop, and playback controls
- Rust-powered backend for media handling and app integration

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

After building, create a portable package by placing these together:

1. `src-tauri/target/release/muzeeka.exe`
2. `src-tauri/bass/`
3. Zip the folder contents

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
3. Make your changes
4. Run `npm run check` and any relevant build or test commands
5. Open a pull request with a short description of what changed and why

If you are fixing a bug, please include the reproduction steps and what you verified.

## Acknowledgements

Muzeeka depends on a number of great projects and libraries:

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

## Possible additions for this README

- Screenshots or a short animated demo
- A release download section
- A feature roadmap
- A troubleshooting section for common Windows issues
- A license badge and a short license summary
- A changelog or version history

## Notes

- Portable builds require the `bass/` folder next to `muzeeka.exe`.
- On Windows, WebView2 is usually required, and Visual C++ Redistributable may also be needed.

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for the full text.

Third-party dependencies keep their own upstream licenses. If you want, I can also add a short third-party notices section or generate a dependency license list.
