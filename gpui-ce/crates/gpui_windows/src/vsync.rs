use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use gpui_util::ResultExt;
use windows::{
    Win32::{
        Foundation::{HANDLE, HMODULE, HWND},
        Graphics::Dwm::{DWM_TIMING_INFO, DwmFlush, DwmGetCompositionTimingInfo},
        System::{
            LibraryLoader::{GetProcAddress, LoadLibraryA},
            Performance::QueryPerformanceFrequency,
        },
    },
    core::{BOOL, HRESULT, s},
};

static QPC_TICKS_PER_SECOND: LazyLock<u64> = LazyLock::new(|| {
    let mut frequency = 0;
    // On systems that run Windows XP or later, the function will always succeed and
    // will thus never return zero.
    unsafe { QueryPerformanceFrequency(&mut frequency).unwrap() };
    frequency as u64
});

const VSYNC_INTERVAL_THRESHOLD: Duration = Duration::from_millis(1);
const DEFAULT_VSYNC_INTERVAL: Duration = Duration::from_micros(16_666); // ~60Hz
// `DCompositionWaitForCompositorClock(0, null, INFINITE)` reports the clock
// tick as `WAIT_OBJECT_0 + count`, which is 0 when no extra handles are passed.
const WAIT_OBJECT_0: u32 = 0;
const INFINITE_TIMEOUT: u32 = u32::MAX;

type CompositorClockWait =
    unsafe extern "system" fn(count: u32, handles: *const HANDLE, timeout_ms: u32) -> u32;
type BoostCompositorClock = unsafe extern "system" fn(enable: BOOL) -> HRESULT;

pub(crate) struct VSyncProvider {
    interval: Duration,
    f: Box<dyn Fn() -> bool>,
}

impl VSyncProvider {
    pub(crate) fn new() -> Self {
        let interval = get_dwm_interval()
            .context("Failed to get DWM interval")
            .log_err()
            .unwrap_or(DEFAULT_VSYNC_INTERVAL);
        // `DwmFlush` on this background thread waits two vblanks, so a 144 Hz
        // display presents at ~72. The compositor clock follows the real refresh,
        // including dynamic refresh. The symbols are missing on Windows 10, and a
        // static import refuses to start there, so resolve them at runtime.
        let f: Box<dyn Fn() -> bool> = if let Some(wait) = load_compositor_clock() {
            match load_refresh_boost() {
                Some(boost) if unsafe { boost(true.into()) }.is_ok() => {
                    log::info!("vsync: compositor clock, refresh boost on");
                }
                Some(_) => log::info!("vsync: compositor clock (refresh boost failed)"),
                None => log::info!("vsync: compositor clock"),
            }
            Box::new(move || unsafe {
                wait(0, std::ptr::null(), INFINITE_TIMEOUT) == WAIT_OBJECT_0
            })
        } else {
            log::info!("vsync: DwmFlush");
            Box::new(|| unsafe { DwmFlush().is_ok() })
        };
        Self { interval, f }
    }

    pub(crate) fn wait_for_vsync(&self) {
        let vsync_start = Instant::now();
        let wait_succeeded = (self.f)();
        let elapsed = vsync_start.elapsed();
        // DwmFlush and DCompositionWaitForCompositorClock returns very early
        // instead of waiting until vblank when the monitor goes to sleep or is
        // unplugged (nothing to present due to desktop occlusion). We use 1ms as
        // a threshold for the duration of the wait functions and fallback to
        // Sleep() if it returns before that. This could happen during normal
        // operation for the first call after the vsync thread becomes non-idle,
        // but it shouldn't happen often.
        if !wait_succeeded || elapsed < VSYNC_INTERVAL_THRESHOLD {
            log::trace!("VSyncProvider::wait_for_vsync() took less time than expected");
            std::thread::sleep(self.interval);
        }
    }
}

fn dcomp_module() -> Option<HMODULE> {
    // The refcount is left raised. The function pointers are used for the
    // process lifetime, and the renderer already keeps dcomp.dll loaded.
    unsafe { LoadLibraryA(s!("dcomp.dll")).ok() }
}

fn load_dcomp_symbol(name: windows::core::PCSTR) -> Option<unsafe extern "system" fn() -> isize> {
    let module = dcomp_module()?;
    unsafe { GetProcAddress(module, name) }
}

fn load_compositor_clock() -> Option<CompositorClockWait> {
    let symbol = load_dcomp_symbol(s!("DCompositionWaitForCompositorClock"))?;
    Some(unsafe { std::mem::transmute(symbol) })
}

fn load_refresh_boost() -> Option<BoostCompositorClock> {
    let symbol = load_dcomp_symbol(s!("DCompositionBoostCompositorClock"))?;
    Some(unsafe { std::mem::transmute(symbol) })
}

fn get_dwm_interval() -> Result<Duration> {
    let mut timing_info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing_info) }?;
    let interval = retrieve_duration(timing_info.qpcRefreshPeriod, *QPC_TICKS_PER_SECOND);
    // Check for interval values that are impossibly low. A 29 microsecond
    // interval was seen (from a qpcRefreshPeriod of 60).
    if interval < VSYNC_INTERVAL_THRESHOLD {
        Ok(retrieve_duration(
            timing_info.rateRefresh.uiDenominator as u64,
            timing_info.rateRefresh.uiNumerator as u64,
        ))
    } else {
        Ok(interval)
    }
}

#[inline]
fn retrieve_duration(counts: u64, ticks_per_second: u64) -> Duration {
    let ticks_per_microsecond = ticks_per_second / 1_000_000;
    Duration::from_micros(counts / ticks_per_microsecond)
}
