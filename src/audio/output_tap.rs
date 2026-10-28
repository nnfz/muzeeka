use crate::audio::bass::ffi::DataPump;
use crate::audio::bass::{self, BassLibrary};

const TAP_DSP_PRIORITY: i32 = -1_000_000;

pub struct OutputTapCtx {
    pump: DataPump,
    push_handle: u32,
    max_buffered: u32,
}

unsafe extern "system" fn tap_dsp_callback(
    _handle: bass::DWORD,
    _channel: bass::DWORD,
    buffer: *mut std::ffi::c_void,
    length: bass::DWORD,
    user: *mut std::ffi::c_void,
) {
    if buffer.is_null() || user.is_null() || length == 0 {
        return;
    }
    let ctx = &*(user as *const OutputTapCtx);
    if ctx.push_handle == 0 {
        return;
    }
    if ctx.pump.queued(ctx.push_handle) > ctx.max_buffered {
        return;
    }
    let data = std::slice::from_raw_parts(buffer as *const u8, length as usize);
    ctx.pump.push(ctx.push_handle, data);
}

pub struct OutputTap {
    pub push_handle: u32,
    pub dsp: u32,
    _ctx: Box<OutputTapCtx>,
}

pub fn attach(
    bass: &BassLibrary,
    mixer: u32,
    device: i32,
    volume: f32,
) -> Result<OutputTap, String> {
    if mixer == 0 {
        return Err("mixer is not running".into());
    }

    let mixer_info = bass.channel_get_info(mixer)?;
    let freq = if mixer_info.freq >= 8000 {
        mixer_info.freq
    } else {
        44100
    };
    let chans = mixer_info.chans.max(1);
    let max_buffered = freq.saturating_mul(chans).saturating_mul(4) / 2;

    let previous = bass.get_device();
    bass.set_device(device)?;
    let created = bass
        .stream_create_push(freq, chans, bass::BASS_SAMPLE_FLOAT)
        .and_then(|handle| {
            let _ = bass.channel_set_attribute(handle, bass::BASS_ATTRIB_BUFFER, 0.5);
            bass.channel_set_attribute(handle, bass::BASS_ATTRIB_VOL, volume)
                .and_then(|()| bass.channel_play(handle, false))
                .map(|()| handle)
                .map_err(|err| {
                    let _ = bass.channel_free(handle);
                    err
                })
        });
    let _ = bass.set_device(previous);
    let push_handle = created?;

    let mut ctx = Box::new(OutputTapCtx {
        pump: bass.data_pump(),
        push_handle,
        max_buffered,
    });
    let user = ctx.as_mut() as *mut OutputTapCtx as *mut std::ffi::c_void;

    let dsp = bass
        .channel_set_dsp_ex(
            mixer,
            tap_dsp_callback,
            user,
            TAP_DSP_PRIORITY,
            bass::BASS_DSP_FLOAT,
        )
        .or_else(|_| bass.channel_set_dsp(mixer, tap_dsp_callback, TAP_DSP_PRIORITY, user));

    match dsp {
        Ok(dsp) => Ok(OutputTap {
            push_handle,
            dsp,
            _ctx: ctx,
        }),
        Err(err) => {
            let _ = bass.channel_stop(push_handle);
            let _ = bass.channel_free(push_handle);
            Err(err)
        }
    }
}

pub fn detach(bass: &BassLibrary, mixer: u32, tap: OutputTap) {
    if mixer != 0 && tap.dsp != 0 {
        let _ = bass.channel_remove_dsp(mixer, tap.dsp);
    }
    if tap.push_handle != 0 {
        let _ = bass.channel_stop(tap.push_handle);
        let _ = bass.channel_free(tap.push_handle);
    }
    drop(tap);
}
