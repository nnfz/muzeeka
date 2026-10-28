use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::LoadLibraryW;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, LoadCursorW, SetCursor, HCURSOR, IDC_ARROW, SM_CXCURSOR, WM_SETCURSOR,
};

const HTCLIENT: u32 = 1;
const OLE_COPY: u16 = 3;
const OLE_COPY_LARGE: u16 = 7;

static SUBCLASSED: AtomicBool = AtomicBool::new(false);
static ARMED: AtomicBool = AtomicBool::new(false);
static CURSOR: AtomicIsize = AtomicIsize::new(0);

pub fn set(window: &Window, on: bool) -> bool {
    let Some(hwnd) = hwnd_of(window) else {
        return false;
    };
    if !SUBCLASSED.swap(true, Ordering::AcqRel) {
        let ok = unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), 2, 0) };
        if !ok.as_bool() {
            SUBCLASSED.store(false, Ordering::Relaxed);
            return false;
        }
    }
    if !on {
        if ARMED.swap(false, Ordering::Relaxed) {
            unsafe { SetCursor(LoadCursorW(None, IDC_ARROW).ok()) };
        }
        return true;
    }
    if CURSOR.load(Ordering::Relaxed) == 0 {
        if let Some(cursor) = load() {
            CURSOR.store(cursor.0 as isize, Ordering::Relaxed);
        }
    }
    let cursor = CURSOR.load(Ordering::Relaxed);
    ARMED.store(cursor != 0, Ordering::Relaxed);
    if cursor != 0 {
        unsafe { SetCursor(Some(HCURSOR(cursor as *mut _))) };
    }
    cursor != 0
}

fn hwnd_of(window: &Window) -> Option<HWND> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(win) = handle.as_raw() else {
        return None;
    };
    let hwnd = HWND(win.hwnd.get() as *mut _);
    if hwnd.0.is_null() {
        None
    } else {
        Some(hwnd)
    }
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let hit = lparam.0 as u32 & 0xffff;
    let cursor = CURSOR.load(Ordering::Relaxed);
    if message == WM_SETCURSOR && ARMED.load(Ordering::Relaxed) && hit == HTCLIENT && cursor != 0 {
        unsafe { SetCursor(Some(HCURSOR(cursor as *mut _))) };
        return LRESULT(1);
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

fn load() -> Option<HCURSOR> {
    unsafe {
        let name: Vec<u16> = "ole32.dll"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let module = LoadLibraryW(PCWSTR(name.as_ptr())).ok()?;
        let id = if GetSystemMetrics(SM_CXCURSOR) > 32 {
            OLE_COPY_LARGE
        } else {
            OLE_COPY
        };
        let module = module.into();
        LoadCursorW(Some(module), PCWSTR(id as usize as *const u16))
            .ok()
            .or_else(|| LoadCursorW(Some(module), PCWSTR(OLE_COPY as usize as *const u16)).ok())
    }
}
