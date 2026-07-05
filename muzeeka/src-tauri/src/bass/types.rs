// BASS audio library constants and types
// Reference: https://www.un4seen.com/doc/

#![allow(dead_code, non_upper_case_globals)]

// ── Handle types ──────────────────────────────────────────────────────────────
/// BASS stream / channel handle (HSTREAM, HCHANNEL, etc.)
pub type HSTREAM = u32;
pub type HCHANNEL = u32;
pub type HPLUGIN = u32;
pub type DWORD = u32;
pub type BOOL = i32;
pub type QWORD = u64;

// ── Error codes ───────────────────────────────────────────────────────────────
pub const BASS_OK: i32 = 0;
pub const BASS_ERROR_MEM: i32 = 1;
pub const BASS_ERROR_FILEOPEN: i32 = 2;
pub const BASS_ERROR_DRIVER: i32 = 3;
pub const BASS_ERROR_BUFLOST: i32 = 4;
pub const BASS_ERROR_HANDLE: i32 = 5;
pub const BASS_ERROR_FORMAT: i32 = 6;
pub const BASS_ERROR_POSITION: i32 = 7;
pub const BASS_ERROR_INIT: i32 = 8;
pub const BASS_ERROR_START: i32 = 9;
pub const BASS_ERROR_ALREADY: i32 = 14;
pub const BASS_ERROR_NOTAUDIO: i32 = 17;
pub const BASS_ERROR_NOCHAN: i32 = 18;
pub const BASS_ERROR_ILLTYPE: i32 = 19;
pub const BASS_ERROR_ILLPARAM: i32 = 20;
pub const BASS_ERROR_NO3D: i32 = 21;
pub const BASS_ERROR_NOEAX: i32 = 22;
pub const BASS_ERROR_DEVICE: i32 = 23;
pub const BASS_ERROR_NOPLAY: i32 = 24;
pub const BASS_ERROR_FREQ: i32 = 25;
pub const BASS_ERROR_NOTFILE: i32 = 27;
pub const BASS_ERROR_NOHW: i32 = 29;
pub const BASS_ERROR_EMPTY: i32 = 31;
pub const BASS_ERROR_NONET: i32 = 32;
pub const BASS_ERROR_CREATE: i32 = 33;
pub const BASS_ERROR_NOFX: i32 = 34;
pub const BASS_ERROR_NOTAVAIL: i32 = 37;
pub const BASS_ERROR_DECODE: i32 = 38;
pub const BASS_ERROR_DX: i32 = 39;
pub const BASS_ERROR_TIMEOUT: i32 = 40;
pub const BASS_ERROR_FILEFORM: i32 = 41;
pub const BASS_ERROR_SPEAKER: i32 = 42;
pub const BASS_ERROR_VERSION: i32 = 43;
pub const BASS_ERROR_CODEC: i32 = 44;
pub const BASS_ERROR_ENDED: i32 = 45;
pub const BASS_ERROR_BUSY: i32 = 46;
pub const BASS_ERROR_UNKNOWN: i32 = -1;

// ── Stream flags ──────────────────────────────────────────────────────────────
pub const BASS_STREAM_PRESCAN: DWORD = 0x20000;
pub const BASS_STREAM_AUTOFREE: DWORD = 0x40000;
pub const BASS_STREAM_DECODE: DWORD = 0x200000;
pub const BASS_UNICODE: DWORD = 0x80000000;
pub const BASS_SAMPLE_FLOAT: DWORD = 256;

// ── Position mode ─────────────────────────────────────────────────────────────
pub const BASS_POS_BYTE: DWORD = 0;

// ── Active states ─────────────────────────────────────────────────────────────
pub const BASS_ACTIVE_STOPPED: DWORD = 0;
pub const BASS_ACTIVE_PLAYING: DWORD = 1;
pub const BASS_ACTIVE_STALLED: DWORD = 2;
pub const BASS_ACTIVE_PAUSED: DWORD = 3;
pub const BASS_ACTIVE_PAUSED_DEVICE: DWORD = 4;

// ── Channel attributes ───────────────────────────────────────────────────────
pub const BASS_ATTRIB_FREQ: DWORD = 1;
pub const BASS_ATTRIB_VOL: DWORD = 2;
pub const BASS_ATTRIB_PAN: DWORD = 3;

// ── Config options ────────────────────────────────────────────────────────────
pub const BASS_CONFIG_FLOATDSP: DWORD = 46;

