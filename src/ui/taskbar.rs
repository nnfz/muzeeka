//! Previous, play, and next on the Windows taskbar thumbnail.
//!
//! The window proc only stores a click. The session tick performs it, so a
//! button never calls back into GPUI while Windows is still inside the message.

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU8, Ordering};

use gpui::Window;
use parking_lot::Mutex;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LRESULT, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, ITaskbarList3, SetWindowSubclass, TaskbarList, THBF_DISABLED, THBF_ENABLED,
    THBN_CLICKED, THB_FLAGS, THB_ICON, THB_TOOLTIP, THUMBBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    LoadImageW, RegisterWindowMessageW, HICON, IMAGE_ICON, LR_LOADFROMFILE, WM_COMMAND,
};

const ID_PREV: u32 = 11;
const ID_TOGGLE: u32 = 12;
const ID_NEXT: u32 = 13;
const ACT_PREV: u8 = 1;
const ACT_TOGGLE: u8 = 2;
const ACT_NEXT: u8 = 3;

static HWND_BITS: AtomicIsize = AtomicIsize::new(0);
static ACTION: AtomicU8 = AtomicU8::new(0);
static NEED_BUTTONS: AtomicBool = AtomicBool::new(false);
static ADDED: AtomicBool = AtomicBool::new(false);
static SUBCLASSED: AtomicBool = AtomicBool::new(false);
static CREATED_MSG: AtomicU32 = AtomicU32::new(0);
static TRIES: AtomicU8 = AtomicU8::new(0);
static LAST_FLAGS: AtomicU8 = AtomicU8::new(0xff);
static ICONS: Mutex<Option<Icons>> = Mutex::new(None);

struct Icons {
    prev: isize,
    prev_off: isize,
    next: isize,
    next_off: isize,
    play: isize,
    pause: isize,
}

pub enum Action {
    Prev,
    Toggle,
    Next,
}

/// Subclass the main window and add the buttons once the taskbar allows it.
pub fn attach(window: &Window) -> bool {
    let Some(hwnd) = hwnd_of(window) else {
        return false;
    };
    HWND_BITS.store(hwnd.0 as isize, Ordering::Relaxed);
    if CREATED_MSG.load(Ordering::Relaxed) == 0 {
        let wide: Vec<u16> = "TaskbarButtonCreated"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let message = unsafe { RegisterWindowMessageW(PCWSTR(wide.as_ptr())) };
        if message != 0 {
            CREATED_MSG.store(message, Ordering::Relaxed);
        }
    }
    if !SUBCLASSED.swap(true, Ordering::AcqRel) {
        let ok = unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), 1, 0) };
        if !ok.as_bool() {
            SUBCLASSED.store(false, Ordering::Relaxed);
            return false;
        }
    }
    NEED_BUTTONS.store(true, Ordering::Relaxed);
    true
}

/// Read a thumbnail click and refresh the icons when playback changes.
pub fn poll(playing: bool, can_play: bool, has_prev: bool, has_next: bool) -> Option<Action> {
    let hwnd = hwnd_bits();
    if hwnd.0.is_null() {
        return take_action();
    }
    if NEED_BUTTONS.load(Ordering::Relaxed) && !ADDED.load(Ordering::Relaxed) {
        let tries = TRIES.fetch_add(1, Ordering::Relaxed);
        if tries < 100 && install(hwnd, playing, can_play, has_prev, has_next) {
            ADDED.store(true, Ordering::Relaxed);
            NEED_BUTTONS.store(false, Ordering::Relaxed);
            LAST_FLAGS.store(
                pack(playing, can_play, has_prev, has_next),
                Ordering::Relaxed,
            );
        }
    } else if ADDED.load(Ordering::Relaxed) {
        let flags = pack(playing, can_play, has_prev, has_next);
        if LAST_FLAGS.swap(flags, Ordering::Relaxed) != flags {
            let _ = update(hwnd, playing, can_play, has_prev, has_next);
        }
    }
    take_action()
}

fn take_action() -> Option<Action> {
    match ACTION.swap(0, Ordering::AcqRel) {
        ACT_PREV => Some(Action::Prev),
        ACT_TOGGLE => Some(Action::Toggle),
        ACT_NEXT => Some(Action::Next),
        _ => None,
    }
}

fn hwnd_bits() -> HWND {
    HWND(HWND_BITS.load(Ordering::Relaxed) as *mut _)
}

fn hwnd_of(window: &Window) -> Option<HWND> {
    // `Window::window_handle` is GPUI's own handle. The raw HWND is the trait method.
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

fn pack(playing: bool, can_play: bool, has_prev: bool, has_next: bool) -> u8 {
    u8::from(playing)
        | (u8::from(can_play) << 1)
        | (u8::from(has_prev) << 2)
        | (u8::from(has_next) << 3)
}

fn install(hwnd: HWND, playing: bool, can_play: bool, has_prev: bool, has_next: bool) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(taskbar) =
            CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
        else {
            return false;
        };
        if taskbar.HrInit().is_err() {
            return false;
        }
        let Some(buttons) = buttons(playing, can_play, has_prev, has_next) else {
            return false;
        };
        // Add once. A later call replaces the set after Explorer restarts.
        taskbar.ThumbBarAddButtons(hwnd, &buttons).is_ok()
    }
}

