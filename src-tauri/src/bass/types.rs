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

// ── Error codes (raw BASS_ErrorGetCode values) ────────────────────────────────
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

/// Typed BASS error for matching without parsing strings.
///
/// Raw `BASS_ERROR_*` constants remain for documentation and FFI comparison;
/// prefer this enum at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BassError {
    Ok,
    Mem,
    FileOpen,
    Driver,
    BufLost,
    Handle,
    Format,
    Position,
    Init,
    Start,
    Already,
    NotAudio,
    NoChan,
    IllType,
    IllParam,
    No3d,
    NoEax,
    Device,
    NoPlay,
    Freq,
    NotFile,
    NoHw,
    Empty,
    NoNet,
    Create,
    NoFx,
    NotAvail,
    Decode,
    Dx,
    Timeout,
    FileForm,
    Speaker,
    Version,
    Codec,
    Ended,
    Busy,
    Unknown(i32),
}

impl BassError {
    pub fn code(self) -> i32 {
        match self {
            Self::Ok => BASS_OK,
            Self::Mem => BASS_ERROR_MEM,
            Self::FileOpen => BASS_ERROR_FILEOPEN,
            Self::Driver => BASS_ERROR_DRIVER,
            Self::BufLost => BASS_ERROR_BUFLOST,
            Self::Handle => BASS_ERROR_HANDLE,
            Self::Format => BASS_ERROR_FORMAT,
            Self::Position => BASS_ERROR_POSITION,
            Self::Init => BASS_ERROR_INIT,
            Self::Start => BASS_ERROR_START,
            Self::Already => BASS_ERROR_ALREADY,
            Self::NotAudio => BASS_ERROR_NOTAUDIO,
            Self::NoChan => BASS_ERROR_NOCHAN,
            Self::IllType => BASS_ERROR_ILLTYPE,
            Self::IllParam => BASS_ERROR_ILLPARAM,
            Self::No3d => BASS_ERROR_NO3D,
            Self::NoEax => BASS_ERROR_NOEAX,
            Self::Device => BASS_ERROR_DEVICE,
            Self::NoPlay => BASS_ERROR_NOPLAY,
            Self::Freq => BASS_ERROR_FREQ,
            Self::NotFile => BASS_ERROR_NOTFILE,
            Self::NoHw => BASS_ERROR_NOHW,
            Self::Empty => BASS_ERROR_EMPTY,
            Self::NoNet => BASS_ERROR_NONET,
            Self::Create => BASS_ERROR_CREATE,
            Self::NoFx => BASS_ERROR_NOFX,
            Self::NotAvail => BASS_ERROR_NOTAVAIL,
            Self::Decode => BASS_ERROR_DECODE,
            Self::Dx => BASS_ERROR_DX,
            Self::Timeout => BASS_ERROR_TIMEOUT,
            Self::FileForm => BASS_ERROR_FILEFORM,
            Self::Speaker => BASS_ERROR_SPEAKER,
            Self::Version => BASS_ERROR_VERSION,
            Self::Codec => BASS_ERROR_CODEC,
            Self::Ended => BASS_ERROR_ENDED,
            Self::Busy => BASS_ERROR_BUSY,
            Self::Unknown(c) => c,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Mem => "memory error",
            Self::FileOpen => "can't open the file",
            Self::Driver => "can't find a free/valid driver",
            Self::BufLost => "the sample buffer was lost",
            Self::Handle => "invalid handle",
            Self::Format => "unsupported sample format",
            Self::Position => "invalid position",
            Self::Init => "BASS_Init has not been successfully called",
            Self::Start => "BASS_Start has not been successfully called",
            Self::Already => "already initialized/paused/whatever",
            Self::NotAudio => "file does not contain audio",
            Self::NoChan => "can't get a free channel",
            Self::IllType => "illegal type",
            Self::IllParam => "illegal parameter",
            Self::No3d => "no 3D support",
            Self::NoEax => "no EAX support",
            Self::Device => "illegal device number",
            Self::NoPlay => "not playing",
            Self::Freq => "illegal sample rate",
            Self::NotFile => "not a file stream",
            Self::NoHw => "no hardware voices available",
            Self::Empty => "the file has no sample data",
            Self::NoNet => "no internet connection",
            Self::Create => "couldn't create the file",
            Self::NoFx => "effects are not available",
            Self::NotAvail => "requested data/action is not available",
            Self::Decode => "the channel is a decoding channel",
            Self::Dx => "a sufficient DirectX version is not installed",
            Self::Timeout => "connection timed out",
            Self::FileForm => "unsupported file format",
            Self::Speaker => "unavailable speaker",
            Self::Version => "invalid BASS version",
            Self::Codec => "codec is not available/supported",
            Self::Ended => "the channel/file has ended",
            Self::Busy => "the device is busy",
            Self::Unknown(_) => "unknown error",
        }
    }
}

