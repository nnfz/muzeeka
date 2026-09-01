# Muzeeka plugins

> Русская версия: [README_RU.md](README_RU.md)

Muzeeka supports two kinds of plugins: **JS** (a script run by the embedded Boa engine) and
**native** (a Windows DLL with a C ABI). Plugins get access to the player, the library,
audio devices and settings, and can run their own HTTP server (static files plus the
player's REST/SSE API).

Plugin folder:

- **dev build** — `plugins/` in the repository root (next to `src-tauri`);
- **packaged build** — `plugins/` next to the exe (otherwise the app resources).

A plugin is a folder containing a `plugin.json`. Drop the folder in, then restart or
refresh the list in *Settings → Plugins*. The `sdk/` folder is skipped by the scanner.

Plugin data (settings and anything else) lives separately, in
`<app data>/plugin-data/<id>/`.

---

## plugin.json

```json
{
  "id": "user.mixpamp",
  "name": "MixPamp",
  "version": "0.1.0",
  "author": "you",
  "description": "What the plugin does",
  "main": "index.js",
  "runtime": "js",
  "enabled_by_default": false,
  "permissions": ["player:read", "player:control"],
  "settings": [
    {
      "key": "gain",
      "type": "number",
      "label": "Gain",
      "description": "0.0–2.0",
      "min": 0,
      "max": 2,
      "default": 1
    }
  ]
}
```

| Field | Description |
|---|---|
| `id` | Required. At least two dot-separated sections (`vendor.name`), only `a-z 0-9 . _ -`, no leading/trailing dots and no `..`. Example: `muzeeka.remote`, `user.mixpamp`. |
| `name` | Required, non-empty. Shown in settings. |
| `version`, `author`, `description` | Optional, shown in the UI. |
| `main` | Entry point, `index.js` by default. For native plugins, the path to the `.dll` relative to the plugin folder. |
| `runtime` | `"js"` or `"native"` (`"dll"` also accepted). If omitted, inferred from the `main` extension. |
| `enabled_by_default` | Enable the plugin when it is first discovered (the user can still turn it off). |
| `permissions` | Permission list, see below. An unknown permission rejects the manifest. |
| `settings` | Declarative settings; Muzeeka renders the form itself, no UI code needed. |

### settings[]

Each entry has `key`, `type` (`number` \| `boolean` \| `string`), `label`, `description`,
`min`/`max` for numbers, and an optional `default`. The host coerces stored values to the
declared type, clamps numbers into min/max and falls back to the default, so a plugin
always receives a valid value. Changing settings in the UI **restarts** the plugin.

---

## Permissions

| Permission | Unlocks |
|---|---|
| `player:read` | `player.state` |
| `player:control` | `player.play / pause / resume / toggle / next / prev / seek / volume` |
| `library:read` | `library.playlists`, `library.playlist` |
| `audio:devices` | `audio.devices` |
| `audio:output` | `audio.addOutput`, `audio.removeOutput`, `audio.setOutputVolume`, `audio.outputs` |
| `http:listen` | `http.serve`, `http.stop`, `http.status` |
| `fs:plugin-dir` | Reserved, grants nothing yet. |

A call without the required permission fails with
`Plugin is missing permission '<x>'`.

---

## Plugin API

JS plugins get a global `muzeeka` object; native plugins reach the same methods through
`host.call("<method>", "<json>")`. Names and payloads are identical.

```js
muzeeka.player
  .state()                  // snapshot: isPlaying, isPaused, position, duration, volume,
                            // shuffleEnabled, repeatMode, track, activePlaylistId/Name
  .play(path, playlistId?)  // play a file (playlistId is the playback context)
  .pause() .resume() .toggle() .next() .prev()
  .seek(position)           // seconds
  .volume(v)                // 0.0–1.0

muzeeka.library
  .playlists()              // [{ id, name, trackCount }]
  .playlist(id)             // { id, name, tracks: [{ path, title, artist, album, durationSecs, coverUrl }] }

muzeeka.audio
  .devices()                // output devices
  .addOutput(deviceId, volume?) // add a parallel output; volume 0.0–1.0, defaults to 1.0
  .removeOutput(id)
  .setOutputVolume(id, v)   // volume of one extra output, 0.0–1.0
  .outputs()                // active extra outputs: [{ id, deviceId, name, volume }]

muzeeka.http
  .serve({ port, staticDir, mount: ["player-api"] })
  .stop() .status()

muzeeka.settings
  .get(key?)                // without an argument, the whole settings object
  .set(key, value)          // or .set({ ...patch })

muzeeka.log
  .info(msg) .error(msg)    // goes to the dev log and is visible in settings
```

Errors (missing permission, bad arguments, unknown method) are thrown as `Error` in JS and
returned as `{"__error":"..."}` from native calls.

`track` in the player state is `{ path, title, artist, album, durationSecs, coverUrl }` or
`null`.

---

## JS plugins

The `main` file (usually `index.js`) is evaluated when the plugin starts, then the hooks
are called:

```js
function start(muzeeka) {
  const port = muzeeka.settings.get("port") || 8765;
  muzeeka.http.serve({ port, staticDir: "ui", mount: ["player-api"] });
}

function stop(muzeeka) {
  muzeeka.http.stop();
}
```

Things to know about the environment:

- Every plugin is its own JS context on a shared engine thread. `start()` must return
  quickly: a long loop inside it blocks starting and stopping the other JS plugins.
- **Host calls only work inside `start()` and `stop()`** — once `start()` returns, calling
  `muzeeka.*` throws. The JS runtime has no timers, events or background callbacks: a JS
  plugin is "configure the server at startup, then get out of the way".
- There is no `console` — log through `muzeeka.log`.
- An error in the script or in `start()` means the plugin did not start; the error text is
  visible in settings.

The natural use for a JS plugin is a web UI: `http.serve` serves static files from the
plugin folder (`staticDir` is a relative path) and, with `mount: ["player-api"]`, the full
player REST/SSE API (see below). The live logic then runs client-side in the browser.

## Native plugins (DLL)

`main` points at a DLL built against ABI 1. The DLL must live **inside the plugin folder**.

Three required exports:

```c
#include "muzeeka_plugin.h"

uint32_t muzeeka_plugin_abi(void);                       // return MUZEEKA_PLUGIN_ABI
int      muzeeka_plugin_start(const MuzeekaHost *host);  // 0 = ok
void     muzeeka_plugin_stop(void);
```

`MuzeekaHost` has three fields: `data`, `call(data, method, payload_json) -> char*` and
`free_str(ptr)`. Payload and result are UTF-8 JSON; an error is `{"__error":"..."}`. The
string returned by `call` must be released with `free_str`. The `host` pointer is valid
until `stop` returns; after that the host must not be called.

Rules:

- Do long-running work on your own threads and **join them in `stop`** (see
  `plugins/native-probe/src/lib.rs`).
- A panic or crash in the DLL takes down all of Muzeeka — there is no isolation.
- The DLL is loaded with `LOAD_WITH_ALTERED_SEARCH_PATH`, so ship your own dependent DLLs
  next to it in the plugin folder.
- An ABI mismatch stops the plugin from starting; rebuild against the current
  `plugins/sdk/muzeeka_plugin.h`.

### Rust

Use the helper at `plugins/sdk/muzeeka_plugin.rs` (a `MuzeekaHost` struct with
`call(method, payload) -> Result<serde_json::Value, String>`). Minimal example:
`plugins/sdk/example.rs`; a real one: `plugins/native-probe`. Build the crate as a
`cdylib`.

```toml
[lib]
crate-type = ["cdylib"]
```

### C/C++

Include `plugins/sdk/muzeeka_plugin.h` and implement the three exports.

---

## Plugin HTTP server

Each plugin gets its own listener (calling `serve` again with the same options is a no-op;
with new options it restarts). Options:

- `port` — ports below 1024 are replaced with 8765;
- `staticDir` — static folder relative to the plugin folder; unknown paths fall back to
  `index.html` (SPA mode);
- `mount: ["player-api"]` — mount the player REST/SSE API.

The server listens on `0.0.0.0` and CORS is fully open. `http.status()` and the plugin card
in settings show the `localhost` URL and the best LAN address (heuristics drop VPN/virtual
adapters and fake-IP ranges).

### Player API (`mount: ["player-api"]`)

```
GET  /api, /api/info      API description (self-documenting)
GET  /api/state           player state snapshot
GET  /api/stream          live state stream: SSE ("state" event) by default,
                          ?format=ndjson for line-delimited JSON, ?interval=ms (50–2000, default 250)
GET  /api/events          alias for /api/stream
GET  /api/playlists       playlist list
GET  /api/playlist?id=…   playlist tracks
GET  /api/cover?path=…    cover art bytes for a track
POST /api/play            { "path": "...", "playlistId"?: "..." }
POST /api/toggle | /api/pause | /api/resume | /api/next | /api/prev
POST /api/seek            { "position": 12.5 }
POST /api/volume          { "volume": 0.5 }
POST /api/playlist/select { "id": "..." }
POST /api/shuffle/toggle  → { "shuffle_enabled": bool }
POST /api/repeat/toggle   → { "repeat_mode": "off|one|all" }
```

---

## Bundled plugins

### muzeeka.remote — JS

Phone remote: starts an HTTP server (`port` setting, 8765 by default), serves
`ui/index.html` and the player API. Permissions: `player:read`,
`player:control`, `library:read`, `http:listen`. Enabled by default; on first run the port
and on/off settings are migrated from the old built-in remote module.

### muzeeka.micspam — native

Micspam: duplicates the player output into a virtual audio cable so Discord and games see
the music as a microphone. A background thread polls every `poll_ms` (500–10000, default
2000), looks for an output device whose name contains `device_match` (`CABLE Input` by
default), attaches it via `audio.addOutput` and reattaches it if the output was dropped by
a BASS restart. On `stop` the output is removed. Permissions: `audio:devices`,
`audio:output`. Disabled by default. Rebuild with `cargo build --release` in
`plugins/micspam`.

The cable is a kernel-mode driver, so the plugin does not create it and does not touch the
system: install [VB-CABLE](https://vb-audio.com/Cable/) once by hand, then pick
`CABLE Output` as your microphone in Discord. Until the device exists the plugin just logs
that and keeps waiting.

Voice volume is separate: the `volume_percent` setting (0–100, default 70) moves only the
cable level, through `audio.setOutputVolume` (`BASS_ATTRIB_VOL` on the tap's push stream).
What you hear in your own headphones stays on the mixer and does not change. The slider
applies to a live output and survives a BASS restart.

Technically an extra output is not a splitter but a tap (`src-tauri/src/output_tap.rs`): a
DSP on the mixer copies the buffer into a push stream on the cable device.
`BASS_Split_StreamCreate` does not work here — it needs a decode source while the mixer is
a playing channel, which is where error 38 came from. The voice feed matches what you hear:
the DSP sits after the rack.

Check cable audio quality in a DAW (FL Studio or similar), not in a voice chat: Telegram
and Discord compress voice with a narrowband codec, which falls apart on bass regardless of
the source.

### muzeeka.native-probe — native

A test DLL plugin: every `interval_ms` (500–60000, default 3000) it logs what is playing.
Demonstrates a background thread, reading settings and calling `player.state` from native
code. Disabled by default. Rebuild with `cargo build --release` in
`plugins/native-probe`.

### plugins/sdk

`muzeeka_plugin.h` (C/C++), `muzeeka_plugin.rs` (Rust helper), `example.rs` (minimal
example). Not scanned by the plugin scanner.

---

## Checklist: what a plugin can do

- Read the player state and the library (playlists, tracks).
- Control playback: play/pause/next/prev/seek/volume.
- Add and remove parallel audio outputs (a second pair of speakers, for instance).
- Run an HTTP server with its own web UI and the player REST/SSE API — remote controls,
  integrations, OBS overlays and so on.
- Store settings: a declarative form in the Muzeeka UI plus read/write from the plugin.
- Write to the dev log (`muzeeka.log`).
- Native plugins: any background logic on their own threads (polling, hotkeys, network
  clients) — within the same set of host methods.

Not available yet: subscribing to player events from JS (only polling on native threads or
SSE through your own HTTP server), arbitrary filesystem access, and custom UI pages inside
the Muzeeka window.
