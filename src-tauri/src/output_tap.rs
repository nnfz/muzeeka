//! Отвод звука мижера на второе устройство.
//!
//! `BASS_Split_StreamCreate` здесь неприменим: сплиттер требует decode-источник, а мижер
//! в плеере играющий (`create_mixer` не ставит `BASS_STREAM_DECODE` и сразу зовёт
//! `channel_play`), поэтому сплит падал с ошибкой 38 «the channel is a decoding channel».
//!
//! Вместо этого на мижер вешается DSP, который копирует сэмплы в push-поток, созданный на
//! целевом устройстве. Буфер мижера при этом не изменяется, так что основной выход не
//! затрагивается вообще.

use crate::bass::ffi::DataPump;
use crate::bass::{self, BassLibrary};

/// Отвод стоит последним в цепочке DSP: в войс уходит то же, что и в уши, — уже с рэком,
/// эквалайзером и лимитером. `BASS_DSP_PRIORITY_FIRST` — это наибольший приоритет,
/// то есть «первым»; нам нужен противоположный конец шкалы.
const TAP_DSP_PRIORITY: i32 = -1_000_000;

/// Максимум неразобранного звука в push-потоке (44100 × 2ch × f32 ≈ 0.5 c).
/// Если устройство отстаёт, лишнее выбрасывается — иначе задержка растёт бесконечно.
const MAX_BUFFERED_BYTES: u32 = 176_400;

/// Живёт в `ExtraOutput`, указатель на неё передан в DSP как `user`.
/// Снимать DSP нужно до дропа этой структуры.
pub struct OutputTapCtx {
    pump: DataPump,
    push_handle: u32,
}

/// Копирует буфер мижера в push-поток. Сам буфер не трогает.
///
/// Данные всегда float: мижер создан с `BASS_SAMPLE_FLOAT`, и `BASS_CONFIG_FLOATDSP`
/// включён до `BASS_Init`, так что формат буфера совпадает с форматом push-потока
/// независимо от того, прошёл ли `channel_set_dsp_ex` или сработал фолбэк.
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
    // Устройство не успевает — пропускаем порцию, чтобы не копить задержку.
    if ctx.pump.queued(ctx.push_handle) > MAX_BUFFERED_BYTES {
        return;
    }
    let data = std::slice::from_raw_parts(buffer as *const u8, length as usize);
    ctx.pump.push(ctx.push_handle, data);
}

pub struct OutputTap {
    pub push_handle: u32,
    pub dsp: u32,
    /// Держится живым ради указателя, который лежит в DSP. Порядок дропа важен:
    /// сначала снять DSP, потом отпустить контекст.
    _ctx: Box<OutputTapCtx>,
}

/// Поднимает push-поток на `device` и цепляет к `mixer` копирующий DSP.
/// `volume` (0.0–1.0) применяется к push-потоку, то есть только к этому устройству.
pub fn attach(
    bass: &BassLibrary,
    mixer: u32,
    device: i32,
    volume: f32,
) -> Result<OutputTap, String> {
    if mixer == 0 {
        return Err("mixer is not running".into());
    }

    // Push-поток принадлежит устройству, активному на момент создания, поэтому
    // переключаемся, создаём и возвращаем устройство назад.
    let previous = bass.get_device();
    bass.set_device(device)?;
    let created = bass
        .stream_create_push(44100, 2, bass::BASS_SAMPLE_FLOAT)
        .and_then(|handle| {
            // Громкость до play, иначе первые миллисекунды уйдут на полной.
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

/// Снимает DSP и освобождает push-поток. DSP убирается первым: после этого колбэк уже не
/// может обратиться к освобождённому хэндлу и к контексту.
///
/// `mixer` можно передать 0, если мижер уже освобождён — DSP умер вместе с ним.
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
