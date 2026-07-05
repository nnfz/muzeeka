// BASS FFI — dynamic loading of bass.dll via libloading
//
// We load every function pointer at runtime so the binary doesn't hard-link
// against bass.dll / bass.lib. This lets us ship the DLL alongside the app
// and also load addon DLLs (bassflac.dll, etc.) at runtime.

use libloading::{Library, Symbol};
use std::ffi::OsStr;
use std::path::Path;
use std::ptr;

use super::types::*;

/// Holds the loaded bass.dll and its resolved function pointers.
#[allow(dead_code)]
pub struct BassLibrary {
    // Keep the library alive so function pointers remain valid.
    _lib: Library,

    // ── Core functions ────────────────────────────────────────────────────
    bass_init:
        unsafe extern "system" fn(device: i32, freq: DWORD, flags: DWORD, win: *mut std::ffi::c_void, dsguid: *const std::ffi::c_void) -> BOOL,
    bass_free: unsafe extern "system" fn() -> BOOL,
    bass_error_get_code: unsafe extern "system" fn() -> i32,

    // ── Stream creation ───────────────────────────────────────────────────
    bass_stream_create_file:
        unsafe extern "system" fn(mem: BOOL, file: *const u16, offset: QWORD, length: QWORD, flags: DWORD) -> HSTREAM,

    // ── Channel control ───────────────────────────────────────────────────
    bass_channel_play: unsafe extern "system" fn(handle: DWORD, restart: BOOL) -> BOOL,
    bass_channel_pause: unsafe extern "system" fn(handle: DWORD) -> BOOL,
    bass_channel_stop: unsafe extern "system" fn(handle: DWORD) -> BOOL,
    bass_channel_set_position:
        unsafe extern "system" fn(handle: DWORD, pos: QWORD, mode: DWORD) -> BOOL,
    bass_channel_get_position:
        unsafe extern "system" fn(handle: DWORD, mode: DWORD) -> QWORD,
    bass_channel_get_length:
        unsafe extern "system" fn(handle: DWORD, mode: DWORD) -> QWORD,
    bass_channel_bytes2seconds:
        unsafe extern "system" fn(handle: DWORD, pos: QWORD) -> f64,
    bass_channel_set_attribute:
        unsafe extern "system" fn(handle: DWORD, attrib: DWORD, value: f32) -> BOOL,
    bass_channel_get_attribute:
        unsafe extern "system" fn(handle: DWORD, attrib: DWORD, value: *mut f32) -> BOOL,
    bass_channel_get_info:
        unsafe extern "system" fn(handle: DWORD, info: *mut BassChannelInfo) -> BOOL,
    bass_channel_is_active: unsafe extern "system" fn(handle: DWORD) -> DWORD,
    bass_channel_get_level: unsafe extern "system" fn(handle: DWORD) -> DWORD,

    // ── Config / DSP ──────────────────────────────────────────────────────────
    bass_set_config: unsafe extern "system" fn(option: DWORD, value: f32) -> BOOL,
    bass_channel_set_dsp:
        unsafe extern "system" fn(handle: DWORD, proc: DspProc, priority: i32, user: *mut std::ffi::c_void) -> HDSP,
    bass_channel_set_dsp_ex:
        unsafe extern "system" fn(
            handle: DWORD,
            proc: DspProc,
            user: *mut std::ffi::c_void,
            priority: i32,
            flags: DWORD,
        ) -> HDSP,
    bass_channel_remove_dsp: unsafe extern "system" fn(handle: DWORD, dsp: HDSP) -> BOOL,
}

// Safety: BassLibrary is always used behind a parking_lot::Mutex.
// All BASS calls must happen from the same thread that called BASS_Init (the main thread
// in our design), so the Mutex serialization guarantees correct access.
unsafe impl Send for BassLibrary {}
unsafe impl Sync for BassLibrary {}

/// Resolve a function pointer from a loaded library.
///  
/// # Safety
/// The caller must ensure the symbol exists and has the correct signature.
macro_rules! load_fn {
    ($lib:expr, $name:expr) => {{
        let sym: Symbol<*const ()> = $lib
            .get($name)
            .map_err(|e| format!("Failed to load {}: {}", String::from_utf8_lossy($name), e))?;
        std::mem::transmute(*sym)
    }};
}

