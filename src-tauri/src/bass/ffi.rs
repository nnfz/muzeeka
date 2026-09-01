// BASS FFI — dynamic loading of bass.dll via libloading
//
// We load every function pointer at runtime so the binary doesn't hard-link
// against bass.dll / bass.lib. This lets us ship the DLL alongside the app
// and also load addon DLLs (bassflac.dll, etc.) at runtime.

use libloading::{Library, Symbol};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use super::types::*;
use super::types::{BASS_STREAM_STATUS, BASS_TAG_META};
use std::ffi::CStr;

fn cstr(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// Holds the loaded bass.dll and its resolved function pointers.
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
    bass_stream_free: unsafe extern "system" fn(handle: HSTREAM) -> BOOL,
    // Internet streams: URL is always a narrow string (no UNICODE form).
    // Real signature: (url, DWORD offset, DWORD flags, DOWNLOADPROC*, user)
    // — there is NO length argument (that is StreamCreateFile). Passing a dummy
    // length put `flags` in the DOWNLOADPROC slot; bass_aac then called it → AV.
    bass_stream_create_url: unsafe extern "system" fn(
        url: *const i8,
        offset: DWORD,
        flags: DWORD,
        proc: *mut std::ffi::c_void,
        user: *mut std::ffi::c_void,
    ) -> HSTREAM,
    bass_stream_get_file_position:
        unsafe extern "system" fn(handle: HSTREAM, mode: DWORD) -> QWORD,
    bass_stream_create: unsafe extern "system" fn(
        freq: DWORD,
        chans: DWORD,
        flags: DWORD,
        proc: *const std::ffi::c_void,
        user: *mut std::ffi::c_void,
    ) -> HSTREAM,
    bass_stream_put_data:
        unsafe extern "system" fn(handle: HSTREAM, buffer: *const std::ffi::c_void, length: DWORD) -> DWORD,

    // Music / tracker modules (MOD, XM, IT, S3M etc.)
    bass_music_load:
        unsafe extern "system" fn(mem: BOOL, file: *const u16, offset: QWORD, length: DWORD, flags: DWORD, freq: DWORD) -> HSTREAM,
    bass_music_free: unsafe extern "system" fn(handle: HSTREAM) -> BOOL,

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
    bass_channel_seconds2bytes:
        unsafe extern "system" fn(handle: DWORD, pos: f64) -> QWORD,
    bass_channel_set_attribute:
        unsafe extern "system" fn(handle: DWORD, attrib: DWORD, value: f32) -> BOOL,
    bass_channel_get_attribute:
        unsafe extern "system" fn(handle: DWORD, attrib: DWORD, value: *mut f32) -> BOOL,
    bass_channel_slide_attribute:
        unsafe extern "system" fn(handle: DWORD, attrib: DWORD, value: f32, time: DWORD) -> BOOL,
    bass_channel_get_info:
        unsafe extern "system" fn(handle: DWORD, info: *mut BassChannelInfo) -> BOOL,
    bass_channel_is_active: unsafe extern "system" fn(handle: DWORD) -> DWORD,
    #[allow(dead_code)] // used by channel_get_level
    bass_channel_get_level: unsafe extern "system" fn(handle: DWORD) -> DWORD,
    bass_channel_get_data:
        unsafe extern "system" fn(handle: DWORD, buffer: *mut std::ffi::c_void, length: DWORD) -> DWORD,
    bass_channel_get_tags:
        unsafe extern "system" fn(handle: DWORD, tags: DWORD) -> *const std::ffi::c_void,
    bass_channel_set_sync: unsafe extern "system" fn(
        handle: DWORD,
        synctype: DWORD,
        param: QWORD,
        proc: unsafe extern "system" fn(DWORD, DWORD, *mut std::ffi::c_void, *mut std::ffi::c_void),
        user: *mut std::ffi::c_void,
    ) -> DWORD,
    bass_channel_remove_sync: unsafe extern "system" fn(handle: DWORD, sync: DWORD) -> BOOL,

    // ── Config / DSP ──────────────────────────────────────────────────────────
    // DWORD value — BASS_SetConfig is not a float API. Passing f32 10000.0 as the
    // timeout used to send the *bit pattern* (~13 days) instead of 10000 ms.
    bass_set_config: unsafe extern "system" fn(option: DWORD, value: DWORD) -> BOOL,
    bass_set_config_ptr:
        unsafe extern "system" fn(option: DWORD, value: *const std::ffi::c_void) -> BOOL,
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

    // ── Plugins ─────────────────────────────────────────────────────────────
    bass_plugin_load:
        unsafe extern "system" fn(file: *const u16, flags: DWORD) -> HPLUGIN,

    bass_get_device_info:
        unsafe extern "system" fn(device: DWORD, info: *mut BassDeviceInfo) -> BOOL,
    bass_set_device: unsafe extern "system" fn(device: DWORD) -> BOOL,
    bass_get_device: unsafe extern "system" fn() -> DWORD,
    bass_channel_set_device: unsafe extern "system" fn(handle: DWORD, device: DWORD) -> BOOL,

    // ── Mixer (bassmix) — None when bassmix.dll is missing ──────────────────
    _mixer_lib: Option<Library>,
    bass_mixer_stream_create:
        Option<unsafe extern "system" fn(freq: DWORD, chans: DWORD, flags: DWORD) -> HSTREAM>,
    bass_mixer_stream_add_channel:
        Option<unsafe extern "system" fn(handle: DWORD, channel: DWORD, flags: DWORD) -> BOOL>,
    bass_mixer_channel_remove: Option<unsafe extern "system" fn(channel: DWORD) -> BOOL>,
    bass_mixer_channel_set_position:
        Option<unsafe extern "system" fn(channel: DWORD, pos: QWORD, mode: DWORD) -> BOOL>,
    bass_mixer_channel_get_position:
        Option<unsafe extern "system" fn(channel: DWORD, mode: DWORD) -> QWORD>,
    #[allow(dead_code)] // used by mixer_channel_flags
    bass_mixer_channel_flags:
        Option<unsafe extern "system" fn(channel: DWORD, flags: DWORD, mask: DWORD) -> DWORD>,
    bass_split_stream_create:
        Option<unsafe extern "system" fn(channel: DWORD, flags: DWORD, chanmap: *const i32) -> HSTREAM>,

    // ── FX (bass_fx.dll) — None until enable_fx_from_plugin succeeds ────────
    _fx_lib: Option<Library>,
    bass_fx_tempo_create:
        Option<unsafe extern "system" fn(chan: DWORD, flags: DWORD) -> HSTREAM>,
    bass_fx_tempo_get_source: Option<unsafe extern "system" fn(chan: HSTREAM) -> DWORD>,
}

