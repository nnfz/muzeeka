# BASS Audio Library

Place the BASS library files here:

## Required
- `bass.dll` — Main BASS library

## Optional Addons  
- `bassflac.dll` — FLAC support
- `bassape.dll` — APE (Monkey's Audio) support
- `bassopus.dll` — Opus support  
- `basswma.dll` — WMA support
- `bassalac.dll` — ALAC support
- `basscd.dll` — Audio CD support
- `basshls.dll` — HLS streaming
- `bassmidi.dll` — MIDI support
- `basswv.dll` — WavPack support

**Tracker / chiptune support:**
Place your tracker plugin (e.g. `basszxtune.dll` or similar) in this folder.
It will be **automatically discovered and loaded** at startup (any `bass*.dll` except core ones like bassmix).

This enables playing tracker files (MOD, XM, S3M, IT, AY, VGM, NSF, PT3, etc.).

If the plugin is incompatible with your `bass.dll` version you will see a non-fatal "BASS plugin not loaded" message (playback of other formats continues to work).

Official addons: https://www.un4seen.com/
