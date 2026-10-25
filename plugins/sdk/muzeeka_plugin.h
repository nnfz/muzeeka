#ifndef MUZEEKA_PLUGIN_H
#define MUZEEKA_PLUGIN_H

/*
  Muzeeka native plugin ABI 1 (Windows x64).

  Export these three functions from a DLL. ABI stays 1; new methods are new call names.
  player.state / play / pause / resume / toggle / next / prev / seek / volume
  library.playlists / playlist / tracks
  library.createPlaylist / ensurePlaylist / renamePlaylist / deletePlaylist / clearPlaylist
  library.addTracks / removeTracks / deleteTracks / import / setLiked
  audio.devices / addOutput / removeOutput / setOutputVolume / outputs
  http.serve / http.stop / http.status
  settings.get / settings.set
  log.info / log.error

  library.import scans files or folders into a playlist (a game soundtrack, for
  example). A file with no extension is accepted when its header is audio.
  Optional labels [{path, title, artist, cover}] fill a blank title or artist.
  cover is an image path used only when the file has no embedded cover.
  clear replaces that playlist. All tracks and Liked cannot be edited.
  settings.set from the plugin saves the file and does not restart it.

  Payload and return value are UTF-8 JSON. Errors: {"__error":"..."}.
  Strings returned by `call` must be released with `free_str`.

  Join your own threads in muzeeka_plugin_stop. After stop the host pointer
  is invalid. A panic/crash in the DLL still kills Muzeeka (same as foobar).
*/

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MUZEEKA_PLUGIN_ABI 1

typedef struct MuzeekaHost {
    const void *data;
    char *(*call)(const void *data, const char *method, const char *payload_json);
    void (*free_str)(char *ptr);
} MuzeekaHost;

/* Must return MUZEEKA_PLUGIN_ABI. */
uint32_t muzeeka_plugin_abi(void);

/* 0 = ok, nonzero = fail. Host pointer is valid until stop. */
int muzeeka_plugin_start(const MuzeekaHost *host);

void muzeeka_plugin_stop(void);

#ifdef __cplusplus
}
#endif

#endif
