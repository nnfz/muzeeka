// Player-related Tauri commands.

use std::sync::Arc;

use serde::Deserialize;
use tauri::State;

use crate::discord_rpc::DiscordPresence;
use crate::dsp_chain::{ChainSlotSettings, DspChainStatus};
use crate::player::{
    ArmedMix, GaplessTrack, MixVolSegment, Player, PlayerStateSnapshot,
};
use crate::session::PlaybackSession;

use super::notify::{notify_playback_change, notify_playback_seek};

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NextTrackInput {
    file_path: Option<String>,
    audio_path: Option<String>,
    cue_start: Option<f64>,
    cue_end: Option<f64>,
}

fn parse_gapless_track(input: NextTrackInput) -> Option<GaplessTrack> {
    let track_path = input.file_path.filter(|value| !value.is_empty())?;
    let audio_path = input
        .audio_path
        .filter(|value| !value.is_empty())
        .or_else(|| {
            // Virtual CUE paths always encode the real audio file before `#cue:`.
            crate::cue::parse_virtual_cue_path(&track_path).map(|(audio, _)| audio)
        })?;
    Some(GaplessTrack {
        track_path,
        audio_path,
        cue_start: input.cue_start,
        cue_end: input.cue_end,
    })
}

fn parse_gapless_queue(queue: Option<Vec<NextTrackInput>>) -> Vec<GaplessTrack> {
    queue
        .unwrap_or_default()
        .into_iter()
        .filter_map(parse_gapless_track)
        .collect()
}

/// Initialize the BASS audio engine. Must be called once before playback.
#[tauri::command]
pub fn player_init(player: State<'_, Player>) -> Result<(), String> {
    player.init()
}

/// Start playing a file by its full path.
#[tauri::command]
pub fn player_play(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<PlaybackSession>>,
    file_path: String,
    audio_path: Option<String>,
    cue_start: Option<f64>,
    cue_end: Option<f64>,
    queue: Option<Vec<NextTrackInput>>,
) -> Result<(), String> {
    player.play(
        &file_path,
        audio_path.as_deref(),
        cue_start,
        cue_end,
        parse_gapless_queue(queue),
    )?;
    notify_playback_change(&player, &discord, controller.inner());
    Ok(())
}

/// Refresh the gapless queue from the current track onward.
#[tauri::command]
pub fn player_prepare_next(
    player: State<'_, Player>,
    current_file: Option<String>,
    queue: Option<Vec<NextTrackInput>>,
) -> Result<(), String> {
    player.prepare_next(current_file.as_deref(), parse_gapless_queue(queue))
}

/// Two-deck mix for the Mix transition editor (timeline-aligned + volume envelopes).
#[tauri::command]
pub fn player_mix_crossfade(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<PlaybackSession>>,
    from_path: String,
    from_audio_path: Option<String>,
    from_cue_start: Option<f64>,
    from_cue_end: Option<f64>,
    to_path: String,
    to_audio_path: Option<String>,
    to_cue_start: Option<f64>,
    to_cue_end: Option<f64>,
    to_delay_secs: f64,
    from_duration_secs: f64,
    from_vol: Option<Vec<MixVolSegment>>,
    to_vol: Option<Vec<MixVolSegment>>,
    from_lp: Option<Vec<MixVolSegment>>,
    from_hp: Option<Vec<MixVolSegment>>,
    to_lp: Option<Vec<MixVolSegment>>,
    to_hp: Option<Vec<MixVolSegment>>,
    from_speed: Option<Vec<MixVolSegment>>,
    to_speed: Option<Vec<MixVolSegment>>,
) -> Result<(), String> {
    player.play_mix_crossfade(
        &from_path,
        from_audio_path.as_deref(),
        from_cue_start,
        from_cue_end,
        &to_path,
        to_audio_path.as_deref(),
        to_cue_start,
        to_cue_end,
        to_delay_secs,
        from_duration_secs,
        from_vol.unwrap_or_default(),
        to_vol.unwrap_or_default(),
        from_lp.unwrap_or_default(),
        from_hp.unwrap_or_default(),
        to_lp.unwrap_or_default(),
        to_hp.unwrap_or_default(),
        from_speed.unwrap_or_default(),
        to_speed.unwrap_or_default(),
    )?;
    notify_playback_change(&player, &discord, controller.inner());
    Ok(())
}

