use std::ffi::c_void;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::muzeeka_plugin::MuzeekaHost;
use crate::{log, STOP};

const BASS_SAMPLE_FLOAT: u32 = 0x100;
/// BASSmix: `BASS_CTYPE_STREAM_MIXER`.
const BASS_CTYPE_STREAM_MIXER: u32 = 0x10800;
const IDLE_SILENCE_AFTER: Duration = Duration::from_millis(500);
const SILENCE_STEP: Duration = Duration::from_millis(20);
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// Lower than the EQ rack (`i32::MAX`) and the extra-output tap (`-1_000_000`).
/// `BASS_ATTRIB_VOL` is applied after every DSP, so this copy is full level.
const DSP_PRIORITY: i32 = -1_100_000;

#[repr(C)]
struct ChannelInfo {
    freq: u32,
    chans: u32,
    flags: u32,
    ctype: u32,
    origres: u32,
    plugin: u32,
    sample: u32,
    filename: *const c_char,
}

type DspProc = unsafe extern "system" fn(u32, u32, *mut c_void, u32, *mut c_void);

struct Api {
    get_info: unsafe extern "system" fn(u32, *mut ChannelInfo) -> i32,
    set_dsp: unsafe extern "system" fn(u32, DspProc, *mut c_void, i32) -> u32,
    remove_dsp: unsafe extern "system" fn(u32, u32) -> i32,
    error_code: unsafe extern "system" fn() -> i32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
}

