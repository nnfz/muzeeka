//! Helpers for spawning external CLI tools from a Windows GUI app.

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