// BassLibrary holds raw FFI function pointers into loaded DLLs. Access is always
// serialized via `parking_lot::Mutex<PlayerInner>` (BassLibrary is never shared
// as bare Arc/& without that mutex). We only need Send so the mutex can cross
// threads; Sync is intentionally not claimed here.
unsafe impl Send for BassLibrary {}

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

macro_rules! try_load_fn {
    ($lib:expr, $name:expr) => {{
        $lib
            .get($name)
            .ok()
            .map(|sym: Symbol<*const ()>| std::mem::transmute(*sym))
    }};
}

fn to_wide(path: &str) -> Vec<u16> {
    OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

impl BassLibrary {
    fn check(&self, ok: BOOL) -> Result<(), String> {
        if ok == 0 {
            Err(self.last_error_string())
        } else {
            Ok(())
        }
    }

    fn require_mixer_fn<'a, T>(
        &self,
        f: Option<&'a T>,
    ) -> Result<&'a T, String> {
        f.ok_or_else(|| "bassmix.dll not loaded".to_string())
    }

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

        // Load bassmix for proper gapless (BASS_Mixer_*)
        let mixer_lib = unsafe { Library::new(bass_dir.join("bassmix.dll")).ok() };
        let (mixer_create, mixer_add, mixer_remove, mixer_set_pos, mixer_get_pos, mixer_flags, split_create) =
            if let Some(ref mlib) = mixer_lib {
                unsafe {
                    (
                        Some(load_fn!(mlib, b"BASS_Mixer_StreamCreate\0")),
                        Some(load_fn!(mlib, b"BASS_Mixer_StreamAddChannel\0")),
                        Some(load_fn!(mlib, b"BASS_Mixer_ChannelRemove\0")),
                        Some(load_fn!(mlib, b"BASS_Mixer_ChannelSetPosition\0")),
                        Some(load_fn!(mlib, b"BASS_Mixer_ChannelGetPosition\0")),
                        Some(load_fn!(mlib, b"BASS_Mixer_ChannelFlags\0")),
                        try_load_fn!(mlib, b"BASS_Split_StreamCreate\0"),
                    )
                }
            } else {
                (None, None, None, None, None, None, None)
            };

        unsafe {
            Ok(Self {
                bass_init: load_fn!(lib, b"BASS_Init\0"),
                bass_free: load_fn!(lib, b"BASS_Free\0"),
                bass_error_get_code: load_fn!(lib, b"BASS_ErrorGetCode\0"),
                bass_stream_create_file: load_fn!(lib, b"BASS_StreamCreateFile\0"),
                bass_stream_free: load_fn!(lib, b"BASS_StreamFree\0"),
                bass_stream_create_url: load_fn!(lib, b"BASS_StreamCreateURL\0"),
                bass_stream_get_file_position: load_fn!(lib, b"BASS_StreamGetFilePosition\0"),
                bass_stream_create: load_fn!(lib, b"BASS_StreamCreate\0"),
                bass_stream_put_data: load_fn!(lib, b"BASS_StreamPutData\0"),
                bass_music_load: load_fn!(lib, b"BASS_MusicLoad\0"),
                bass_music_free: load_fn!(lib, b"BASS_MusicFree\0"),
                bass_channel_play: load_fn!(lib, b"BASS_ChannelPlay\0"),
                bass_channel_pause: load_fn!(lib, b"BASS_ChannelPause\0"),
                bass_channel_stop: load_fn!(lib, b"BASS_ChannelStop\0"),
                bass_channel_set_position: load_fn!(lib, b"BASS_ChannelSetPosition\0"),
                bass_channel_get_position: load_fn!(lib, b"BASS_ChannelGetPosition\0"),
                bass_channel_get_length: load_fn!(lib, b"BASS_ChannelGetLength\0"),
                bass_channel_bytes2seconds: load_fn!(lib, b"BASS_ChannelBytes2Seconds\0"),
                bass_channel_seconds2bytes: load_fn!(lib, b"BASS_ChannelSeconds2Bytes\0"),
                bass_channel_set_attribute: load_fn!(lib, b"BASS_ChannelSetAttribute\0"),
                bass_channel_get_attribute: load_fn!(lib, b"BASS_ChannelGetAttribute\0"),
                bass_channel_slide_attribute: load_fn!(lib, b"BASS_ChannelSlideAttribute\0"),
                bass_channel_get_info: load_fn!(lib, b"BASS_ChannelGetInfo\0"),
                bass_channel_is_active: load_fn!(lib, b"BASS_ChannelIsActive\0"),
                bass_channel_get_level: load_fn!(lib, b"BASS_ChannelGetLevel\0"),
                bass_channel_get_data: load_fn!(lib, b"BASS_ChannelGetData\0"),
                bass_channel_get_tags: load_fn!(lib, b"BASS_ChannelGetTags\0"),
                bass_channel_set_sync: load_fn!(lib, b"BASS_ChannelSetSync\0"),
                bass_channel_remove_sync: load_fn!(lib, b"BASS_ChannelRemoveSync\0"),
                bass_set_config: load_fn!(lib, b"BASS_SetConfig\0"),
                bass_set_config_ptr: load_fn!(lib, b"BASS_SetConfigPtr\0"),
                bass_channel_set_dsp: load_fn!(lib, b"BASS_ChannelSetDSP\0"),
                bass_channel_set_dsp_ex: load_fn!(lib, b"BASS_ChannelSetDSPEx\0"),
                bass_channel_remove_dsp: load_fn!(lib, b"BASS_ChannelRemoveDSP\0"),
                bass_plugin_load: load_fn!(lib, b"BASS_PluginLoad\0"),
                bass_get_device_info: load_fn!(lib, b"BASS_GetDeviceInfo\0"),
                bass_set_device: load_fn!(lib, b"BASS_SetDevice\0"),
                bass_get_device: load_fn!(lib, b"BASS_GetDevice\0"),
                bass_channel_set_device: load_fn!(lib, b"BASS_ChannelSetDevice\0"),
                _lib: lib,
                _mixer_lib: mixer_lib,
                bass_mixer_stream_create: mixer_create,
                bass_mixer_stream_add_channel: mixer_add,
                bass_mixer_channel_remove: mixer_remove,
                bass_mixer_channel_set_position: mixer_set_pos,
                bass_mixer_channel_get_position: mixer_get_pos,
                bass_mixer_channel_flags: mixer_flags,
                bass_split_stream_create: split_create,
                _fx_lib: None,
                bass_fx_tempo_create: None,
                bass_fx_tempo_get_source: None,
            })
        }
    }

    /// Load bass_fx.dll directly and resolve tempo FX entry points from it.
    pub fn enable_fx_from_plugin(&mut self, fx_dll_path: &Path) -> bool {
        let fx_lib = match unsafe { Library::new(fx_dll_path) } {
            Ok(lib) => lib,
            Err(e) => {
                eprintln!("Failed to load bass_fx.dll as Library: {e}");
                return false;
            }
        };
        unsafe {
            let Some(create) = try_load_fn!(fx_lib, b"BASS_FX_TempoCreate\0") else {
                return false;
            };
            let Some(get_source) = try_load_fn!(fx_lib, b"BASS_FX_TempoGetSource\0") else {
                return false;
            };
            self.bass_fx_tempo_create = Some(create);
            self.bass_fx_tempo_get_source = Some(get_source);
            self._fx_lib = Some(fx_lib);
            true
        }
    }

    pub fn has_fx(&self) -> bool {
        self.bass_fx_tempo_create.is_some()
    }

    /// Load a BASS format plugin (bassflac.dll, bassape.dll, etc.).
    pub fn plugin_load(&self, path: &str) -> Result<HPLUGIN, String> {
        let wide = to_wide(path);
        let handle = unsafe { (self.bass_plugin_load)(wide.as_ptr(), BASS_UNICODE) };
        if handle == 0 {
            Err(self.last_error_string())
        } else {
            Ok(handle)
        }
    }

    // ── Wrapped safe-ish API ──────────────────────────────────────────────

    /// Initialize BASS output. `device = -1` for default, `freq = 44100` typical.
    pub fn init(&self, device: i32, freq: u32) -> Result<(), String> {
        let ok = unsafe {
            (self.bass_init)(device, freq, 0, ptr::null_mut(), ptr::null())
        };
        self.check(ok)
    }

    /// Free all BASS resources.
    pub fn free(&self) -> Result<(), String> {
        let ok = unsafe { (self.bass_free)() };
        self.check(ok)
    }

    /// Create a stream from a file path (Windows wide-string).
    pub fn stream_create_file(&self, path: &str, flags: DWORD) -> Result<HSTREAM, String> {
        let wide = to_wide(path);
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

    /// Create a decode stream from an internet URL (HTTP/HTTPS radio).
    /// BLOCK keeps memory bounded for endless streams; the caller must add the
    /// stream to the mixer and free it via ChannelFree like any decode source.
    pub fn stream_create_url(&self, url: &str, flags: DWORD) -> Result<HSTREAM, String> {
        self.url_opener().open(url, flags)
    }

    /// Decode push stream — mixer reads this, a worker feeds it with `stream_put_data`.
    /// Never add an internet URL stream to the mixer directly: `ChannelGetData` on a
    /// URL decode source blocks BASS's update thread (silence + pause/Ctrl+R hang).
    pub fn stream_create_push(&self, freq: u32, chans: u32, flags: DWORD) -> Result<HSTREAM, String> {
        let handle = unsafe {
            (self.bass_stream_create)(
                freq,
                chans,
                flags,
                STREAMPROC_PUSH,
                ptr::null_mut(),
            )
        };
        if handle == 0 {
            Err(self.last_error_string())
        } else {
            Ok(handle)
        }
    }

    pub fn stream_put_data(&self, handle: DWORD, data: &[u8]) -> Result<u32, String> {
        if data.is_empty() {
            return Ok(0);
        }
        let wrote = unsafe {
            (self.bass_stream_put_data)(handle, data.as_ptr().cast(), data.len() as DWORD)
        };
        if wrote == DWORD::MAX {
            Err(self.last_error_string())
        } else {
            Ok(wrote)
        }
    }

    /// Bytes already waiting in a decode/internet stream (does not block).
    pub fn channel_data_available(&self, handle: DWORD) -> u32 {
        let got = unsafe {
            (self.bass_channel_get_data)(handle, ptr::null_mut(), BASS_DATA_AVAILABLE)
        };
        if got == DWORD::MAX {
            0
        } else {
            got
        }
    }

    pub fn data_pump(&self) -> DataPump {
        DataPump {
            get_data: self.bass_channel_get_data,
            put_data: self.bass_stream_put_data,
        }
    }

    /// A tiny, `Send + Copy` handle to just the raw entry points needed to open a
    /// URL stream. `BASS_StreamCreateURL` blocks during the network connect and must
    /// run on the BASS thread (never overlapping mixer/poll calls).
    pub fn url_opener(&self) -> UrlStreamOpener {
        UrlStreamOpener {
            create: self.bass_stream_create_url,
            error_get_code: self.bass_error_get_code,
        }
    }

    /// Raw ICY/HTTP tag block for a URL stream. `tags` is one of
    /// [`types::BASS_TAG_META`], [`types::BASS_TAG_ICY`], [`types::BASS_TAG_HTTP`].
    ///
    /// # Safety
    /// Must be called on the BASS thread while `handle` is a live URL stream.
    pub unsafe fn channel_get_tags_raw(&self, handle: DWORD, tags: DWORD) -> Option<String> {
        let ptr = (self.bass_channel_get_tags)(handle, tags);
        if ptr.is_null() {
            return None;
        }
        // BASS returns a NUL-terminated narrow string for META and a
        // NUL-NUL-terminated list of lines for ICY/HTTP/OGG/MP4.
        let mut len = 0usize;
        let mut p = ptr as *const u8;
        let list = tags != BASS_TAG_META;
        loop {
            if *p == 0 {
                if !list || *(p.add(1)) == 0 {
                    break;
                }
            }
            len += 1;
            p = p.add(1);
            if len > 64 * 1024 {
                break; // pathological header block — bail out
            }
        }
        let bytes = std::slice::from_raw_parts(ptr as *const u8, len);
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    /// Copy a BASS tag block from a raw `BASS_ChannelGetTags` pointer.
    /// Safe to call from a mixtime SYNCPROC.
    pub unsafe fn copy_tag_ptr(ptr: *const std::ffi::c_void, list: bool) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        let mut p = ptr as *const u8;
        loop {
            if *p == 0 {
                if !list || *(p.add(1)) == 0 {
                    break;
                }
            }
            len += 1;
            p = p.add(1);
            if len > 64 * 1024 {
                break;
            }
        }
        if len == 0 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr as *const u8, len);
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    pub fn channel_set_sync(
        &self,
        handle: DWORD,
        synctype: DWORD,
        proc: unsafe extern "system" fn(DWORD, DWORD, *mut std::ffi::c_void, *mut std::ffi::c_void),
        user: *mut std::ffi::c_void,
    ) -> Result<DWORD, String> {
        let sync = unsafe { (self.bass_channel_set_sync)(handle, synctype, 0, proc, user) };
        if sync == 0 {
            Err(self.last_error_string())
        } else {
            Ok(sync)
        }
    }

    pub fn tags_fn(
        &self,
    ) -> unsafe extern "system" fn(DWORD, DWORD) -> *const std::ffi::c_void {
        self.bass_channel_get_tags
    }

    pub fn channel_remove_sync(&self, handle: DWORD, sync: DWORD) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_remove_sync)(handle, sync) };
        self.check(ok)
    }

    /// Load a tracker/module (e.g. .it, .xm, .mod, .s3m) using BASS_MusicLoad.
    /// Many tracker plugins work best (or only) through the music API.
    pub fn music_load(&self, path: &str, flags: DWORD) -> Result<HSTREAM, String> {        let wide = to_wide(path);
        let handle = unsafe {
            (self.bass_music_load)(
                0, // mem = FALSE
                wide.as_ptr(),
                0,
                0, // length (0 = use all)
                flags | BASS_UNICODE,
                0, // freq = 0 (default)
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
        self.check(ok)
    }

    pub fn channel_pause(&self, handle: DWORD) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_pause)(handle) };
        self.check(ok)
    }

    pub fn channel_stop(&self, handle: DWORD) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_stop)(handle) };
        self.check(ok)
    }

    /// Free a stream or music handle. Tries StreamFree then MusicFree.
    /// Tempo FX streams are also freed with StreamFree (without FREESOURCE the source stays).
    pub fn channel_free(&self, handle: DWORD) -> Result<(), String> {
        if handle == 0 {
            return Ok(());
        }
        let ok = unsafe { (self.bass_stream_free)(handle) };
        if ok != 0 {
            return Ok(());
        }
        let ok = unsafe { (self.bass_music_free)(handle) };
        if ok != 0 {
            return Ok(());
        }
        Err(self.last_error_string())
    }

    pub fn channel_set_position(&self, handle: DWORD, pos: QWORD, mode: DWORD) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_set_position)(handle, pos, mode) };
        self.check(ok)
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

    pub fn channel_seconds2bytes(&self, handle: DWORD, seconds: f64) -> QWORD {
        unsafe { (self.bass_channel_seconds2bytes)(handle, seconds) }
    }

    pub fn channel_set_attribute(&self, handle: DWORD, attrib: DWORD, value: f32) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_set_attribute)(handle, attrib, value) };
        self.check(ok)
    }

    pub fn channel_get_attribute(&self, handle: DWORD, attrib: DWORD) -> Result<f32, String> {
        let mut value: f32 = 0.0;
        let ok = unsafe { (self.bass_channel_get_attribute)(handle, attrib, &mut value) };
        if ok == 0 {
            Err(self.last_error_string())
        } else {
            Ok(value)
        }
    }

    pub fn channel_slide_attribute(
        &self,
        handle: DWORD,
        attrib: DWORD,
        value: f32,
        time_ms: DWORD,
    ) -> Result<(), String> {
        let ok = unsafe { (self.bass_channel_slide_attribute)(handle, attrib, value, time_ms) };
        self.check(ok)
    }

    pub fn channel_get_info(&self, handle: DWORD) -> Result<BassChannelInfo, String> {
        let mut info = BassChannelInfo::default();
        let ok = unsafe { (self.bass_channel_get_info)(handle, &mut info) };
        if ok == 0 {
            Err(self.last_error_string())
        } else {
            Ok(info)
        }
    }

    pub fn channel_is_active(&self, handle: DWORD) -> DWORD {
        unsafe { (self.bass_channel_is_active)(handle) }
    }

    #[allow(dead_code)] // public BASS API surface; not yet used by player
    pub fn channel_get_level(&self, handle: DWORD) -> DWORD {
        unsafe { (self.bass_channel_get_level)(handle) }
    }

    /// Soft EOF / empty for decode-channel `ChannelGetData` pulls.
    fn channel_get_data_is_eof(&self, got: DWORD) -> bool {
        if got != DWORD::MAX {
            return got == 0;
        }
        let code = self.last_error();
        matches!(code, BassError::Ended | BassError::Ok | BassError::Decode)
            || code.code() == 0
    }

    /// Pull float PCM from a decode stream. `buffer` length is in **samples**.
    /// Returns samples written (0 at EOF).
    pub fn channel_get_data_f32(&self, handle: DWORD, buffer: &mut [f32]) -> Result<usize, String> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let want_bytes = std::mem::size_of_val(buffer) as DWORD;
        let length = want_bytes | BASS_DATA_FLOAT;
        let got = unsafe {
            (self.bass_channel_get_data)(
                handle,
                buffer.as_mut_ptr() as *mut std::ffi::c_void,
                length,
            )
        };
        if self.channel_get_data_is_eof(got) {
            return Ok(0);
        }
        if got == DWORD::MAX {
            return Err(format!(
                "BASS_ChannelGetData(float) failed: {}",
                self.last_error_string()
            ));
        }
        Ok((got as usize) / std::mem::size_of::<f32>())
    }

    /// Pull 16-bit PCM from a decode stream. `buffer` length is in **samples**.
    /// Returns samples written (0 at EOF).
    pub fn channel_get_data_i16(&self, handle: DWORD, buffer: &mut [i16]) -> Result<usize, String> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let want_bytes = std::mem::size_of_val(buffer) as DWORD;
        let got = unsafe {
            (self.bass_channel_get_data)(
                handle,
                buffer.as_mut_ptr() as *mut std::ffi::c_void,
                want_bytes,
            )
        };
        if self.channel_get_data_is_eof(got) {
            return Ok(0);
        }
        if got == DWORD::MAX {
            return Err(format!(
                "BASS_ChannelGetData(i16) failed: {}",
                self.last_error_string()
            ));
        }
        Ok((got as usize) / std::mem::size_of::<i16>())
    }

    pub fn set_config(&self, option: DWORD, value: DWORD) -> Result<(), String> {
        let ok = unsafe { (self.bass_set_config)(option, value) };
        self.check(ok)
    }

    pub fn set_config_ptr(&self, option: DWORD, value: *const std::ffi::c_void) -> Result<(), String> {
        let ok = unsafe { (self.bass_set_config_ptr)(option, value) };
        self.check(ok)
    }

    /// Internet-stream download/buffer state. `u64::MAX` means the call failed
    /// (not a file stream, or BASS error).
    pub fn stream_get_file_position(&self, handle: DWORD, mode: DWORD) -> u64 {
        unsafe { (self.bass_stream_get_file_position)(handle, mode) }
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
        self.check(ok)
    }

    // ── Mixer wrappers (require bassmix.dll) ────────────────────────────────

    pub fn mixer_stream_create(&self, freq: u32, chans: u32, flags: DWORD) -> Result<HSTREAM, String> {
        let f = self.require_mixer_fn(self.bass_mixer_stream_create.as_ref())?;
        let handle = unsafe { f(freq, chans, flags) };
        if handle == 0 {
            Err(self.last_error_string())
        } else {
            Ok(handle)
        }
    }

    pub fn mixer_stream_add_channel(&self, mixer: DWORD, channel: DWORD, flags: DWORD) -> Result<(), String> {
        let f = self.require_mixer_fn(self.bass_mixer_stream_add_channel.as_ref())?;
        let ok = unsafe { f(mixer, channel, flags) };
        self.check(ok)
    }

    pub fn mixer_channel_remove(&self, channel: DWORD) -> Result<(), String> {
        let f = self.require_mixer_fn(self.bass_mixer_channel_remove.as_ref())?;
        let ok = unsafe { f(channel) };
        self.check(ok)
    }

    pub fn mixer_channel_set_position(&self, channel: DWORD, pos: QWORD, mode: DWORD) -> Result<(), String> {
        let f = self.require_mixer_fn(self.bass_mixer_channel_set_position.as_ref())?;
        let ok = unsafe { f(channel, pos, mode) };
        self.check(ok)
    }

    pub fn mixer_channel_get_position(&self, channel: DWORD, mode: DWORD) -> QWORD {
        match self.bass_mixer_channel_get_position {
            Some(f) => unsafe { f(channel, mode) },
            None => 0,
        }
    }

    #[allow(dead_code)] // public BASS API surface; not yet used by player
    pub fn mixer_channel_flags(&self, channel: DWORD, flags: DWORD, mask: DWORD) -> DWORD {
        match self.bass_mixer_channel_flags {
            Some(f) => unsafe { f(channel, flags, mask) },
            None => 0,
        }
    }

    pub fn split_stream_create(&self, channel: DWORD, flags: DWORD) -> Result<HSTREAM, String> {
        let f = self
            .bass_split_stream_create
            .as_ref()
            .ok_or_else(|| "BASS_Split_StreamCreate is not available (old bassmix.dll?)".to_string())?;
        let handle = unsafe { f(channel, flags, std::ptr::null()) };
        if handle == 0 {
            Err(self.last_error_string())
        } else {
            Ok(handle)
        }
    }

    pub fn get_device_info(&self, device: u32) -> Option<(String, String, DWORD)> {
        let mut info = BassDeviceInfo {
            name: std::ptr::null(),
            driver: std::ptr::null(),
            flags: 0,
        };
        let ok = unsafe { (self.bass_get_device_info)(device, &mut info) };
        if ok == 0 {
            return None;
        }
        Some((cstr(info.name), cstr(info.driver), info.flags))
    }

    pub fn get_device(&self) -> i32 {
        unsafe { (self.bass_get_device)() as i32 }
    }

    pub fn set_device(&self, device: i32) -> Result<(), String> {
        if device < 0 {
            return Err("Invalid output device".into());
        }
        let ok = unsafe { (self.bass_set_device)(device as DWORD) };
        self.check(ok)
    }

    pub fn channel_set_device(&self, handle: DWORD, device: i32) -> Result<(), String> {
        if device < 0 {
            return Err("Invalid output device".into());
        }
        let ok = unsafe { (self.bass_channel_set_device)(handle, device as DWORD) };
        self.check(ok)
    }

    pub fn list_devices(&self) -> Vec<OutputDeviceInfo> {
        let mut devices = Vec::new();
        for index in 1..=64u32 {
            let Some((name, driver, flags)) = self.get_device_info(index) else {
                break;
            };
            if name.is_empty() {
                continue;
            }
            devices.push(OutputDeviceInfo {
                id: index as i32,
                name,
                driver,
                flags,
            });
        }
        devices
    }

    pub fn fx_tempo_create(&self, chan: DWORD, flags: DWORD) -> Result<HSTREAM, String> {
        let f = self
            .bass_fx_tempo_create
            .ok_or_else(|| "bass_fx is not loaded".to_string())?;
        let tempo = unsafe { f(chan, flags) };
        if tempo == 0 {
            Err(self.last_error_string())
        } else {
            Ok(tempo)
        }
    }

    pub fn fx_tempo_get_source(&self, tempo: HSTREAM) -> DWORD {
        match self.bass_fx_tempo_get_source {
            Some(f) => unsafe { f(tempo) },
            None => 0,
        }
    }

    // ── Error helpers ─────────────────────────────────────────────────────

    pub fn last_error(&self) -> BassError {
        BassError::from(unsafe { (self.bass_error_get_code)() })
    }

    pub fn last_error_string(&self) -> String {
        self.last_error().to_string()
    }
}