#[cfg(unix)]
extern "C" {
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

#[cfg(windows)]
unsafe fn find_symbol(name: &'static [u8]) -> *mut c_void {
    let module = GetModuleHandleA(b"bass.dll\0".as_ptr() as *const c_char);
    if module.is_null() {
        return std::ptr::null_mut();
    }
    GetProcAddress(module, name.as_ptr() as *const c_char)
}

#[cfg(unix)]
unsafe fn find_symbol(name: &'static [u8]) -> *mut c_void {
    dlsym(std::ptr::null_mut(), name.as_ptr() as *const c_char)
}

unsafe fn load<T: Copy>(name: &'static [u8]) -> Result<T, String> {
    let ptr = find_symbol(name);
    if ptr.is_null() {
        let shown = String::from_utf8_lossy(&name[..name.len() - 1]).into_owned();
        return Err(format!("{shown} not found: bass.dll is not loaded in Muzeeka"));
    }
    Ok(std::mem::transmute_copy(&ptr))
}

impl Api {
    fn load() -> Result<Api, String> {
        unsafe {
            Ok(Api {
                get_info: load(b"BASS_ChannelGetInfo\0")?,
                set_dsp: load(b"BASS_ChannelSetDSP\0")?,
                remove_dsp: load(b"BASS_ChannelRemoveDSP\0")?,
                error_code: load(b"BASS_ErrorGetCode\0")?,
            })
        }
    }
}

static TX: Mutex<Option<SyncSender<Vec<f32>>>> = Mutex::new(None);
static POOL: Mutex<Vec<Vec<f32>>> = Mutex::new(Vec::new());
static CHANS: AtomicU32 = AtomicU32::new(0);
static IS_FLOAT: AtomicBool = AtomicBool::new(true);
/// Set from the mixer DSP. The capture thread clears it once a second.
/// If it stays clear, the DSP was lost (mixer recreated under the same handle).
static GOT_DATA: AtomicBool = AtomicBool::new(false);

const POOL_MAX: usize = 32;
const CHUNK_CAP: usize = 16_384;

fn take_buf(samples: usize) -> Option<Vec<f32>> {
    let mut pool = POOL.try_lock().ok()?;
    let mut buf = pool.pop().unwrap_or_else(|| Vec::with_capacity(samples));
    if buf.capacity() < samples {
        buf.reserve(samples - buf.capacity());
    }
    buf.clear();
    Some(buf)
}

fn recycle(buf: Vec<f32>) {
    if let Ok(mut pool) = POOL.try_lock() {
        if pool.len() < POOL_MAX {
            pool.push(buf);
        }
    }
}

fn fill_pool() {
    let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
    while pool.len() < POOL_MAX {
        pool.push(Vec::with_capacity(CHUNK_CAP));
    }
}

unsafe extern "system" fn dsp_callback(
    _handle: u32,
    _channel: u32,
    buffer: *mut c_void,
    length: u32,
    _user: *mut c_void,
) {
    if buffer.is_null() || length == 0 {
        return;
    }
    let chans = CHANS.load(Ordering::Relaxed) as usize;
    if chans == 0 {
        return;
    }
    let float_samples = IS_FLOAT.load(Ordering::Relaxed);
    let total = if float_samples {
        length as usize / 4
    } else {
        length as usize / 2
    };
    let frames = total / chans;
    if frames == 0 {
        return;
    }
    // Count the callback even if this chunk is dropped: the DSP is still alive.
    GOT_DATA.store(true, Ordering::Release);
    let Some(mut out) = take_buf(frames * 2) else {
        return;
    };
    if float_samples {
        let src = std::slice::from_raw_parts(buffer as *const f32, total);
        for f in 0..frames {
            let left = src[f * chans];
            let right = if chans > 1 { src[f * chans + 1] } else { left };
            out.push(left);
            out.push(right);
        }
    } else {
        let src = std::slice::from_raw_parts(buffer as *const i16, total);
        for f in 0..frames {
            let left = src[f * chans] as f32 / 32768.0;
            let right = if chans > 1 {
                src[f * chans + 1] as f32 / 32768.0
            } else {
                left
            };
            out.push(left);
            out.push(right);
        }
    }
    if let Ok(guard) = TX.try_lock() {
        if let Some(tx) = guard.as_ref() {
            if let Err(TrySendError::Full(buf) | TrySendError::Disconnected(buf)) = tx.try_send(out)
            {
                recycle(buf);
            }
            return;
        }
    }
    recycle(out);
}

struct Attached {
    mixer: u32,
    dsp: u32,
    rate: u32,
}

fn channel_info(api: &Api, handle: u32) -> Option<ChannelInfo> {
    let mut info = unsafe { std::mem::zeroed::<ChannelInfo>() };
    if unsafe { (api.get_info)(handle, &mut info) } == 0 {
        return None;
    }
    Some(info)
}

fn is_mixer(api: &Api, handle: u32) -> bool {
    channel_info(api, handle).is_some_and(|info| info.ctype == BASS_CTYPE_STREAM_MIXER)
}

fn find_mixer(api: &Api, hint: Option<u32>) -> Option<u32> {
    if let Some(handle) = hint {
        if is_mixer(api, handle) {
            return Some(handle);
        }
    }
    for index in 1..8192u32 {
        for handle in [index | 0x8000_0000, index] {
            if is_mixer(api, handle) {
                return Some(handle);
            }
        }
    }
    None
}

fn attach(api: &Api, mixer: u32) -> Result<Attached, String> {
    let info = channel_info(api, mixer).ok_or("player mixer disappeared")?;
    if info.chans == 0 || info.freq == 0 {
        return Err("player mixer has no format".into());
    }
    CHANS.store(info.chans, Ordering::SeqCst);
    IS_FLOAT.store(info.flags & BASS_SAMPLE_FLOAT != 0, Ordering::SeqCst);
    let dsp = unsafe { (api.set_dsp)(mixer, dsp_callback, std::ptr::null_mut(), DSP_PRIORITY) };
    if dsp == 0 {
        return Err(format!(
            "BASS_ChannelSetDSP failed, BASS error {}",
            unsafe { (api.error_code)() }
        ));
    }
    Ok(Attached {
        mixer,
        dsp,
        rate: info.freq,
    })
}

fn detach(api: &Api, attached: &mut Option<Attached>) {
    if let Some(a) = attached.take() {
        unsafe { (api.remove_dsp)(a.mixer, a.dsp) };
    }
}

fn ensure(api: &Api, host: &MuzeekaHost, attached: &mut Option<Attached>) -> Result<(), String> {
    let hint = attached.as_ref().map(|a| a.mixer);
    let Some(mixer) = find_mixer(api, hint) else {
        detach(api, attached);
        return Err("player mixer is not running yet".into());
    };
    if let Some(a) = attached.as_ref() {
        let same = a.mixer == mixer
            && channel_info(api, mixer)
                .is_some_and(|info| info.freq == a.rate && info.chans > 0)
            && GOT_DATA.swap(false, Ordering::AcqRel);
        if same {
            return Ok(());
        }
    }
    let prev = attached.as_ref().map(|a| (a.mixer, a.rate));
    detach(api, attached);
    let fresh = attach(api, mixer)?;
    // Measure only callbacks that happen after this attach.
    GOT_DATA.store(false, Ordering::Release);
    if prev != Some((fresh.mixer, fresh.rate)) {
        log(
            host,
            "info",
            &format!(
                "capturing mixer {mixer:#x} before the volume slider, {} Hz",
                fresh.rate
            ),
        );
    }
    *attached = Some(fresh);
    Ok(())
}

pub fn run(host: &MuzeekaHost, sink: &mut dyn FnMut(u32, &[f32])) -> Result<(), String> {
    let api = Api::load()?;
    fill_pool();
    GOT_DATA.store(false, Ordering::Release);
    let (tx, rx) = sync_channel::<Vec<f32>>(128);
    *TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);

    let mut attached: Option<Attached> = None;
    let mut last_check = Instant::now() - CHECK_EVERY;
    let mut last_data = Instant::now();
    let mut silence: Vec<f32> = Vec::new();

    let result = loop {
        if STOP.load(Ordering::SeqCst) {
            break Ok(());
        }
        if last_check.elapsed() >= CHECK_EVERY {
            last_check = Instant::now();
            if let Err(err) = ensure(&api, host, &mut attached) {
                break Err(err);
            }
        }
        match rx.recv_timeout(SILENCE_STEP) {
            Ok(chunk) => {
                if let Some(a) = attached.as_ref() {
                    sink(a.rate, &chunk);
                    last_data = Instant::now();
                }
                recycle(chunk);
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(a) = attached.as_ref() {
                    if last_data.elapsed() >= IDLE_SILENCE_AFTER {
                        let frames = (SILENCE_STEP.as_secs_f64() * a.rate as f64) as usize;
                        silence.clear();
                        silence.resize(frames * 2, 0.0);
                        sink(a.rate, &silence);
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break Err("capture channel closed".into()),
        }
    };

    detach(&api, &mut attached);
    *TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    result
}
