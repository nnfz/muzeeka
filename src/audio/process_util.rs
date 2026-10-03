use std::process::Command;
use std::sync::Mutex;

pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

#[cfg(windows)]
#[allow(dead_code)]
struct MmcssHandle(*mut core::ffi::c_void);

#[cfg(windows)]
unsafe impl Send for MmcssHandle {}

#[cfg(windows)]
static MMCSS_HANDLES: Mutex<Vec<MmcssHandle>> = Mutex::new(Vec::new());

pub fn register_audio_thread() {
    #[cfg(windows)]
    {
        let mut task_index: u32 = 0;
        let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let handle = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut task_index) };
        if handle.is_null() {
            const THREAD_PRIORITY_HIGHEST: i32 = 2;
            unsafe {
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
            }
            return;
        }
        let _ = unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_CRITICAL) };
        let mut handles = MMCSS_HANDLES.lock().unwrap_or_else(|e| e.into_inner());
        handles.push(MmcssHandle(handle));
    }
}

pub fn set_background_mode(background: bool) {
    #[cfg(windows)]
    {
        use std::sync::atomic::{AtomicBool, Ordering};

        static LAST_BACKGROUND: AtomicBool = AtomicBool::new(false);
        if LAST_BACKGROUND.swap(background, Ordering::Relaxed) == background {
            return;
        }

        // Only the calling UI thread. Dropping the *process* class to
        // BELOW_NORMAL used to starve BASS's WASAPI/update threads whenever a
        // game or encode saturated the CPU, which other players don't do.
        const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
        const THREAD_PRIORITY_NORMAL: i32 = 0;
        unsafe {
            let _ = SetThreadPriority(
                GetCurrentThread(),
                if background {
                    THREAD_PRIORITY_BELOW_NORMAL
                } else {
                    THREAD_PRIORITY_NORMAL
                },
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = background;
    }
}

#[cfg(windows)]
const AVRT_PRIORITY_CRITICAL: i32 = 2;

#[cfg(windows)]
unsafe extern "system" {
    fn GetCurrentThread() -> *mut core::ffi::c_void;
    fn SetThreadPriority(h_thread: *mut core::ffi::c_void, n_priority: i32) -> i32;
}

#[cfg(windows)]
#[link(name = "avrt")]
unsafe extern "system" {
    fn AvSetMmThreadCharacteristicsW(
        task_name: *const u16,
        task_index: *mut u32,
    ) -> *mut core::ffi::c_void;
    fn AvSetMmThreadPriority(av_rt_handle: *mut core::ffi::c_void, priority: i32) -> i32;
}