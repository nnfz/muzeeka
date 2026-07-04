// BASS audio module — re-exports types and FFI
//
// The user should place bass.dll (and optional addon DLLs like bassflac.dll)
// into the `src-tauri/bass/` directory.

pub mod ffi;
pub mod types;

pub use ffi::{BassLibrary, load_addon};
pub use types::*;