impl From<i32> for BassError {
    fn from(code: i32) -> Self {
        match code {
            BASS_OK => Self::Ok,
            BASS_ERROR_MEM => Self::Mem,
            BASS_ERROR_FILEOPEN => Self::FileOpen,
            BASS_ERROR_DRIVER => Self::Driver,
            BASS_ERROR_BUFLOST => Self::BufLost,
            BASS_ERROR_HANDLE => Self::Handle,
            BASS_ERROR_FORMAT => Self::Format,
            BASS_ERROR_POSITION => Self::Position,
            BASS_ERROR_INIT => Self::Init,
            BASS_ERROR_START => Self::Start,
            BASS_ERROR_ALREADY => Self::Already,
            BASS_ERROR_NOTAUDIO => Self::NotAudio,
            BASS_ERROR_NOCHAN => Self::NoChan,
            BASS_ERROR_ILLTYPE => Self::IllType,
            BASS_ERROR_ILLPARAM => Self::IllParam,
            BASS_ERROR_NO3D => Self::No3d,
            BASS_ERROR_NOEAX => Self::NoEax,
            BASS_ERROR_DEVICE => Self::Device,
            BASS_ERROR_NOPLAY => Self::NoPlay,
            BASS_ERROR_FREQ => Self::Freq,
            BASS_ERROR_NOTFILE => Self::NotFile,
            BASS_ERROR_NOHW => Self::NoHw,
            BASS_ERROR_EMPTY => Self::Empty,
            BASS_ERROR_NONET => Self::NoNet,
            BASS_ERROR_CREATE => Self::Create,
            BASS_ERROR_NOFX => Self::NoFx,
            BASS_ERROR_NOTAVAIL => Self::NotAvail,
            BASS_ERROR_DECODE => Self::Decode,
            BASS_ERROR_DX => Self::Dx,
            BASS_ERROR_TIMEOUT => Self::Timeout,
            BASS_ERROR_FILEFORM => Self::FileForm,
            BASS_ERROR_SPEAKER => Self::Speaker,
            BASS_ERROR_VERSION => Self::Version,
            BASS_ERROR_CODEC => Self::Codec,
            BASS_ERROR_ENDED => Self::Ended,
            BASS_ERROR_BUSY => Self::Busy,
            other => Self::Unknown(other),
        }
    }
}

impl std::fmt::Display for BassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(c) => write!(f, "BASS error {c}: unknown error"),
            other => write!(f, "BASS error {}: {}", other.code(), other.message()),
        }
    }
}

impl std::error::Error for BassError {}

/// Human-readable error description for a raw BASS error code.
pub fn bass_error_to_string(code: i32) -> &'static str {
    BassError::from(code).message()
}

// ── Stream flags ──────────────────────────────────────────────────────────────
pub const BASS_STREAM_PRESCAN: DWORD = 0x20000;
pub const BASS_STREAM_AUTOFREE: DWORD = 0x40000;
pub const BASS_STREAM_DECODE: DWORD = 0x200000;
pub const BASS_UNICODE: DWORD = 0x80000000;
pub const BASS_SAMPLE_FLOAT: DWORD = 256;
/// Downmix to mono when creating a stream (good for analysis).
pub const BASS_SAMPLE_MONO: DWORD = 2;

// ── ChannelGetData flags ─────────────────────────────────────────────────────
/// Request floating-point PCM (OR with byte length).
pub const BASS_DATA_FLOAT: DWORD = 0x4000_0000;

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
pub const BASS_ATTRIB_BUFFER: DWORD = 13;

// ── Config options ────────────────────────────────────────────────────────────
pub const BASS_CONFIG_FLOATDSP: DWORD = 46;
pub const BASS_CONFIG_BUFFER: DWORD = 0;
pub const BASS_CONFIG_UPDATEPERIOD: DWORD = 1;

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
pub const BASS_DSP_PRIORITY_FIRST: i32 = i32::MAX;

pub const BASS_DSP_FLOAT: DWORD = 0x400;

// ── FX (bass_fx) ────────────────────────────────────────────────────────────
pub const BASS_FX_FREESOURCE: DWORD = 0x10000;
pub const BASS_ATTRIB_TEMPO: DWORD = 0x10000;
pub const BASS_ATTRIB_TEMPO_PITCH: DWORD = 0x10001;
pub const BASS_ATTRIB_TEMPO_FREQ: DWORD = 0x10002;
/// SoundTouch option: reduce clicks when tempo changes (TRUE/FALSE as 1.0/0.0).
pub const BASS_ATTRIB_TEMPO_OPTION_PREVENT_CLICK: DWORD = 0x10016;

// ── Mixer (bassmix) ─────────────────────────────────────────────────────────
pub const BASS_MIXER_END: DWORD = 0x10000;
pub const BASS_MIXER_NONSTOP: DWORD = 0x200;
pub const BASS_MIXER_QUEUE: DWORD = 0x8000;
pub const BASS_MIXER_RESUME: DWORD = 0x1000;
pub const BASS_MIXER_CHAN_NORAMPIN: DWORD = 0x800000;
pub const BASS_MIXER_CHAN_BUFFER: DWORD = 0x2000;
pub const BASS_MIXER_CHAN_PAUSE: DWORD = 0x20000;

// ── Music (modules / trackers) ────────────────────────────────────────────────
pub const BASS_MUSIC_DECODE: DWORD = 0x200000;
pub const BASS_MUSIC_RAMPS: DWORD = 0x400;
pub const BASS_MUSIC_RAMP: DWORD = 0x200;
pub const BASS_MUSIC_PRESCAN: DWORD = 0x2000;

// ── BASS_CHANNELINFO ──────────────────────────────────────────────────────────
// Contains a raw `filename` pointer owned by BASS. Intentionally !Send/!Sync so
// the compiler keeps it on the calling thread (always under the player mutex).
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
