//! Helpers for spawning external CLI tools from a Windows GUI app,
//! and light process tuning when the player is in the background.

use std::process::Command;
use std::sync::Mutex;

/// Prevent a console window when launching console-subsystem tools
/// (yt-dlp, ffmpeg, …) from the GUI process.
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

/// Windows thread IDs that must stay elevated when the process is BELOW_NORMAL
/// (BASS control thread + BASS mixer/DSP callback thread).
#[cfg(windows)]
static AUDIO_THREAD_IDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Register the calling thread as audio-critical and raise its priority.
///
/// Call from:
/// - the thread that runs `BASS_Init` / player control (`mark_bass_thread`)
/// - the first BASS DSP callback (mixer/update thread that runs the EQ)
///
/// `SetPriorityClass(BELOW_NORMAL)` lowers *all* threads' base priority; without
/// an explicit `SetThreadPriority` boost, the mixer can underrun under game load.
pub fn register_audio_thread() {
    #[cfg(windows)]
    {
        let tid = unsafe { GetCurrentThreadId() };
        {
            let mut ids = AUDIO_THREAD_IDS.lock().unwrap_or_else(|e| e.into_inner());
            if !ids.contains(&tid) {
                ids.push(tid);
            }
        }
        set_thread_priority_elevated(tid, true);
    }
}

/// Drop process priority while the main window is unfocused so games
/// (Dota, etc.) get more CPU/GPU time. Restore when the player is focused.
///
/// Process-wide `BELOW_NORMAL` alone would also slow BASS mixer/DSP threads.
/// Registered audio threads are re-boosted after every class change so
/// playback keeps a higher relative priority than the UI/WebView work.
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

        let class = if background {
            BELOW_NORMAL_PRIORITY_CLASS
        } else {
            NORMAL_PRIORITY_CLASS
        };
        unsafe {
            let _ = SetPriorityClass(GetCurrentProcess(), class);
        }

        // Re-apply elevated priorities — process class changes affect all threads.
        let ids = AUDIO_THREAD_IDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for tid in ids {
            set_thread_priority_elevated(tid, true);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = background;
    }
}

#[cfg(windows)]
fn set_thread_priority_elevated(thread_id: u32, elevated: bool) {
    // THREAD_SET_INFORMATION | THREAD_QUERY_INFORMATION
    const THREAD_SET_INFORMATION: u32 = 0x0020;
    const THREAD_QUERY_INFORMATION: u32 = 0x0040;
    const THREAD_PRIORITY_HIGHEST: i32 = 2;
    const THREAD_PRIORITY_NORMAL: i32 = 0;

    let access = THREAD_SET_INFORMATION | THREAD_QUERY_INFORMATION;
    let handle = unsafe { OpenThread(access, 0, thread_id) };
    if handle.is_null() {
        return;
    }
    let priority = if elevated {
        THREAD_PRIORITY_HIGHEST
    } else {
        THREAD_PRIORITY_NORMAL
    };
    unsafe {
        let _ = SetThreadPriority(handle, priority);
        let _ = CloseHandle(handle);
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut core::ffi::c_void;
    fn GetCurrentThreadId() -> u32;
    fn SetPriorityClass(h_process: *mut core::ffi::c_void, dw_priority_class: u32) -> i32;
    fn OpenThread(
        dw_desired_access: u32,
        b_inherit_handle: i32,
        dw_thread_id: u32,
    ) -> *mut core::ffi::c_void;
    fn SetThreadPriority(h_thread: *mut core::ffi::c_void, n_priority: i32) -> i32;
    fn CloseHandle(h_object: *mut core::ffi::c_void) -> i32;
}
