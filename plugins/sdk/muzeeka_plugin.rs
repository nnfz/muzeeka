//! Host FFI helpers for a native Muzeeka plugin (`cdylib`).
//! See `example.rs` for the three required exports.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

pub const MUZEEKA_PLUGIN_ABI: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MuzeekaHost {
    pub data: *const c_void,
    pub call: unsafe extern "C" fn(
        data: *const c_void,
        method: *const c_char,
        payload: *const c_char,
    ) -> *mut c_char,
    pub free_str: unsafe extern "C" fn(*mut c_char),
}

unsafe impl Send for MuzeekaHost {}
unsafe impl Sync for MuzeekaHost {}

impl MuzeekaHost {
    /// Call a host method. `payload` is JSON (`{}` if unused).
    pub fn call(&self, method: &str, payload: &str) -> Result<serde_json::Value, String> {
        if self.call as usize == 0 || self.data.is_null() {
            return Err("host is gone".into());
        }
        let method = CString::new(method).map_err(|e| e.to_string())?;
        let payload = CString::new(payload.replace('\0', "")).map_err(|e| e.to_string())?;
        let raw = unsafe { (self.call)(self.data, method.as_ptr(), payload.as_ptr()) };
        if raw.is_null() {
            return Err("host returned null".into());
        }
        let text = unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned();
        unsafe { (self.free_str)(raw) };
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("host json: {e}"))?;
        if let Some(err) = value.get("__error").and_then(|v| v.as_str()) {
            return Err(err.to_string());
        }
        Ok(value)
    }
}