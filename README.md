# muzeeka
**Pronunciation:** *moo-ZEE-kah* (рус. «музыка»)

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://shields.io/badge/-Rust-3776AB?style=flat&logo=rust)](https://rust-lang.org/)
[![GPUI](https://img.shields.io/badge/GPUI-gray)](https://gpui.rs/)
[![Bass Audio Library](https://img.shields.io/badge/BASS-Audio_Library-red)](https://www.un4seen.com/)
<p align="center">
	<img src="src/icons/logo.svg" alt="muzeeka logo" width="180" />
</p>

<p align="center">
	A desktop music player built with GPUI and BASS.
</p>

<p align="center">
	<a href="https://github.com/nnfz/muzeeka/releases">Releases</a> ·
	<a href="https://github.com/nnfz/muzeeka/issues">Issues</a>
</p>

> **Status:** early beta.  
> It works, but expect rough edges, incomplete features and occasional weirdness. 

The app is currently in the process of transitioning from WebView to native code.

## What is it?

muzeeka is a desktop audio player focused on local library playback, playlist management, and a polished UI.

**Supported platforms:** Windows only (for now).

## Features

- Local library with folder scanning, metadata & cover art
- Playlists (create, reorder, custom covers, liked tracks)
- Gapless playback
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

- Rust toolchain (stable, installed with [rustup](https://rustup.rs/))
- Visual Studio Build Tools with the "Desktop development with C++" workload and the Windows SDK
- CMake (needed to build GPUI dependencies on Windows)
- Git

Muzeeka is built on [GPUI](https://github.com/zed-industries/zed), which is pulled straight from the Zed repository. The first build downloads and compiles a lot of crates, so expect it to take a while.

### Runtime binaries

Playback, downloads and metadata rely on external binaries that are shipped in the repository under `src/bin`:

```text
src/bin/
  bass/            bass.dll and the BASS plugins
  ffmpeg.exe
  ffprobe.exe
  ffplay.exe
  yt-dlp.exe
  deno.exe
```

In debug builds (`cargo run`) they are picked up from `src/bin` automatically. In release builds they are searched in a `bin` folder next to the executable, and BASS in `bin/bass`.

### Development

```powershell
git clone https://github.com/nnfz/muzeeka.git
cd muzeeka
cargo run
```

### Checks

```powershell
cargo check
cargo test
```

## Build

### Release

```powershell
cargo build --release
```

The executable is generated at:

```text
target/release/muzeeka.exe
```

### Portable

There is no installer yet. A portable build is the executable plus the `bin` folder next to it:

```powershell
cargo build --release
New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\muzeeka.exe dist\
Copy-Item src\bin dist\bin -Recurse
```

The folder is generated in:

```text
dist/
```

The release profile uses thin LTO with a single codegen unit, so release builds are slower than usual.

### Available commands

- `cargo run` - build and start the app in debug mode
- `cargo check` - fast compile check without producing a binary
- `cargo test` - run the unit tests
- `cargo build --release` - optimized build

### Data location

Settings are stored in:

```text
%APPDATA%\com.nnfz.muzeeka\settings.json
```

## Contributing

Contributions are welcome. If you want to help, a good workflow is:

1. Fork or branch from the main repository
2. Install the prerequisites above
3. Run `cargo run` to test your changes locally
4. Make your changes
5. Run `cargo check` and `cargo test`, and make sure `cargo build --release` passes without errors
6. Open a pull request with a short description of what changed and why

If you are fixing a bug, please include the reproduction steps and what you verified.

## Acknowledgements

muzeeka depends on a number of great projects and libraries:

- [BASS](https://www.un4seen.com/) - the core audio playback engine
- [FFmpeg](https://ffmpeg.org/) - multimedia processing and conversion
- [yt-dlp](https://github.com/yt-dlp/yt-dlp) - media extraction capabilities
- [Deno](https://deno.com/) - JavaScript runtime used alongside yt-dlp
- [GPUI](https://github.com/zed-industries/zed) - the GPU-accelerated UI framework from Zed
- [Rust](https://www.rust-lang.org/) - systems language powering the whole application
- [windows](https://crates.io/crates/windows) - Windows API bindings
- [Google Sans Flex](https://fonts.google.com/specimen/Google+Sans+Flex) - font used by the app
- [tokio](https://crates.io/crates/tokio) - async runtime
- [crossbeam-channel](https://crates.io/crates/crossbeam-channel) - multi-producer channels
- [parking_lot](https://crates.io/crates/parking_lot) - faster synchronization primitives
- [libloading](https://crates.io/crates/libloading) - dynamic loading of BASS libraries
- [serde](https://serde.rs/) - Rust serialization framework
- [serde_json](https://docs.rs/serde_json/) - JSON handling in Rust
- [lofty](https://crates.io/crates/lofty) - audio metadata parsing
- [id3](https://crates.io/crates/id3) - ID3 tag support
- [cue-rw](https://crates.io/crates/cue-rw) - CUE sheet parsing
- [encoding_rs](https://crates.io/crates/encoding_rs) - text encoding detection and conversion
- [walkdir](https://crates.io/crates/walkdir) - recursive folder scanning
- [rayon](https://crates.io/crates/rayon) - parallel iteration in Rust
- [image](https://crates.io/crates/image) - image decoding and processing
- [rusqlite](https://crates.io/crates/rusqlite) - SQLite access
- [ureq](https://crates.io/crates/ureq) - HTTP client
- [regex](https://crates.io/crates/regex) - regular expressions
- [discord-rich-presence](https://crates.io/crates/discord-rich-presence) - Discord Rich Presence integration
- [Unison](https://unison.boidu.dev) - Lyrics from Unison

## Legal & Third-Party Notices

muzeeka integrates several powerful third-party libraries and CLI tools to handle media playback and extraction. These tools are governed by their own respective licenses:

- **[BASS Audio Library](https://www.un4seen.com/)**: Audio playback is powered by the BASS library. BASS is a product of Un4seen Developments Ltd. It is free for non-commercial use. If you intend to distribute or use muzeeka commercially, you must obtain a separate commercial license from Un4seen Developments.
- **[FFmpeg](https://ffmpeg.org/)**: This software uses the code of FFmpeg to handle audio processing, licensed under the LGPLv2.1 / GPLv3. FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project.
- **[yt-dlp](https://github.com/yt-dlp/yt-dlp)**: Used for media extraction. Released into the public domain (Unlicense).

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for the full text. 

*Note: The MIT license applies only to the source code of muzeeka itself, not to the pre-compiled third-party binaries (such as BASS or FFmpeg) required to run the application.*