/// Copyable GetData/PutData entry points for the live-radio feeder thread.
/// Mixer never calls GetData on the URL handle; this worker does, then pushes
/// into a `STREAMPROC_PUSH` stream the mixer *does* read.
#[derive(Clone, Copy)]
pub struct DataPump {
    get_data: unsafe extern "system" fn(
        handle: DWORD,
        buffer: *mut std::ffi::c_void,
        length: DWORD,
    ) -> DWORD,
    put_data: unsafe extern "system" fn(
        handle: HSTREAM,
        buffer: *const std::ffi::c_void,
        length: DWORD,
    ) -> DWORD,
}

unsafe impl Send for DataPump {}

impl DataPump {
    pub fn available(&self, handle: DWORD) -> u32 {
        let got = unsafe { (self.get_data)(handle, ptr::null_mut(), BASS_DATA_AVAILABLE) };
        if got == DWORD::MAX {
            0
        } else {
            got
        }
    }

    /// Pull up to `buf.len()` bytes. `float` ORs `BASS_DATA_FLOAT`.
    /// Never pass an empty `buf`. Returns 0 if nothing is ready / error.
    pub fn pull(&self, handle: DWORD, buf: &mut [u8], float: bool) -> u32 {
        if buf.is_empty() {
            return 0;
        }
        let mut length = buf.len() as DWORD;
        if float {
            length |= BASS_DATA_FLOAT;
        }
        let got = unsafe { (self.get_data)(handle, buf.as_mut_ptr().cast(), length) };
        if got == DWORD::MAX {
            0
        } else {
            got
        }
    }