/// Arm a saved transition on the track that is already playing (playlist mix mode).
/// Fires by itself once the playhead reaches the layout origin.
#[tauri::command]
pub fn player_arm_mix(player: State<'_, Player>, mix: ArmedMix) -> Result<(), String> {
    player.arm_mix(mix)
}

/// Drop the armed transition (mix mode off, edge changed, playlist switched).
#[tauri::command]
pub fn player_disarm_mix(player: State<'_, Player>) -> Result<(), String> {
    player.disarm_mix()
}

/// Pause the current playback.
#[tauri::command]
pub fn player_pause(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<PlaybackSession>>,
) -> Result<(), String> {
    player.pause()?;
    notify_playback_change(&player, &discord, controller.inner());
    Ok(())
}

/// Resume the current playback.
#[tauri::command]
pub fn player_resume(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<PlaybackSession>>,
) -> Result<(), String> {
    player.resume()?;
    notify_playback_change(&player, &discord, controller.inner());
    Ok(())
}

/// Stop the current playback and discard the stream.
#[tauri::command]
pub fn player_stop(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<PlaybackSession>>,
) -> Result<(), String> {
    player.stop()?;
    notify_playback_change(&player, &discord, controller.inner());
    Ok(())
}

/// Seek to a position in seconds.
#[tauri::command]
pub fn player_seek(
    player: State<'_, Player>,
    discord: State<'_, DiscordPresence>,
    controller: State<'_, Arc<PlaybackSession>>,
    position: f64,
) -> Result<(), String> {
    player.seek(position)?;
    notify_playback_seek(&player, &discord, controller.inner());
    Ok(())
}

/// Set playback volume (0.0 to 1.0).
#[tauri::command]
pub fn player_set_volume(player: State<'_, Player>, volume: f32) -> Result<(), String> {
    player.set_volume(volume)
}

/// Set playback rate multiplier (0.25 to 2.0).
#[tauri::command]
pub fn player_set_playback_rate(player: State<'_, Player>, rate: f32) -> Result<(), String> {
    player.set_playback_rate(rate)
}

/// Toggle pitch coupling with playback speed (off = preserve pitch via tempo FX).
#[tauri::command]
pub fn player_set_pitch_enabled(player: State<'_, Player>, enabled: bool) -> Result<(), String> {
    player.set_pitch_enabled(enabled)
}

/// Get a snapshot of the current player state.
#[tauri::command]
pub fn player_get_state(player: State<'_, Player>) -> PlayerStateSnapshot {
    player.get_state()
}

/// Load a BASS addon DLL (e.g. "bassflac.dll" or a tracker plugin like "basszxtune.dll").
/// Relative paths resolve against the bass directory.
/// Most tracker/chiptune plugins are auto-loaded if present in the folder.
#[tauri::command]
pub fn load_addon(player: State<'_, Player>, path: String) -> Result<(), String> {
    player.load_addon(&path)
}

/// Get the current effect rack — ordered slots with each effect's settings.
#[tauri::command]
pub fn player_get_dsp_chain(player: State<'_, Player>) -> Vec<ChainSlotSettings> {
    player.get_dsp_chain()
}

/// Rack diagnostics — attach state, buffers processed, and per-slot meters.
#[tauri::command]
pub fn player_get_dsp_chain_status(player: State<'_, Player>) -> DspChainStatus {
    player.get_dsp_chain_status()
}

/// Replace the effect rack. Order is the list order; slots are matched to live
/// nodes by `id`, so reordering keeps each effect's filter state.
#[tauri::command]
pub fn player_set_dsp_chain(
    player: State<'_, Player>,
    slots: Vec<ChainSlotSettings>,
) -> Result<(), String> {
    player.set_dsp_chain(slots)
}
