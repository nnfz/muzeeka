use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::{Library, Symbol};
use parking_lot::Mutex;

use super::host::{PluginCall, PluginHost};

pub const PLUGIN_ABI: u32 = 1;

#[repr(C)]
pub struct MuzeekaHost {
    data: *const c_void,
    call: unsafe extern "C" fn(
        data: *const c_void,
        method: *const c_char,
        payload: *const c_char,
    ) -> *mut c_char,
    free_str: unsafe extern "C" fn(*mut c_char),
}

unsafe impl Send for MuzeekaHost {}

struct NativeCallCtx {
    plugin_id: String,
    permissions: Vec<String>,
    dir: PathBuf,
    host: PluginHost,
}

struct LiveNative {
    /// Keep last so `stop` can still run while the library is loaded.
    _lib: Library,
    stop: unsafe extern "C" fn(),
    _host: Box<MuzeekaHost>,
    _ctx: Arc<NativeCallCtx>,
}

pub struct NativeEngine {
    live: Mutex<HashMap<String, LiveNative>>,
}

impl NativeEngine {
    pub fn new() -> Self {
        Self {
            live: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(
        &self,
        plugin_id: &str,
        dll_path: &Path,
        permissions: Vec<String>,
        dir: PathBuf,
        host: PluginHost,
    ) -> Result<(), String> {
        let _ = self.stop(plugin_id);

        let dll_path = resolve_dll(&dir, dll_path)?;
        let lib = load_library(&dll_path)?;

        let abi: Symbol<unsafe extern "C" fn() -> u32> = unsafe {
            lib.get(b"muzeeka_plugin_abi\0")
        }
        .map_err(|_| {
            "Not a Muzeeka native plugin (missing muzeeka_plugin_abi). Rebuild against plugins/sdk/muzeeka_plugin.h."
                .to_string()
        })?;
        let abi_ver = unsafe { abi() };
        if abi_ver != PLUGIN_ABI {
            return Err(format!(
                "Plugin ABI {abi_ver}, Muzeeka wants {PLUGIN_ABI}. Rebuild the DLL."
            ));
        }

        let start: Symbol<unsafe extern "C" fn(*const MuzeekaHost) -> c_int> = unsafe {
            lib.get(b"muzeeka_plugin_start\0")
        }
        .map_err(|_| "Missing muzeeka_plugin_start".to_string())?;
        let stop: Symbol<unsafe extern "C" fn()> = unsafe {
            lib.get(b"muzeeka_plugin_stop\0")
        }
        .map_err(|_| "Missing muzeeka_plugin_stop".to_string())?;
        let stop = *stop;

        let ctx = Arc::new(NativeCallCtx {
            plugin_id: plugin_id.to_string(),
            permissions,
            dir,
            host,
        });
        let ffi = Box::new(MuzeekaHost {
            data: Arc::as_ptr(&ctx) as *const c_void,
            call: host_call,
            free_str: host_free,
        });

        let code = unsafe { start(&*ffi) };
        if code != 0 {
            return Err(format!("muzeeka_plugin_start returned {code}"));
        }

        self.live.lock().insert(
            plugin_id.to_string(),
            LiveNative {
                _lib: lib,
                stop,
                _host: ffi,
                _ctx: ctx,
            },
        );
        Ok(())
    }

    pub fn stop(&self, plugin_id: &str) -> Result<(), String> {
        let Some(live) = self.live.lock().remove(plugin_id) else {
            return Ok(());
        };
        unsafe {
            (live.stop)();
        }
        drop(live);
        Ok(())
    }
}

fn load_library(path: &Path) -> Result<Library, String> {
    #[cfg(windows)]
    {
        // So Logitech/etc DLLs sitting next to the plugin resolve.
        let flags = libloading::os::windows::LOAD_WITH_ALTERED_SEARCH_PATH;
        let raw = unsafe { libloading::os::windows::Library::load_with_flags(path, flags) }
            .map_err(|e| format!("Failed to load {}: {e}", path.display()))?;
        Ok(raw.into())
    }
    #[cfg(not(windows))]
    {
        unsafe { Library::new(path) }.map_err(|e| format!("Failed to load {}: {e}", path.display()))
    }
}

fn resolve_dll(plugin_dir: &Path, dll_path: &Path) -> Result<PathBuf, String> {
    let path = if dll_path.is_absolute() {
        dll_path.to_path_buf()
    } else {
        plugin_dir.join(dll_path)
    };
    if !path.is_file() {
        return Err(format!("Native plugin DLL not found: {}", path.display()));
    }
    let plugin_dir = std::fs::canonicalize(plugin_dir).unwrap_or_else(|_| plugin_dir.to_path_buf());
    let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if !canonical.starts_with(&plugin_dir) {
        return Err("Native DLL must live inside the plugin folder".into());
    }
    Ok(canonical)
}

unsafe extern "C" fn host_call(
    data: *const c_void,
    method: *const c_char,
    payload: *const c_char,
) -> *mut c_char {
    if data.is_null() || method.is_null() {
        return to_raw_json(r#"{"__error":"invalid host call"}"#);
    }
    let ctx = unsafe { &*(data as *const NativeCallCtx) };
    let method = unsafe { CStr::from_ptr(method) }.to_string_lossy();
    let payload = if payload.is_null() {
        "{}".to_string()
    } else {
        unsafe { CStr::from_ptr(payload) }
            .to_string_lossy()
            .into_owned()
    };
    let result = ctx.host.dispatch(
        &PluginCall {
            plugin_id: &ctx.plugin_id,
            permissions: &ctx.permissions,
            dir: &ctx.dir,
        },
        &method,
        &payload,
    );
    match result {
        Ok(value) => {
            let raw = serde_json::to_string(&value).unwrap_or_else(|_| "null".into());
            to_raw_json(&raw)
        }
        Err(err) => {
            let raw = serde_json::json!({ "__error": err }).to_string();
            to_raw_json(&raw)
        }
    }
}

unsafe extern "C" fn host_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    drop(unsafe { CString::from_raw(ptr) });
}

fn to_raw_json(s: &str) -> *mut c_char {
    CString::new(s.replace('\0', "")).map_or(std::ptr::null_mut(), CString::into_raw)
}
