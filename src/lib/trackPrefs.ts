import { invoke } from '@tauri-apps/api/core';

/** Mirror of Settings global rate — kept so track switches don't re-load settings. */
let cachedGlobalRate = 1.0;

export function clampPlaybackRate(rate: number): number {
  if (!Number.isFinite(rate)) return 1;
  return Math.max(0.25, Math.min(2, rate));
}

/** Call whenever Settings global rate changes (or on boot). */
export function setCachedGlobalPlaybackRate(rate: number) {
  cachedGlobalRate = clampPlaybackRate(rate);
}

export function getCachedGlobalPlaybackRate(): number {
  return cachedGlobalRate;
}

/** `null` = no override (use global). */
export async function getTrackPlaybackRate(
  path: string,
): Promise<number | null> {
  if (!path) return null;
  try {
    const rate = await invoke<number | null>('track_prefs_get_playback_rate', {
      path,
    });
    if (rate == null || !Number.isFinite(rate) || rate <= 0) return null;
    return clampPlaybackRate(rate);
  } catch (e) {
    console.warn('[trackPrefs] get failed', e);
    return null;
  }
}

/** Pass `null` to clear the per-track override. */
export async function setTrackPlaybackRate(
  path: string,
  rate: number | null,
): Promise<void> {
  if (!path) return;
  const payload =
    rate == null || !Number.isFinite(rate) ? null : clampPlaybackRate(rate);
  await invoke('track_prefs_set_playback_rate', { path, rate: payload });
}

/** Extensions the Rust side accepts for a video background. */
export const VIDEO_BG_EXTENSIONS = ['mp4', 'webm', 'mkv', 'mov', 'm4v'] as const;

/**
 * Per-track fullscreen video background. `null` = none (cover background is used).
 *
 * Rust re-grants asset-protocol access on every get, so always read through this
 * before building an asset URL — scope grants do not survive an app restart.
 */
export async function getTrackVideoBg(path: string): Promise<string | null> {
  if (!path) return null;
  try {
    const video = await invoke<string | null>('track_prefs_get_video_bg', {
      path,
    });
    return video?.trim() || null;
  } catch (e) {
    console.warn('[trackPrefs] get video bg failed', e);
    return null;
  }
}

/** Pass `null` to clear. Returns the stored path (validated by Rust). */
export async function setTrackVideoBg(
  path: string,
  videoPath: string | null,
): Promise<string | null> {
  if (!path) return null;
  const stored = await invoke<string | null>('track_prefs_set_video_bg', {
    path,
    videoPath: videoPath?.trim() || null,
  });
  return stored?.trim() || null;
}

/**
 * Auto-download a 15-second video clip from YouTube for track background.
 * Only runs if the setting is enabled and track has no existing video.
 */
export async function autoDownloadTrackVideoBg(
  path: string,
  title: string,
  artist: string,
): Promise<string | null> {
  if (!path || !title || !artist) {
    console.log('[trackPrefs] autoDownloadTrackVideoBg: Missing required params', { path, title, artist });
    return null;
  }

  console.log('[trackPrefs] autoDownloadTrackVideoBg: Starting for', { path, title, artist });

  try {
    const result = await invoke<string | null>(
      'track_prefs_auto_download_video_bg',
      { path, title, artist },
    );

    if (result) {
      console.log('[trackPrefs] autoDownloadTrackVideoBg: SUCCESS - Downloaded:', result);
    } else {
      console.log('[trackPrefs] autoDownloadTrackVideoBg: Skipped (setting disabled or video exists)');
    }

    return result?.trim() || null;
  } catch (e) {
    console.error('[trackPrefs] autoDownloadTrackVideoBg: FAILED -', e);
    return null;
  }
}

/** Effective rate for a path: track override or global Settings. */
export async function getEffectivePlaybackRate(
  path: string | null | undefined,
): Promise<number> {
  if (path) {
    const override = await getTrackPlaybackRate(path);
    if (override != null) return override;
  }
  return cachedGlobalRate;
}

/** Push effective rate into the live player (does not change Settings). */
export async function applyEffectivePlaybackRate(
  path: string | null | undefined,
): Promise<number> {
  const rate = await getEffectivePlaybackRate(path);
  try {
    await invoke('player_set_playback_rate', { rate });
  } catch (e) {
    console.warn('[trackPrefs] apply rate failed', e);
  }
  return rate;
}
