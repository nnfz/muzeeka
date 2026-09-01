//! Tapping the mixer output onto a second device.
//!
//! `BASS_Split_StreamCreate` is not usable here: a splitter needs a decode source, but the
//! player's mixer is a playing channel (`create_mixer` does not set `BASS_STREAM_DECODE`
//! and calls `channel_play` right away), so the split failed with error 38 "the channel is
//! a decoding channel".
//!
//! Instead a DSP is attached to the mixer, copying samples into a push stream created on
//! the target device. The mixer buffer itself is left untouched, so the main output is not
//! affected at all.

use crate::bass::ffi::DataPump;
use crate::bass::{self, BassLibrary};

/// The tap sits last in the DSP chain, so the copy leaves after the rack, the equalizer
/// and the limiter — voice gets exactly what the headphones get. `BASS_DSP_PRIORITY_FIRST`
/// is the highest priority, i.e. "first"; we want the opposite end of the scale.
const TAP_DSP_PRIORITY: i32 = -1_000_000;

/// Cap on unconsumed audio in the push stream (44100 × 2ch × f32 ≈ 0.5 s).
/// If the device falls behind, the excess is dropped — otherwise latency grows forever.
const MAX_BUFFERED_BYTES: u32 = 176_400;

/// Lives in `ExtraOutput`; a pointer to it is handed to the DSP as `user`.
/// The DSP must be removed before this struct is dropped.
pub struct OutputTapCtx {
    pump: DataPump,
    push_handle: u32,
}

/// Copies the mixer buffer into the push stream. Does not modify the buffer.
///
/// The data is always float: the mixer is created with `BASS_SAMPLE_FLOAT` and
/// `BASS_CONFIG_FLOATDSP` is enabled before `BASS_Init`, so the buffer format matches the
/// push stream format whether `channel_set_dsp_ex` succeeded or the fallback kicked in.
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
    // The device is not keeping up — skip this chunk instead of accumulating latency.
    if ctx.pump.queued(ctx.push_handle) > MAX_BUFFERED_BYTES {
        return;
    }
    let data = std::slice::from_raw_parts(buffer as *const u8, length as usize);
    ctx.pump.push(ctx.push_handle, data);
}

pub struct OutputTap {
    pub push_handle: u32,
    pub dsp: u32,
    /// Kept alive for the pointer held by the DSP. Drop order matters: remove the DSP
    /// first, then release the context.
    _ctx: Box<OutputTapCtx>,
}

/// Creates a push stream on `device` and attaches the copying DSP to `mixer`.
/// `volume` (0.0–1.0) applies to the push stream, i.e. to this device only.
pub fn attach(
    bass: &BassLibrary,
    mixer: u32,
    device: i32,
    volume: f32,
) -> Result<OutputTap, String> {
    if mixer == 0 {
        return Err("mixer is not running".into());
    }

    // A push stream belongs to whichever device is current when it is created, so switch
    // over, create it, and switch back.
    let previous = bass.get_device();
    bass.set_device(device)?;
    let created = bass
        .stream_create_push(44100, 2, bass::BASS_SAMPLE_FLOAT)
        .and_then(|handle| {
            // Volume before play, otherwise the first milliseconds go out at full level.
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

/// Removes the DSP and frees the push stream. The DSP goes first: after that the callback
/// can no longer touch the freed handle or the context.
///
/// Pass `mixer` as 0 if the mixer is already gone — the DSP died with it.
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
