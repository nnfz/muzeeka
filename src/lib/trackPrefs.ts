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
