//! Helpers for spawning external CLI tools from a Windows GUI app,
//! and light process tuning when the player is in the background.

use std::process::Command;

/// Prevent a console window when launching console-subsystem tools
/// (yt-dlp, ffmpeg, spotDL, …) from the GUI process.
pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW — do not allocate a new console for the child.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// Drop process priority while the main window is unfocused so games
/// (Dota, etc.) get more CPU/GPU time. Restore when the player is focused.
///
/// Audio continues; only the scheduler priority changes.
pub fn set_background_mode(background: bool) {
    #[cfg(windows)]
    {
        use std::sync::atomic::{AtomicBool, Ordering};

        static LAST_BACKGROUND: AtomicBool = AtomicBool::new(false);
        if LAST_BACKGROUND.swap(background, Ordering::Relaxed) == background {
            return;
        }

        // winapi constants (avoid extra crate)
        const NORMAL_PRIORITY_CLASS: u32 = 0x0000_0020;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut core::ffi::c_void;
            fn SetPriorityClass(h_process: *mut core::ffi::c_void, dw_priority_class: u32) -> i32;
        }

        let class = if background {
            BELOW_NORMAL_PRIORITY_CLASS
        } else {
            NORMAL_PRIORITY_CLASS
        };
        unsafe {
            let _ = SetPriorityClass(GetCurrentProcess(), class);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = background;
    }
}