// ── DSP ───────────────────────────────────────────────────────────────────────
pub type HDSP = DWORD;
pub type DspProc = unsafe extern "system" fn(
    handle: DWORD,
    channel: DWORD,
    buffer: *mut std::ffi::c_void,
    length: DWORD,
    user: *mut std::ffi::c_void,
);

pub const BASS_DSP_PRIORITY_USER: i32 = 0;
pub const BASS_DSP_PRIORITY_FIRST: i32 = 2147483647;

pub const BASS_DSP_FLOAT: DWORD = 0x400;

// ── Mixer (bassmix) ─────────────────────────────────────────────────────────
pub const BASS_MIXER_END: DWORD = 0x10000;
pub const BASS_MIXER_NONSTOP: DWORD = 0x200;
pub const BASS_MIXER_QUEUE: DWORD = 0x8000;
pub const BASS_MIXER_RESUME: DWORD = 0x1000;
pub const BASS_MIXER_CHAN_NORAMPIN: DWORD = 0x800000;
pub const BASS_MIXER_CHAN_BUFFER: DWORD = 0x2000;
pub const BASS_MIXER_CHAN_PAUSE: DWORD = 0x20000;

// ── BASS_CHANNELINFO ──────────────────────────────────────────────────────────
#[repr(C)]
#[derive(Debug, Clone)]
pub struct BassChannelInfo {
    pub freq: DWORD,
    pub chans: DWORD,
    pub flags: DWORD,
    pub ctype: DWORD,
    pub origres: DWORD,
    pub plugin: DWORD,
    pub sample: DWORD,
    pub filename: *const u16,
}

impl Default for BassChannelInfo {
    fn default() -> Self {
        Self {
            freq: 0,
            chans: 0,
            flags: 0,
            ctype: 0,
            origres: 0,
            plugin: 0,
            sample: 0,
            filename: std::ptr::null(),
        }
    }
}

// Safety: BassChannelInfo is only used behind a Mutex and never shared across threads
// while a raw pointer is live.
unsafe impl Send for BassChannelInfo {}
unsafe impl Sync for BassChannelInfo {}

/// Human-readable error description
pub fn bass_error_to_string(code: i32) -> &'static str {
    match code {
        BASS_OK => "OK",
        BASS_ERROR_MEM => "memory error",
        BASS_ERROR_FILEOPEN => "can't open the file",
        BASS_ERROR_DRIVER => "can't find a free/valid driver",
        BASS_ERROR_BUFLOST => "the sample buffer was lost",
        BASS_ERROR_HANDLE => "invalid handle",
        BASS_ERROR_FORMAT => "unsupported sample format",
        BASS_ERROR_POSITION => "invalid position",
        BASS_ERROR_INIT => "BASS_Init has not been successfully called",
        BASS_ERROR_START => "BASS_Start has not been successfully called",
        BASS_ERROR_ALREADY => "already initialized/paused/whatever",
        BASS_ERROR_NOTAUDIO => "file does not contain audio",
        BASS_ERROR_NOCHAN => "can't get a free channel",
        BASS_ERROR_ILLTYPE => "illegal type",
        BASS_ERROR_ILLPARAM => "illegal parameter",
        BASS_ERROR_DEVICE => "illegal device number",
        BASS_ERROR_NOPLAY => "not playing",
        BASS_ERROR_FREQ => "illegal sample rate",
        BASS_ERROR_NOTFILE => "not a file stream",
        BASS_ERROR_NOHW => "no hardware voices available",
        BASS_ERROR_EMPTY => "the file has no sample data",
        BASS_ERROR_NONET => "no internet connection",
        BASS_ERROR_CREATE => "couldn't create the file",
        BASS_ERROR_NOFX => "effects are not available",
        BASS_ERROR_NOTAVAIL => "requested data/action is not available",
        BASS_ERROR_DECODE => "the channel is a decoding channel",
        BASS_ERROR_TIMEOUT => "connection timed out",
        BASS_ERROR_FILEFORM => "unsupported file format",
        BASS_ERROR_SPEAKER => "unavailable speaker",
        BASS_ERROR_VERSION => "invalid BASS version",
        BASS_ERROR_CODEC => "codec is not available/supported",
        BASS_ERROR_ENDED => "the channel/file has ended",
        BASS_ERROR_BUSY => "the device is busy",
        _ => "unknown error",
    }
}