#[allow(dead_code)]
impl BassLibrary {
    /// Load bass.dll from the given directory.
    ///
    /// `bass_dir` should be the folder containing `bass.dll`.
    /// On debug builds this is typically `src-tauri/bass/`.
    pub fn load(bass_dir: &Path) -> Result<Self, String> {
        let dll_path = bass_dir.join("bass.dll");
        if !dll_path.exists() {
            return Err(format!(
                "bass.dll not found at {}. Place the BASS library there.",
                dll_path.display()
            ));
        }

        let lib = unsafe {
            Library::new(&dll_path)
                .map_err(|e| format!("Failed to load bass.dll: {}", e))?
        };

        unsafe {
            Ok(Self {
                bass_init: load_fn!(lib, b"BASS_Init\0"),
                bass_free: load_fn!(lib, b"BASS_Free\0"),
                bass_error_get_code: load_fn!(lib, b"BASS_ErrorGetCode\0"),
                bass_stream_create_file: load_fn!(lib, b"BASS_StreamCreateFile\0"),
                bass_channel_play: load_fn!(lib, b"BASS_ChannelPlay\0"),
                bass_channel_pause: load_fn!(lib, b"BASS_ChannelPause\0"),
                bass_channel_stop: load_fn!(lib, b"BASS_ChannelStop\0"),
                bass_channel_set_position: load_fn!(lib, b"BASS_ChannelSetPosition\0"),
                bass_channel_get_position: load_fn!(lib, b"BASS_ChannelGetPosition\0"),
                bass_channel_get_length: load_fn!(lib, b"BASS_ChannelGetLength\0"),
                bass_channel_bytes2seconds: load_fn!(lib, b"BASS_ChannelBytes2Seconds\0"),
                bass_channel_set_attribute: load_fn!(lib, b"BASS_ChannelSetAttribute\0"),
                bass_channel_get_attribute: load_fn!(lib, b"BASS_ChannelGetAttribute\0"),
                bass_channel_get_info: load_fn!(lib, b"BASS_ChannelGetInfo\0"),
                bass_channel_is_active: load_fn!(lib, b"BASS_ChannelIsActive\0"),
                bass_channel_get_level: load_fn!(lib, b"BASS_ChannelGetLevel\0"),
                bass_set_config: load_fn!(lib, b"BASS_SetConfig\0"),
                bass_channel_set_dsp: load_fn!(lib, b"BASS_ChannelSetDSP\0"),
                bass_channel_set_dsp_ex: load_fn!(lib, b"BASS_ChannelSetDSPEx\0"),
                bass_channel_remove_dsp: load_fn!(lib, b"BASS_ChannelRemoveDSP\0"),
                _lib: lib,
            })
        }
    }

    // ── Wrapped safe-ish API ──────────────────────────────────────────────

    /// Initialize BASS output. `device = -1` for default, `freq = 44100` typical.
    pub fn init(&self, device: i32, freq: u32) -> Result<(), String> {
        let ok = unsafe {
            (self.bass_init)(device, freq, 0, ptr::null_mut(), ptr::null())
        };
        if ok == 0 {
            Err(self.last_error_string())
        } else {
            Ok(())
        }
    }

    /// Free all BASS resources.
    pub fn free(&self) -> Result<(), String> {
        let ok = unsafe { (self.bass_free)() };
        if ok == 0 {
            Err(self.last_error_string())
        } else {
            Ok(())
        }
    }

