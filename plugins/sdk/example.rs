// Minimal native plugin in Rust. Copy into a cdylib crate with muzeeka_plugin.rs.
// plugin.json: { "id": "user.example", "name": "Example", "main": "example.dll",
//   "permissions": ["player:read"] }

use std::os::raw::c_int;

use crate::muzeeka_plugin::{MuzeekaHost, MUZEEKA_PLUGIN_ABI};

static mut HOST: Option<MuzeekaHost> = None;

#[no_mangle]
pub extern "C" fn muzeeka_plugin_abi() -> u32 {
    MUZEEKA_PLUGIN_ABI
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_start(host: *const MuzeekaHost) -> c_int {
    if host.is_null() {
        return 1;
    }
    unsafe {
        HOST = Some(*host);
        let host = HOST.as_ref().unwrap();
        let _ = host.call("log.info", r#"{"message":"native plugin started"}"#);
        let _ = host.call("player.state", "{}");
        // A soundtrack plugin also requests "library:write" and imports the
        // game's music folder into one playlist. ensurePlaylist matches the
        // name without caring about case. clear replaces the previous import.
        // host.call("library.import", r#"{
        //   "playlist": "Celeste",
        //   "paths": ["C:/Games/Celeste/Content/Music"],
        //   "clear": true
        // }"#).ok();
    }
    0
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_stop() {
    unsafe {
        HOST = None;
    }
}
