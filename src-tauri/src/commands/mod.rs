// Tauri commands — the IPC bridge between frontend and Rust backend.
//
// Each `#[tauri::command]` becomes callable from JS via `invoke("command_name", { args })`.

mod library;
mod lyrics;
mod notify;
mod player;
mod settings;
mod vk;
mod ytdlp;

pub use library::*;
pub use lyrics::*;
pub use player::*;
pub use settings::*;
pub use vk::*;
pub use ytdlp::*;