    pub fn push(&self, handle: DWORD, data: &[u8]) -> u32 {
        if data.is_empty() {
            return 0;
        }
        let wrote = unsafe { (self.put_data)(handle, data.as_ptr().cast(), data.len() as DWORD) };
        if wrote == DWORD::MAX {
            0
        } else {
            wrote
        }
    }
}

/// A copyable, `Send` bundle of just the entry points needed to open a URL stream.
/// See [`BassLibrary::url_opener`]. Must still be invoked on the BASS thread —
/// `BASS_StreamCreateURL` is not safe to overlap with mixer/poll calls.
#[derive(Clone, Copy)]
pub struct UrlStreamOpener {
    create: unsafe extern "system" fn(
        url: *const i8,
        offset: DWORD,
        flags: DWORD,
        proc: *mut std::ffi::c_void,
        user: *mut std::ffi::c_void,
    ) -> HSTREAM,
    error_get_code: unsafe extern "system" fn() -> i32,
}

// Only raw fn pointers into an already-loaded, still-alive DLL. Safe to move to
// another thread; BASS itself is process-global.
unsafe impl Send for UrlStreamOpener {}

impl UrlStreamOpener {
    /// Open a URL decode stream. BLOCK keeps memory bounded for endless radio;
    /// STATUS exposes ICY/HTTP headers. Blocks during the network connect — call
    /// only on the BASS thread.
    pub fn open(&self, url: &str, flags: DWORD) -> Result<HSTREAM, String> {
        self.open_proc(url, flags, ptr::null_mut(), ptr::null_mut())
    }

    pub fn open_proc(
        &self,
        url: &str,
        flags: DWORD,
        proc: *mut std::ffi::c_void,
        user: *mut std::ffi::c_void,
    ) -> Result<HSTREAM, String> {
        let narrow = std::ffi::CString::new(url)
            .map_err(|_| "Stream URL contains a NUL byte".to_string())?;
        let handle = unsafe {
            (self.create)(
                narrow.as_ptr(),
                0,
                flags | BASS_STREAM_STATUS,
                proc,
                user,
            )
        };
        if handle == 0 {
            let code = unsafe { (self.error_get_code)() };
            Err(BassError::from(code).to_string())
        } else {
            Ok(handle)
        }
    }
}
