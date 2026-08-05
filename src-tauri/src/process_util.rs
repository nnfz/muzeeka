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
            return;
        }
        let _ = unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_CRITICAL) };
        let mut handles = MMCSS_HANDLES.lock().unwrap_or_else(|e| e.into_inner());
        handles.push(MmcssHandle(handle));
    }
}

pub fn unregister_current_audio_thread() {
    #[cfg(windows)]
    {
        let mut handles = MMCSS_HANDLES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(MmcssHandle(handle)) = handles.pop() {
            unsafe {
                let _ = AvRevertMmThreadCharacteristics(handle);
            }
        }
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
    fn GetCurrentProcess() -> *mut core::ffi::c_void;
    fn SetPriorityClass(h_process: *mut core::ffi::c_void, dw_priority_class: u32) -> i32;
}

#[cfg(windows)]
#[link(name = "avrt")]
unsafe extern "system" {
    fn AvSetMmThreadCharacteristicsW(
        task_name: *const u16,
        task_index: *mut u32,
    ) -> *mut core::ffi::c_void;
    fn AvSetMmThreadPriority(av_rt_handle: *mut core::ffi::c_void, priority: i32) -> i32;
    fn AvRevertMmThreadCharacteristics(av_rt_handle: *mut core::ffi::c_void) -> i32;
}