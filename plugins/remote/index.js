// Example Muzeeka JS plugin. Drop folders like this into /plugins.
// Native (Rust/C): "main": "plugin.dll" — see /plugins/sdk/muzeeka_plugin.h
//
// start(muzeeka) / stop(muzeeka)
// muzeeka.player   play, pause, resume, toggle, next, prev, seek, volume, state
// muzeeka.library  playlists, playlist(id), tracks(query)
//   writes need library:write, which this remote does not request:
//   createPlaylist, ensurePlaylist, renamePlaylist, deletePlaylist, clearPlaylist,
//   addTracks, removeTracks, deleteTracks, import, setLiked
// muzeeka.audio    devices, addOutput(deviceId, volume?), removeOutput(id),
//                  setOutputVolume(id, volume), outputs
// muzeeka.http     serve({ port, staticDir, mount: ["player-api"] }), stop, status
// muzeeka.settings get(key?), set(key, value) | set({ ... })
// muzeeka.log      info, error
//
// plugin.json: id, name, version, author, description, main,
// permissions, enabled_by_default, settings[]
// permissions: player:read, player:control, library:read, library:write,
// audio:devices, audio:output, http:listen, fs:plugin-dir

function start(muzeeka) {
  var port = muzeeka.settings.get("port");
  if (!port) port = 8765;
  muzeeka.http.serve({
    port: port,
    staticDir: "ui",
    mount: ["player-api"]
  });
}

function stop(muzeeka) {
  muzeeka.http.stop();
}
