import { invoke } from '@tauri-apps/api/core';
import type { MusicFile } from '$lib/stores/player.svelte';
import { endExportTrackDragUi } from '$lib/stores/trackDrag.svelte';
import { exportAudioPathForTrack } from '$lib/trackPaths';
import type { TrackDropSession } from '$lib/trackDrop';

const EXPORT_DROP_SUPPRESS_MS = 8000;

let suppressedDropKeys = new Set<string>();
let suppressDropUntil = 0;
let exportDragInProgress = false;
let exportTrackSession: TrackDropSession | null = null;

function normalizePathKey(path: string): string {
  let key = path.trim().replace(/\//g, '\\').toLowerCase();
  if (key.startsWith('\\\\?\\unc\\')) {
    const rest = key.slice('\\\\?\\unc\\'.length);
    const slash = rest.indexOf('\\');
    if (slash > 0) {
      key = `\\\\${rest.slice(0, slash)}\\${rest.slice(slash + 1)}`;
    }
  } else if (key.startsWith('\\\\?\\')) {
    key = key.slice('\\\\?\\'.length);
  }
  return key;
}

function pathKeys(path: string): string[] {
  const keys = new Set<string>();
  const normalized = normalizePathKey(path);
  keys.add(normalized);
  if (normalized.includes('\\')) {
    keys.add(normalized.split('\\').pop() ?? normalized);
  }
  return [...keys];
}

function markPathsForDropSuppress(paths: string[]) {
  suppressedDropKeys = new Set<string>();
  for (const path of paths) {
    for (const key of pathKeys(path)) {
      suppressedDropKeys.add(key);
    }
  }
  exportDragInProgress = true;
  suppressDropUntil = Number.POSITIVE_INFINITY;
}

function finishDropSuppress() {
  exportDragInProgress = false;
  exportTrackSession = null;
  suppressDropUntil = Date.now() + EXPORT_DROP_SUPPRESS_MS;
  window.setTimeout(() => {
    if (Date.now() >= suppressDropUntil) {
      suppressedDropKeys.clear();
      suppressDropUntil = 0;
    }
  }, EXPORT_DROP_SUPPRESS_MS + 200);
}

function isSuppressActive(): boolean {
  if (exportDragInProgress) return true;
  if (suppressDropUntil === 0 && suppressedDropKeys.size === 0) return false;
  if (suppressDropUntil === Number.POSITIVE_INFINITY) return true;
  return Date.now() < suppressDropUntil;
}

function pathIsSuppressed(path: string): boolean {
  if (!isSuppressActive()) return false;
  return pathKeys(path).some((key) => suppressedDropKeys.has(key));
}

export function isExportDragActive(): boolean {
  return exportDragInProgress;
}

export function getExportTrackSession(): TrackDropSession | null {
  return exportTrackSession;
}

export function beginExportTrackSession(session: TrackDropSession) {
  exportTrackSession = session;
}

/** Call synchronously the moment an export drag starts (before invoke). */
export function prepareExportDropSuppress(paths: string[]) {
  const unique = [...new Set(paths.filter(Boolean))];
  if (unique.length === 0) return;
  markPathsForDropSuppress(unique);
}

export function filterIncomingDropPaths(paths: string[]): string[] | null {
  if (paths.length === 0) return null;
  if (!isSuppressActive()) return paths;

  const filtered = paths.filter((path) => !pathIsSuppressed(path));
  return filtered.length > 0 ? filtered : null;
}

export function shouldSuppressDropOverlay(paths: string[]): boolean {
  if (exportTrackSession) return true;
  if (exportDragInProgress) return true;
  if (!isSuppressActive()) return false;
  if (paths.length === 0) return true;
  return paths.every((path) => pathIsSuppressed(path));
}

export interface StartFileDragOptions {
  iconPath?: string | null;
  trackSession?: TrackDropSession;
}

export async function startFileDrag(
  paths: string[],
  options: StartFileDragOptions = {},
): Promise<void> {
  const unique = [...new Set(paths.filter(Boolean))];
  if (unique.length === 0) return;

  const { iconPath = null, trackSession } = options;
  if (trackSession) {
    beginExportTrackSession(trackSession);
  }

  prepareExportDropSuppress(unique);

  try {
    await invoke('start_file_drag', {
      paths: unique,
      icon_path: iconPath,
      track_paths: trackSession?.paths ?? null,
      source_playlist_id: trackSession?.sourcePlaylistId ?? null,
      is_copy: trackSession?.isCopy ?? false,
    });
  } finally {
    finishDropSuppress();
    endExportTrackDragUi();
  }
}

export function audioPathsForDrag(
  paths: string[],
  resolveTrack: (path: string) => MusicFile | undefined,
): string[] {
  const out: string[] = [];
  for (const path of paths) {
    const track = resolveTrack(path);
    const audio = exportAudioPathForTrack(track ?? null, path);
    if (audio) out.push(audio);
  }
  return [...new Set(out)];
}