    /// Create a stream from a file path (Windows wide-string).
    pub fn stream_create_file(&self, path: &str, flags: DWORD) -> Result<HSTREAM, String> {
        let wide: Vec<u16> = OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            (self.bass_stream_create_file)(
                0, // mem = FALSE (file path, not memory)
                wide.as_ptr(),
                0,
                0,
                flags | BASS_UNICODE,
            )
        };
        if handle == 0 {
            Err(self.last_error_string())
        } else {
            Ok(handle)
        }
    }

    pub fn channel_play(&self, handle: DWORD, restart: bool) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_play)(handle, if restart { 1 } else { 0 }) };
        if ok == 0 { Err(self.last_error_string()) } else { Ok(()) }
    }

    pub fn channel_pause(&self, handle: DWORD) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_pause)(handle) };
        if ok == 0 { Err(self.last_error_string()) } else { Ok(()) }
    }

    pub fn channel_stop(&self, handle: DWORD) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_stop)(handle) };
        if ok == 0 { Err(self.last_error_string()) } else { Ok(()) }
    }

    pub fn channel_set_position(&self, handle: DWORD, pos: QWORD, mode: DWORD) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_set_position)(handle, pos, mode) };
        if ok == 0 { Err(self.last_error_string()) } else { Ok(()) }
    }

    pub fn channel_get_position(&self, handle: DWORD, mode: DWORD) -> QWORD {
        unsafe { (self.bass_channel_get_position)(handle, mode) }
    }

    pub fn channel_get_length(&self, handle: DWORD, mode: DWORD) -> QWORD {
        unsafe { (self.bass_channel_get_length)(handle, mode) }
    }

    pub fn channel_bytes2seconds(&self, handle: DWORD, pos: QWORD) -> f64 {
        unsafe { (self.bass_channel_bytes2seconds)(handle, pos) }
    }

    pub fn channel_set_attribute(&self, handle: DWORD, attrib: DWORD, value: f32) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_set_attribute)(handle, attrib, value) };
        if ok == 0 { Err(self.last_error_string()) } else { Ok(()) }
    }

    pub fn channel_get_attribute(&self, handle: DWORD, attrib: DWORD) -> Result<f32, String> {
        let mut value: f32 = 0.0;
        let ok = unsafe { (self.bass_channel_get_attribute)(handle, attrib, &mut value) };
        if ok == 0 { Err(self.last_error_string()) } else { Ok(value) }
    }

    pub fn channel_get_info(&self, handle: DWORD) -> Result<BassChannelInfo, String> {
        let mut info = BassChannelInfo::default();
        let ok = unsafe { (self.bass_channel_get_info)(handle, &mut info) };
        if ok == 0 { Err(self.last_error_string()) } else { Ok(info) }
    }

    pub fn channel_is_active(&self, handle: DWORD) -> DWORD {
        unsafe { (self.bass_channel_is_active)(handle) }
    }

    pub fn channel_get_level(&self, handle: DWORD) -> DWORD {
        unsafe { (self.bass_channel_get_level)(handle) }
    }

    pub fn set_config(&self, option: DWORD, value: f32) -> Result<(), String> {
        let ok = unsafe { (self.bass_set_config)(option, value) };
        if ok == 0 {
            Err(self.last_error_string())
        } else {
            Ok(())
        }
    }

    pub fn channel_set_dsp(
        &self,
        handle: DWORD,
        proc: DspProc,
        priority: i32,
        user: *mut std::ffi::c_void,
    ) -> Result<HDSP, String> {
        let dsp = unsafe { (self.bass_channel_set_dsp)(handle, proc, priority, user) };
        if dsp == 0 {
            Err(self.last_error_string())
        } else {
            Ok(dsp)
        }
    }

    pub fn channel_set_dsp_ex(
        &self,
        handle: DWORD,
        proc: DspProc,
        user: *mut std::ffi::c_void,
        priority: i32,
        flags: DWORD,
    ) -> Result<HDSP, String> {
        let dsp =
            unsafe { (self.bass_channel_set_dsp_ex)(handle, proc, user, priority, flags) };
        if dsp == 0 {
            Err(self.last_error_string())
        } else {
            Ok(dsp)
        }
    }

    pub fn channel_remove_dsp(&self, handle: DWORD, dsp: HDSP) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_remove_dsp)(handle, dsp) };
        if ok == 0 {
            Err(self.last_error_string())
        } else {
            Ok(())
        }
    }

    // ── Error helpers ─────────────────────────────────────────────────────

    pub fn last_error(&self) -> i32 {
        unsafe { (self.bass_error_get_code)() }
    }

    pub fn last_error_string(&self) -> String {
        let code = self.last_error();
        format!("BASS error {}: {}", code, bass_error_to_string(code))
    }
}

/// Load a BASS addon DLL (e.g. bassflac.dll, bassopus.dll).
///
/// Addons register themselves with BASS automatically when loaded, so we just
/// need to keep the `Library` handle alive for the lifetime of the application.
pub fn load_addon(path: &Path) -> Result<Library, String> {
    if !path.exists() {
        return Err(format!("Addon not found: {}", path.display()));
    }
    unsafe {
        Library::new(path)
            .map_err(|e| format!("Failed to load addon {}: {}", path.display(), e))
    }
}

// We need OsStrExt for encode_wide on Windows
use std::os::windows::ffi::OsStrExt;