fn update(hwnd: HWND, playing: bool, can_play: bool, has_prev: bool, has_next: bool) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(taskbar) =
            CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
        else {
            return false;
        };
        if taskbar.HrInit().is_err() {
            return false;
        }
        let Some(buttons) = buttons(playing, can_play, has_prev, has_next) else {
            return false;
        };
        taskbar.ThumbBarUpdateButtons(hwnd, &buttons).is_ok()
    }
}

fn buttons(
    playing: bool,
    can_play: bool,
    has_prev: bool,
    has_next: bool,
) -> Option<[THUMBBUTTON; 3]> {
    let icons = icons()?;
    Some([
        button(
            ID_PREV,
            if has_prev { icons.prev } else { icons.prev_off },
            "Назад",
            has_prev,
        ),
        button(
            ID_TOGGLE,
            if playing { icons.pause } else { icons.play },
            if playing {
                "Пауза"
            } else {
                "Играть"
            },
            can_play,
        ),
        button(
            ID_NEXT,
            if has_next { icons.next } else { icons.next_off },
            "Дальше",
            has_next,
        ),
    ])
}

fn button(id: u32, icon: isize, tip: &str, enabled: bool) -> THUMBBUTTON {
    let mut button = THUMBBUTTON::default();
    button.dwMask = THB_ICON | THB_TOOLTIP | THB_FLAGS;
    button.iId = id;
    button.hIcon = HICON(icon as *mut _);
    button.szTip = tip_units(tip);
    button.dwFlags = if enabled { THBF_ENABLED } else { THBF_DISABLED };
    button
}

fn tip_units(text: &str) -> [u16; 260] {
    let mut units = [0u16; 260];
    for (index, unit) in text.encode_utf16().take(259).enumerate() {
        units[index] = unit;
    }
    units
}

fn icons() -> Option<Icons> {
    let mut slot = ICONS.lock();
    if let Some(icons) = slot.as_ref() {
        return Some(Icons {
            prev: icons.prev,
            prev_off: icons.prev_off,
            next: icons.next,
            next_off: icons.next_off,
            play: icons.play,
            pause: icons.pause,
        });
    }
    let icons = Icons {
        prev: load_icon(
            "prev.ico",
            include_bytes!("../icons/taskbar/PREV_THUMB.ico"),
        )?,
        prev_off: load_icon(
            "prev-off.ico",
            include_bytes!("../icons/taskbar/PREV_THUMB_DISABLED.ico"),
        )?,
        next: load_icon(
            "next.ico",
            include_bytes!("../icons/taskbar/NEXT_THUMB.ico"),
        )?,
        next_off: load_icon(
            "next-off.ico",
            include_bytes!("../icons/taskbar/NEXT_THUMB_DISABLED.ico"),
        )?,
        play: load_icon(
            "play.ico",
            include_bytes!("../icons/taskbar/PLAY_THUMB.ico"),
        )?,
        pause: load_icon(
            "pause.ico",
            include_bytes!("../icons/taskbar/PAUSE_THUMB.ico"),
        )?,
    };
    *slot = Some(Icons {
        prev: icons.prev,
        prev_off: icons.prev_off,
        next: icons.next,
        next_off: icons.next_off,
        play: icons.play,
        pause: icons.pause,
    });
    Some(icons)
}

fn load_icon(name: &str, bytes: &[u8]) -> Option<isize> {
    let path = icon_file(name, bytes)?;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        LoadImageW(
            None,
            PCWSTR(wide.as_ptr()),
            IMAGE_ICON,
            16,
            16,
            LR_LOADFROMFILE,
        )
        .ok()?
    };
    if handle.0.is_null() {
        None
    } else {
        Some(handle.0 as isize)
    }
}

fn icon_file(name: &str, bytes: &[u8]) -> Option<PathBuf> {
    let dir = crate::ui::catalog::app_data_dir().join("taskbar");
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    if !path.is_file() {
        fs::write(&path, bytes).ok()?;
    }
    Some(path)
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let created = CREATED_MSG.load(Ordering::Relaxed);
    if created != 0 && message == created {
        ADDED.store(false, Ordering::Relaxed);
        NEED_BUTTONS.store(true, Ordering::Relaxed);
        TRIES.store(0, Ordering::Relaxed);
    } else if message == WM_COMMAND {
        let word = wparam.0 as u32;
        let id = word & 0xffff;
        let code = (word >> 16) & 0xffff;
        if code == THBN_CLICKED {
            let action = match id {
                ID_PREV => ACT_PREV,
                ID_TOGGLE => ACT_TOGGLE,
                ID_NEXT => ACT_NEXT,
                _ => 0,
            };
            if action != 0 {
                ACTION.store(action, Ordering::Relaxed);
                return LRESULT(0);
            }
        }
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}
