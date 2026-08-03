import { clearCoverSrcCache, prefetchCoverPaths } from '$lib/coverCache';
import { collectPlaylistCoverPaths } from '$lib/playlistCover';
import { setupTaskbar } from '$lib/taskbar';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import {
  notifyTrackPropertiesCloseAll,
  notifyTrackPropertiesPathsRemoved,
} from '$lib/stores/trackProperties.svelte';
import {
  applyEffectivePlaybackRate,
  setCachedGlobalPlaybackRate,
} from '$lib/trackPrefs';
import { reorderItemsAtBoundary } from '$lib/trackOrder';
import { setImportProgress, resetImportProgress } from '$lib/stores/importProgress.svelte';
import {
  applyShuffleModeFromSettings,
  readShuffleMode,
  type ShuffleMode,
} from '$lib/stores/settings.svelte';

// Yield to the browser event loop so the UI stays responsive
function yieldToUI() {
  return new Promise<void>((resolve) => queueMicrotask(resolve));
}

/** Normalize track paths for equality (Windows case, slashes, `\\?\`, `#cue:N`). */
function pathKey(path: string | null | undefined): string {
  if (!path) return '';
  let p = path.trim();
  if (p.startsWith('\\\\?\\')) p = p.slice(4);
  else if (p.startsWith('//?/')) p = p.slice(4);
  p = p.replace(/\//g, '\\');
  return p.toLowerCase();
}

export function sameTrackPath(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return false;
  return pathKey(a) === pathKey(b);
}

function isCueVirtualPath(path: string | null | undefined): boolean {
  return !!path && path.includes('#cue:');
}

/** Stable segment length for CUE rows (INDEX bounds beat full-file tags). */
function cueSegmentDuration(track: MusicFile | null | undefined): number | null {
  if (!track) return null;
  const start = track.cue_start_secs;
  const end = track.cue_end_secs;
  if (typeof start === 'number' && typeof end === 'number' && end > start + 0.05) {
    return end - start;
  }
  if (typeof track.duration_secs === 'number' && track.duration_secs > 0) {
    return track.duration_secs;
  }
  return null;
}

function findTrackIndexByPath(tracks: MusicFile[], filePath: string | null | undefined): number {
  if (!filePath) return -1;
  const key = pathKey(filePath);
  return tracks.findIndex((t) => pathKey(t.path) === key);
}

function findTrackByPath(tracks: MusicFile[], filePath: string | null | undefined): MusicFile | undefined {
  if (!filePath) return undefined;
  const key = pathKey(filePath);
  return tracks.find((t) => pathKey(t.path) === key);
}

// --- Types ---

export interface PlayerState {
  is_playing: boolean;
  is_paused: boolean;
  position: number;
  duration: number;
  volume: number;
  current_file: string | null;
  current_file_name: string | null;
}

export interface MusicFile {
  path: string;
  file_name: string;
  extension: string;
  size: number;
  title?: string | null;
  artist?: string | null;
  album?: string | null;
  duration_secs?: number | null;
  year?: number | null;
  track_number?: number | null;
  genre?: string | null;
  cover_path?: string | null;
  cover_path_full?: string | null;
  audio_path?: string | null;
  cue_start_secs?: number | null;
  cue_end_secs?: number | null;
}

export interface Playlist {
  id: string;
  name: string;
  tracks: MusicFile[];
  cover_path?: string | null;
}

type RepeatMode = 'off' | 'all' | 'one';

interface PlaylistsData {
  playlists: Playlist[];
  library_tracks: MusicFile[];
  active_playlist_id: string | null;
  playing_playlist_id?: string | null;
  current_file: string | null;
  volume: number | null;
  liked_paths?: string[];
  all_paths?: string[];
  shuffle_enabled?: boolean;
  repeat_mode?: RepeatMode;
  playback_position?: number | null;
}

interface LibraryState {
  active_playlist_id: string | null;
  playing_playlist_id: string | null;
  current_file: string | null;
  volume: number | null;
  shuffle_enabled: boolean;
  repeat_mode: RepeatMode;
  playback_position: number | null;
}

interface StoreSyncPayload {
  activePlaylistId?: string | null;
  playingPlaylistId?: string | null;
  shuffleEnabled?: boolean;
  repeatMode?: RepeatMode;
  volume?: number | null;
  currentFile?: string | null;
  isPlaying?: boolean;
  isPaused?: boolean;
  position?: number;
  duration?: number;
}

// --- Virtual Playlist IDs ---

export const VIRTUAL_ALL_ID = '__all__';
export const VIRTUAL_LIKED_ID = '__liked__';

// --- Reactive State ---

let isPlaying = $state(false);
let isPaused = $state(false);
let position = $state(0);
let duration = $state(0);
let volume = $state(0.8);
/**
 * False until we know a real volume (library DB or live backend).
 * Prevents bootstrap races from blasting the frontend default 0.8 into BASS
 * while a quiet session is still playing after Ctrl+R.
 */
let volumeHydrated = false;
let currentFile = $state<string | null>(null);
let currentFileName = $state<string | null>(null);
let playlists = $state<Playlist[]>([]);
let libraryTracks = $state<MusicFile[]>([]);
let activePlaylistId = $state<string | null>(null);
let playingPlaylistId = $state<string | null>(null);
let currentTrackIndex = $state(-1);
let shuffleEnabled = $state(false);
let shuffleOrder = $state<number[]>([]);
let shufflePosition = $state(0);
/** Mirrors settings.shuffle_mode (default smart). */
let shuffleMode = $state<ShuffleMode>('smart');
/**
 * Smart shuffle: paths already heard in the current playing playlist cycle.
 * Cleared when the playlist changes or every track has been played once.
 */
let smartPlayedPaths = new Set<string>();
let smartPlayedPlaylistId: string | null = null;
let repeatMode = $state<RepeatMode>('off');
let playbackRate = $state(1.0);
let likedPaths = $state<string[]>([]);
let allPaths = $state<string[]>([]);
let isInitialized = $state(false);
let initPromise: Promise<void> | null = null;
let persistReady = $state(false);
let stateSaveTimer: ReturnType<typeof setTimeout> | null = null;
let persistenceChain: Promise<void> = Promise.resolve();
let lastGaplessChangeAt = 0;
let lastManualPlayAt = 0;
let lastPlayedFile = '';
/** Paths last sent to the backend gapless queue (same order the player will advance). */
let lastGaplessQueuePaths: string[] = [];
let lastPauseRequestAt = 0;
let applyingExternalSync = false;
/** Path of the track we transitioned FROM in the last track-changed event (gapless advance). */
let lastTrackChangedFromPath = '';
let lastTrackChangedAt = 0;
/**
 * Matches `playRequestId` once the backend has acknowledged the play command.
 * Before confirmation, stale track-changed events from the old gapless queue
 * are rejected unconditionally (the backend's 400ms suppress guarantees no
 * legitimate advance can happen before the IPC round-trip completes).
 */
let queueConfirmedForPlay = 0;
let listenersSetup = false;
/**
 * Table sort / display order for the playlist currently shown in the track list.
 * Used to seed playback order when that same playlist is playing.
 * (Plain lets: only read synchronously in adopt/setView, not inside $derived.)
 */
let viewOrderPlaylistId: string | null = null;
let viewOrderPaths: string[] | null = null;
/**
 * Active next/prev (and gapless) order for the playing playlist, when overridden by table sort.
 * Must be $state so `playingTracks` $derived re-runs when sort order changes.
 */
let playOrderPlaylistId = $state<string | null>(null);
let playOrderPaths = $state<string[] | null>(null);

const PAUSE_FADE_GUARD_MS = 350;

function recentPlayRequested(): boolean {
  return Date.now() - lastManualPlayAt < PAUSE_FADE_GUARD_MS;
}

function inPauseFadeWindow(): boolean {
  return Date.now() - lastPauseRequestAt < PAUSE_FADE_GUARD_MS;
}

/** Apply backend playback flags without stale pause events clobbering a new play. */
function applyBackendPlaybackState(payload: {
  is_playing?: boolean;
  is_paused?: boolean;
  state?: string;
}) {
  const playing = payload.is_playing === true || payload.state === 'playing';
  const paused = payload.is_paused === true || payload.state === 'paused';

  if (playing) {
    // Ignore stale "playing" only while UI is still paused during the fade-out tail.
    if (isPaused && inPauseFadeWindow() && !recentPlayRequested()) return;
    isPlaying = true;
    isPaused = false;
    lastPauseRequestAt = 0;
    return;
  }

  if (paused) {
    if (recentPlayRequested()) return;
    if (inPauseFadeWindow() && isPlaying) return;
    isPlaying = false;
    isPaused = true;
    return;
  }

  if (payload.state === 'stopped') {
    if (recentPlayRequested()) return;
    isPlaying = false;
    isPaused = false;
  }
}

// --- Derived ---

function buildTrackByPathMap(): Map<string, MusicFile> {
  return new Map(libraryTracks.map((track) => [track.path, track]));
}

function defaultAllPaths(): string[] {
  return libraryTracks.map((track) => track.path);
}

function reorderPathList(list: string[], paths: string[], insertIndex: number): string[] {
  return reorderItemsAtBoundary(list, paths, insertIndex, (path) => path);
}

let trackByPath = $derived(buildTrackByPathMap());
let playlistById = $derived(new Map(playlists.map((playlist) => [playlist.id, playlist])));
let playlistIdByTrackPath = $derived.by(() => {
  const result = new Map<string, string>();
  for (const playlist of playlists) {
    for (const track of playlist.tracks) {
      if (!result.has(track.path)) {
        result.set(track.path, playlist.id);
      }
    }
  }
  return result;
});

let allTracks = $derived.by(() => {
  const defaultOrder = defaultAllPaths();

  if (allPaths.length === 0) {
    return defaultOrder
      .map((path) => trackByPath.get(path))
      .filter((track): track is MusicFile => !!track);
  }

  const result: MusicFile[] = [];
  const seen = new Set<string>();

  for (const path of allPaths) {
    const track = trackByPath.get(path);
    if (track) {
      result.push(track);
      seen.add(path);
    }
  }

  for (const path of defaultOrder) {
    if (!seen.has(path)) {
      const track = trackByPath.get(path);
      if (track) result.push(track);
    }
  }

  return result;
});

let likedTracks = $derived.by(() => {
  const result: MusicFile[] = [];
  for (const path of likedPaths) {
    const track = trackByPath.get(path);
    if (track) result.push(track);
  }
  return result;
});

let tracks = $derived.by(() => {
  if (activePlaylistId === VIRTUAL_ALL_ID) return allTracks;
  if (activePlaylistId === VIRTUAL_LIKED_ID) return likedTracks;
  if (!activePlaylistId) return [];
  return playlistById.get(activePlaylistId)?.tracks ?? [];
});

let activePlaylist = $derived(
  activePlaylistId ? (playlistById.get(activePlaylistId) ?? null) : null
);

let activePlaylistName = $derived.by(() => {
  if (activePlaylistId === VIRTUAL_ALL_ID) return 'All tracks';
  if (activePlaylistId === VIRTUAL_LIKED_ID) return 'Liked';
  return activePlaylist?.name ?? null;
});

let playingPlaylist = $derived(
  playingPlaylistId ? (playlistById.get(playingPlaylistId) ?? null) : null
);

function samePathOrder(a: string[] | null, b: string[] | null): boolean {
  if (a === b) return true;
  if (!a || !b || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/** Reorder `tracks` to follow `orderPaths`; unknown paths are dropped, new tracks append. */
function applyPathOrder(tracks: MusicFile[], orderPaths: string[]): MusicFile[] {
  if (orderPaths.length === 0 || tracks.length === 0) return tracks;
  const byPath = new Map(tracks.map((track) => [track.path, track]));
  const ordered: MusicFile[] = [];
  const seen = new Set<string>();
  for (const path of orderPaths) {
    const track = byPath.get(path);
    if (!track || seen.has(path)) continue;
    ordered.push(track);
    seen.add(path);
  }
  if (ordered.length === tracks.length) return ordered;
  for (const track of tracks) {
    if (!seen.has(track.path)) ordered.push(track);
  }
  return ordered;
}

let playingTracksBase = $derived.by(() => {
  if (!playingPlaylistId) return [] as MusicFile[];
  if (playingPlaylistId === VIRTUAL_ALL_ID) return allTracks;
  if (playingPlaylistId === VIRTUAL_LIKED_ID) return likedTracks;
  return playlistById.get(playingPlaylistId)?.tracks ?? [];
});

let playingTracks = $derived.by(() => {
  const base = playingTracksBase;
  if (
    playOrderPaths &&
    playOrderPlaylistId &&
    playOrderPlaylistId === playingPlaylistId
  ) {
    return applyPathOrder(base, playOrderPaths);
  }
  return base;
});

// Search across ALL playlists so metadata survives playlist switches
let currentTrack = $derived.by(() => {
  if (!currentFile) return null;
  return (
    trackByPath.get(currentFile) ??
    [...trackByPath.values()].find((t) => sameTrackPath(t.path, currentFile)) ??
    null
  );
});

let progress = $derived(duration > 0 ? Math.min(1, Math.max(0, position / duration)) : 0);
let hasTrack = $derived(currentFile !== null);
// hasCurrentTrack: track is remembered but player is fully stopped (e.g. after app restart)
let hasCurrentTrack = $derived(currentFile !== null && !isPlaying && !isPaused);
let hasTracks = $derived(tracks.length > 0);
let hasPlayingTracks = $derived(playingTracks.length > 0);
let hasAnyTracks = $derived(playlists.some((p) => p.tracks.length > 0));
let hasPlaylists = $derived(playlists.length > 0);
let hasNext = $derived(
  repeatMode === 'all' && hasPlayingTracks
    ? true
    : shuffleEnabled
      ? shufflePosition < shuffleOrder.length - 1
      : currentTrackIndex < playingTracks.length - 1
);
let hasPrev = $derived(
  shuffleEnabled
    ? shufflePosition > 0 || position > 3
    : currentTrackIndex > 0
);

let formattedPosition = $derived(formatTime(position));
let formattedDuration = $derived(formatTime(duration));

// --- Helpers ---

function formatTime(seconds: number): string {
  const mins = Math.floor(seconds / 60);
  const secs = Math.floor(seconds % 60);
  return `${mins}:${secs.toString().padStart(2, '0')}`;
}

export function trackDisplayTitle(track: MusicFile): string {
  const title = track.title?.trim();
  if (title) return title;
  return track.file_name.replace(/\.[^/.]+$/, '');
}

export function trackDisplayArtist(track: MusicFile): string {
  return track.artist?.trim() || 'Unknown Artist';
}

export const APP_TITLE = 'Muzeeka';

export function formatWindowTitle(
  track: MusicFile | null,
  fallbackFileName?: string | null
): string {
  if (!track && !fallbackFileName) return APP_TITLE;

  const title = track ? trackDisplayTitle(track) : (fallbackFileName ?? APP_TITLE);
  const artist = track ? trackDisplayArtist(track) : 'Unknown Artist';

  return `${title} - ${artist} | ${APP_TITLE}`;
}

let lastWindowTitle = '';

function syncWindowTitle() {
  const title = currentFile
    ? formatWindowTitle(currentTrack, currentFileName)
    : APP_TITLE;

  if (title === lastWindowTitle) return;
  lastWindowTitle = title;

  if (typeof document !== 'undefined') {
    document.title = title;
  }

  try {
    const win = getCurrentWindow();
    if (win.label !== 'main') return;
    void win.setTitle(title).catch((e) => {
      console.error('Failed to set window title:', e);
    });
  } catch {
    // not in a Tauri webview
  }
}

export function trackSearchText(track: MusicFile): string {
  return [
    trackDisplayTitle(track),
    trackDisplayArtist(track),
    track.album,
    track.file_name,
    track.genre,
  ]
    .filter(Boolean)
    .join(' ')
    .toLowerCase();
}

// Set to true after the first enrichment pass so we stop treating all cached
// cover paths as potentially stale on subsequent calls.
let coverCacheValidated = false;

function isStaleCoverPath(path: string | null | undefined): boolean {
  // A cover stored inside the app covers cache may disappear if the cache
  // was wiped. Mark it as stale so enrichment fetches a fresh one.
  // Only do this check on the very first pass — after that we trust the cache.
  if (coverCacheValidated) return false;
  return typeof path === 'string' && /[\\/](?:covers|playlist_covers)[\\/]/i.test(path);
}

function needsMetadata(track: MusicFile): boolean {
  // CUE rows with missing INDEX bounds / duration need a repair pass too.
  if (isCueVirtualPath(track.path)) {
    const missingBounds =
      track.cue_start_secs == null ||
      track.cue_end_secs == null ||
      track.duration_secs == null ||
      track.duration_secs <= 0;
    return missingBounds || isStaleCoverPath(track.cover_path);
  }
  return track.duration_secs == null || isStaleCoverPath(track.cover_path);
}



function mergeTrackMetadata(track: MusicFile, incoming: MusicFile): MusicFile {
  // Preserve CUE segment identity: never replace a virtual path with a plain file.
  if (isCueVirtualPath(track.path) && !isCueVirtualPath(incoming.path)) {
    return {
      ...track,
      title: track.title ?? incoming.title,
      artist: track.artist ?? incoming.artist,
      album: track.album ?? incoming.album,
      cover_path: incoming.cover_path ?? track.cover_path,
      cover_path_full: incoming.cover_path_full ?? track.cover_path_full,
      duration_secs: cueSegmentDuration(track) ?? track.duration_secs,
    };
  }
  if (isCueVirtualPath(track.path) || isCueVirtualPath(incoming.path)) {
    const start = incoming.cue_start_secs ?? track.cue_start_secs;
    const end = incoming.cue_end_secs ?? track.cue_end_secs;
    let duration = incoming.duration_secs ?? track.duration_secs;
    if (typeof start === 'number' && typeof end === 'number' && end > start + 0.05) {
      duration = end - start;
    }
    return {
      ...track,
      ...incoming,
      path: track.path,
      audio_path: incoming.audio_path ?? track.audio_path,
      cue_start_secs: start,
      cue_end_secs: end,
      duration_secs: duration,
    };
  }
  return { ...track, ...incoming, path: track.path };
}

function mergeMetadataIntoPlaylists(enriched: MusicFile[]) {
  if (enriched.length === 0) return;

  const byPath = new Map(enriched.map((track) => [pathKey(track.path), track]));
  libraryTracks = libraryTracks.map((track) => {
    const incoming = byPath.get(pathKey(track.path));
    return incoming ? mergeTrackMetadata(track, incoming) : track;
  });
  const canonical = new Map(libraryTracks.map((track) => [pathKey(track.path), track]));
  playlists = playlists.map((playlist) => ({
    ...playlist,
    tracks: playlist.tracks.map((track) => canonical.get(pathKey(track.path)) ?? track),
  }));

  if (currentFile && byPath.has(pathKey(currentFile))) {
    syncWindowTitle();
  }
  const updated = enriched
    .map((track) => libraryTracks.find((item) => pathKey(item.path) === pathKey(track.path)))
    .filter((track): track is MusicFile => !!track);
  persistMutation('library_tracks_upsert', { tracks: updated });
}

let enrichRunning = false;
let enrichQueued = false;

async function enrichTrackMetadata() {
  if (enrichRunning) {
    enrichQueued = true;
    return;
  }
  enrichRunning = true;
  try {
    await _doEnrichTrackMetadata();
  } finally {
    enrichRunning = false;
    if (enrichQueued) {
      enrichQueued = false;
      void enrichTrackMetadata();
    }
  }
}

async function _doEnrichTrackMetadata() {
  const allNeedingMeta = [
    ...new Set(
      [...libraryTracks, ...playlists.flatMap((p) => p.tracks)]
        .filter(needsMetadata)
        .map((t) => t.path)
    ),
  ];

  coverCacheValidated = true;

  if (allNeedingMeta.length === 0) return;

  const CHUNK = 50;
  for (let i = 0; i < allNeedingMeta.length; i += CHUNK) {
    const paths = allNeedingMeta.slice(i, i + CHUNK);
    try {
      const enriched = await invoke<MusicFile[]>('library_fetch_metadata', { paths });
      mergeMetadataIntoPlaylists(enriched);
      // Thumbs only — never bulk-decode full covers into the image cache.
      prefetchCoverPaths(enriched.map((t) => t.cover_path), 40);
    } catch (e) {
      console.error('Failed to fetch track metadata:', e);
    }
    await yieldToUI();
  }
}

/** Reload playlists + covers after Settings → Rebuild covers. */
async function refreshCoversAfterRebuild() {
  clearCoverSrcCache();
  coverCacheValidated = false;
  try {
    await persistenceChain;
    const data = await invoke<PlaylistsData>('playlists_load');
    libraryTracks = data.library_tracks ?? [];
    playlists = (data.playlists ?? []).map(repairPlaylistTracks);
    allPaths = data.all_paths ?? libraryTracks.map((track) => track.path);
    likedPaths = data.liked_paths ?? [];
    coverCacheValidated = false;
    await enrichTrackMetadata();
    prefetchCoverPaths([
      ...playlists.flatMap((p) => p.tracks.map((t) => t.cover_path)),
      ...collectPlaylistCoverPaths(playlists),
    ]);
  } catch (e) {
    console.error('Failed to refresh covers after rebuild:', e);
  }
}

function findPlaylistForTrack(path: string): string | null {
  // Prefer the current playing playlist if the track exists in it (important for "que" clicks and gapless).
  if (playingPlaylistId && playingTracks.some((t) => sameTrackPath(t.path, path))) {
    return playingPlaylistId;
  }
  const direct = playlistIdByTrackPath.get(path);
  if (direct) return direct;
  // Case / `\\?\` mismatch between backend events and playlist JSON.
  const key = pathKey(path);
  for (const [p, id] of playlistIdByTrackPath) {
    if (pathKey(p) === key) return id;
  }
  return null;
}

function syncTrackIndex() {
  currentTrackIndex = findTrackIndexByPath(playingTracks, currentFile);
  syncShufflePosition();
}

/** Reconcile UI indices and the backend queue after the playing collection changed. */
function refreshPlayingQueueAfterMutation(playlistId: string) {
  const affectsPlayingPlaylist =
    playlistId === playingPlaylistId ||
    (playingPlaylistId === VIRTUAL_ALL_ID && playlistId !== VIRTUAL_LIKED_ID);
  if (!affectsPlayingPlaylist) return;

  syncTrackIndex();
  if (shuffleEnabled) {
    rebuildShuffleOrder(currentTrackIndex >= 0);
    syncShufflePosition();
  }
  if (currentFile && (isPlaying || isPaused)) {
    void prepareGaplessNext(currentFile);
  }
}

/** A drag reorder establishes a new natural order; old table-order snapshots are invalid. */
function commitTrackOrderMutation(playlistId: string) {
  if (viewOrderPlaylistId === playlistId) {
    viewOrderPaths = null;
  }
  if (playOrderPlaylistId === playlistId) {
    playOrderPlaylistId = null;
    playOrderPaths = null;
  }
  refreshPlayingQueueAfterMutation(playlistId);
}

/**
 * Override next/prev/gapless order for a playlist (table sort).
 * Pass `paths: null` to restore the playlist's natural order.
 * Returns true when the active play order actually changed.
 */
function setPlaybackOrder(playlistId: string | null, paths: string[] | null): boolean {
  const nextId = playlistId && paths && paths.length > 0 ? playlistId : null;
  const nextPaths = nextId ? [...paths!] : null;
  if (playOrderPlaylistId === nextId && samePathOrder(playOrderPaths, nextPaths)) {
    return false;
  }
  playOrderPlaylistId = nextId;
  playOrderPaths = nextPaths;
  syncTrackIndex();
  if (shuffleEnabled) {
    rebuildShuffleOrder(currentTrackIndex >= 0);
    syncShufflePosition();
  }
  return true;
}

/**
 * Report the track list's current display order (after table sort).
 * When that playlist is the one playing, next/prev and gapless follow it.
 */
function setViewPlayOrder(playlistId: string | null, paths: string[] | null) {
  viewOrderPlaylistId = playlistId;
  viewOrderPaths = paths && paths.length > 0 ? [...paths] : null;

  // Only rewrite the live play queue when the user is looking at the playing list.
  // Switching to another playlist must not clobber the order of what's already playing.
  if (playlistId != null && playlistId === playingPlaylistId) {
    const changed = setPlaybackOrder(playlistId, viewOrderPaths);
    if (changed && currentFile && (isPlaying || isPaused)) {
      void prepareGaplessNext(currentFile);
    }
  }
}

/** After `playingPlaylistId` is set for a play request, adopt the table order if it matches. */
function adoptViewOrderForPlayingPlaylist() {
  if (viewOrderPlaylistId && viewOrderPlaylistId === playingPlaylistId && viewOrderPaths?.length) {
    setPlaybackOrder(playingPlaylistId, viewOrderPaths);
  } else if (playOrderPlaylistId !== playingPlaylistId) {
    setPlaybackOrder(null, null);
  }
}

/** True when `toPath` is the immediate next track after `fromPath` in the active play order. */
function isNaturalQueueAdvance(fromPath: string, toPath: string): boolean {
  if (!fromPath || !toPath || sameTrackPath(fromPath, toPath) || !hasPlayingTracks) return false;

  if (shuffleEnabled) {
    ensureShuffleOrder();
    const fromIdx = findTrackIndexByPath(playingTracks, fromPath);
    if (fromIdx < 0) return false;
    const orderPos = shuffleOrder.indexOf(fromIdx);
    if (orderPos < 0) return false;
    if (orderPos < shuffleOrder.length - 1) {
      return sameTrackPath(playingTracks[shuffleOrder[orderPos + 1]]?.path, toPath);
    }
    return repeatMode === 'all' && sameTrackPath(playingTracks[shuffleOrder[0]]?.path, toPath);
  }

  const idx = findTrackIndexByPath(playingTracks, fromPath);
  if (idx < 0) return false;
  if (idx < playingTracks.length - 1) {
    return sameTrackPath(playingTracks[idx + 1]?.path, toPath);
  }
  return repeatMode === 'all' && sameTrackPath(playingTracks[0]?.path, toPath);
}

/**
 * True when `toPath` is the next entry after `fromPath` in the queue we last sent to the backend.
 * Prefer this over UI play-order when deciding whether a track-changed event is a real gapless
 * advance — the backend only knows the queue we sent (critical for sub-second tracks that
 * finish inside the manual-play guard window).
 */
function isSentQueueAdvance(fromPath: string, toPath: string): boolean {
  if (!fromPath || !toPath || sameTrackPath(fromPath, toPath) || lastGaplessQueuePaths.length < 2) {
    return false;
  }
  const fromIdx = lastGaplessQueuePaths.findIndex((p) => sameTrackPath(p, fromPath));
  const toIdx = lastGaplessQueuePaths.findIndex((p) => sameTrackPath(p, toPath));
  // Any forward step in the queue we sent counts — intermediate track-changed
  // events can be missed when several sub-second tracks fire in one poll window.
  return fromIdx >= 0 && toIdx > fromIdx;
}

/** Accept a track-changed event as a legitimate auto-advance (not a stale gapless poll). */
function isLegitimateTrackAdvance(fromPath: string, toPath: string): boolean {
  if (!fromPath || !toPath || sameTrackPath(fromPath, toPath)) return false;
  // Backend queue is authoritative for what gapless will actually play next.
  if (isSentQueueAdvance(fromPath, toPath)) return true;
  if (isNaturalQueueAdvance(fromPath, toPath)) return true;
  // Also accept advance from the UI's current file (may lag lastPlayedFile by one event).
  if (currentFile && !sameTrackPath(currentFile, fromPath) && isSentQueueAdvance(currentFile, toPath)) {
    return true;
  }
  if (currentFile && !sameTrackPath(currentFile, fromPath) && isNaturalQueueAdvance(currentFile, toPath)) {
    return true;
  }
  return false;
}

function rememberGaplessQueue(queue: { filePath?: string }[] | string[]) {
  if (queue.length === 0) {
    lastGaplessQueuePaths = [];
    return;
  }
  if (typeof queue[0] === 'string') {
    lastGaplessQueuePaths = queue as string[];
    return;
  }
  lastGaplessQueuePaths = (queue as { filePath?: string }[])
    .map((item) => item.filePath)
    .filter((path): path is string => typeof path === 'string' && path.length > 0);
}

function shuffleArray<T>(items: T[]): T[] {
  const arr = [...items];
  for (let i = arr.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [arr[i], arr[j]] = [arr[j], arr[i]];
  }
  return arr;
}

function shuffleIndices(count: number): number[] {
  return shuffleArray(Array.from({ length: count }, (_, i) => i));
}

function smartPlaylistKey(): string | null {
  return playingPlaylistId ?? activePlaylistId ?? null;
}

function ensureSmartPlaylistScope() {
  const key = smartPlaylistKey();
  if (smartPlayedPlaylistId !== key) {
    smartPlayedPlaylistId = key;
    smartPlayedPaths = new Set();
  }
}

function pruneSmartHistory() {
  if (playingTracks.length === 0) {
    smartPlayedPaths = new Set();
    return;
  }
  const live = new Set(playingTracks.map((t) => t.path));
  const next = new Set<string>();
  for (const path of smartPlayedPaths) {
    if (live.has(path)) next.add(path);
  }
  smartPlayedPaths = next;
}

/** Remember a track as already heard (smart shuffle only). */
function markSmartPlayed(path: string | null | undefined) {
  if (shuffleMode !== 'smart' || !path) return;
  ensureSmartPlaylistScope();
  smartPlayedPaths.add(path);
}

function applyShuffleMode(mode: ShuffleMode) {
  const next = mode === 'normal' ? 'normal' : 'smart';
  if (shuffleMode === next) return;
  shuffleMode = next;
  if (next === 'normal') {
    smartPlayedPaths = new Set();
    smartPlayedPlaylistId = null;
  }
  if (shuffleEnabled) {
    rebuildShuffleOrder(currentTrackIndex >= 0);
    syncShufflePosition();
    if (currentFile && (isPlaying || isPaused)) {
      void prepareGaplessNext(currentFile);
    }
  }
}

function rebuildShuffleOrder(keepCurrent = true) {
  if (playingTracks.length === 0) {
    shuffleOrder = [];
    shufflePosition = 0;
    return;
  }

  let pool: number[];

  if (shuffleMode === 'smart') {
    ensureSmartPlaylistScope();
    pruneSmartHistory();

    const unplayed: number[] = [];
    for (let i = 0; i < playingTracks.length; i++) {
      if (!smartPlayedPaths.has(playingTracks[i].path)) {
        unplayed.push(i);
      }
    }

    if (unplayed.length === 0) {
      // Full cycle complete — start a fresh random pass over the whole playlist.
      smartPlayedPaths = new Set();
      pool = Array.from({ length: playingTracks.length }, (_, i) => i);
    } else {
      pool = unplayed;
    }
  } else {
    pool = Array.from({ length: playingTracks.length }, (_, i) => i);
  }

  const indices = shuffleArray(pool);

  if (keepCurrent && currentTrackIndex >= 0) {
    const at = indices.indexOf(currentTrackIndex);
    if (at > 0) {
      indices.splice(at, 1);
      indices.unshift(currentTrackIndex);
    } else if (at < 0) {
      // Current track already counted as played (smart) — keep it at the head so
      // next/prev still work while it is finishing.
      indices.unshift(currentTrackIndex);
    }
  }

  shuffleOrder = indices;
  shufflePosition = 0;
}

function syncShufflePosition() {
  if (!shuffleEnabled || currentTrackIndex < 0) return;
  const pos = shuffleOrder.indexOf(currentTrackIndex);
  if (pos >= 0) shufflePosition = pos;
}

function ensureShuffleOrder() {
  if (!shuffleEnabled) return;

  const invalid =
    shuffleOrder.length === 0 ||
    shuffleOrder.some((index) => index < 0 || index >= playingTracks.length);

  if (shuffleMode === 'smart') {
    if (invalid) {
      rebuildShuffleOrder(currentTrackIndex >= 0);
      syncShufflePosition();
      return;
    }
    // If new unplayed tracks appeared (import) that aren't in the order, rebuild
    // so they get a turn before the cycle ends.
    ensureSmartPlaylistScope();
    pruneSmartHistory();
    const orderSet = new Set(shuffleOrder);
    let missingUnplayed = false;
    for (let i = 0; i < playingTracks.length; i++) {
      if (!smartPlayedPaths.has(playingTracks[i].path) && !orderSet.has(i)) {
        // Current track may already be played in history while still in order.
        if (i !== currentTrackIndex) {
          missingUnplayed = true;
          break;
        }
      }
    }
    if (missingUnplayed) {
      rebuildShuffleOrder(currentTrackIndex >= 0);
      syncShufflePosition();
    }
    return;
  }

  if (invalid || shuffleOrder.length !== playingTracks.length) {
    rebuildShuffleOrder(currentTrackIndex >= 0);
    syncShufflePosition();
  }
}

/** Advance shuffle cursor; reshuffle / start a new smart cycle at the end when allowed. */
function advanceShufflePosition(): boolean {
  ensureShuffleOrder();
  if (shuffleOrder.length === 0) return false;

  if (shufflePosition < shuffleOrder.length - 1) {
    shufflePosition += 1;
    return true;
  }

  if (repeatMode !== 'all') {
    return false;
  }

  if (shuffleMode === 'smart') {
    // Finished the remaining unplayed set — clear history for a new full cycle.
    smartPlayedPaths = new Set();
    smartPlayedPlaylistId = smartPlaylistKey();
  }

  const finishedPath = currentFile;
  rebuildShuffleOrder(false);
  // Avoid immediately replaying the track we just finished when possible.
  if (
    finishedPath &&
    shuffleOrder.length > 1 &&
    playingTracks[shuffleOrder[0]]?.path === finishedPath
  ) {
    const first = shuffleOrder.shift()!;
    shuffleOrder.push(first);
  }
  shufflePosition = 0;
  return shuffleOrder.length > 0;
}

const DOWNLOADS_PLAYLIST_NAME = 'Downloads';

async function persistDownloadPlaylistId(id: string) {
  try {
    const current = await invoke<{ download_playlist_id?: string | null } & Record<string, unknown>>(
      'settings_load'
    );
    if (current.download_playlist_id === id) return;
    await invoke('settings_save', {
      data: { ...current, download_playlist_id: id },
    });
  } catch (e) {
    console.error('Failed to persist download playlist id:', e);
  }
}

async function syncDownloadPlaylistFromLibrary() {
  try {
    const current = await invoke<{ download_playlist_id?: string | null } & Record<string, unknown>>(
      'settings_load'
    );
    const configured = current.download_playlist_id;
    if (configured && playlistById.has(configured)) return;

    const existing = playlists.find(
      (p) => p.name.toLowerCase() === DOWNLOADS_PLAYLIST_NAME.toLowerCase()
    );
    if (!existing) return;

    await invoke('settings_save', {
      data: { ...current, download_playlist_id: existing.id },
    });
  } catch (e) {
    console.error('Failed to sync download playlist from library:', e);
  }
}

function resolveDownloadPlaylistId(configuredId: string | null | undefined): string {
  if (configuredId && playlistById.has(configuredId)) {
    return configuredId;
  }

  const id = ensurePlaylist(DOWNLOADS_PLAYLIST_NAME, { select: false });

  if (!configuredId || configuredId !== id) {
    void persistDownloadPlaylistId(id);
  }

  return id;
}

function nextPlaylistName(): string {
  let index = playlists.length + 1;
  let name = `Playlist ${index}`;
  while (playlists.some((p) => p.name === name)) {
    index += 1;
    name = `Playlist ${index}`;
  }
  return name;
}

function buildLibraryState(): LibraryState {
  return {
    active_playlist_id: activePlaylistId,
    playing_playlist_id: playingPlaylistId,
    current_file: currentFile,
    volume,
    shuffle_enabled: shuffleEnabled,
    repeat_mode: repeatMode,
    // Persist seekbar so cold start can restore the scrub position.
    playback_position:
      Number.isFinite(position) && position > 0 ? position : null,
  };
}

function persistMutation(command: string, args: Record<string, unknown>) {
  persistenceChain = persistenceChain
    .then(() => invoke<void>(command, args))
    .catch((error) => {
      console.error(`Failed SQLite mutation ${command}:`, error);
    });
}

function scheduleSave() {
  if (!persistReady || applyingExternalSync) return;
  if (stateSaveTimer) clearTimeout(stateSaveTimer);
  stateSaveTimer = setTimeout(() => {
    stateSaveTimer = null;
    persistMutation('library_state_save', { state: buildLibraryState() });
  }, 250);
}

/** Push pending playlists/volume to disk immediately (window close / hide). */
function flushSave() {
  if (!persistReady || applyingExternalSync) return;
  if (stateSaveTimer) {
    clearTimeout(stateSaveTimer);
    stateSaveTimer = null;
  }
  persistMutation('library_state_save', { state: buildLibraryState() });
}

/**
 * Clamp and apply volume to the native player (safe before/after BASS init).
 * Until volume is hydrated from DB/backend, skip the push — otherwise the
 * JS default 0.8 overwrites a live quiet session for ~0.5s on Ctrl+R.
 */
async function applyVolumeToPlayer(
  vol: number = volume,
  options?: { force?: boolean },
) {
  const clamped = Math.max(0, Math.min(1, vol));
  volume = clamped;
  if (!volumeHydrated && !options?.force) {
    return;
  }
  try {
    await invoke('player_set_volume', { volume: clamped });
  } catch (e) {
    // Player may not be ready yet during very early startup; init() will re-apply.
    console.error('Failed to apply volume:', e);
  }
}

/** Prefer live BASS volume after webview reload (backend keeps playing). */
async function hydrateVolumeFromBackend(): Promise<boolean> {
  try {
    const state = await invoke<PlayerState>('player_get_state');
    if (typeof state.volume === 'number' && Number.isFinite(state.volume)) {
      volume = Math.max(0, Math.min(1, state.volume));
      volumeHydrated = true;
      return true;
    }
  } catch {
    // Player not ready yet
  }
  return false;
}

async function loadShuffleModeFromSettings() {
  try {
    const data = await invoke<{ shuffle_mode?: ShuffleMode }>('settings_load');
    shuffleMode = applyShuffleModeFromSettings(data.shuffle_mode);
  } catch {
    shuffleMode = readShuffleMode();
  }
}

/** Session snapshot loaded from SQLite — applied after player init (cold start). */
let pendingSessionRestore: {
  file: string;
  position: number;
} | null = null;

/**
 * After cold start, the next play() of this file should seek here.
 * play() always opens at 0; without this, "resume" / first Play after restart
 * restarts the track from the beginning.
 */
let pendingResumePosition: { file: string; position: number } | null = null;

/** Throttle how often seekbar position is flushed to disk while playing. */
let lastPositionPersistAt = 0;
const POSITION_PERSIST_MS = 2000;

async function loadPlaylists() {
  try {
    await loadShuffleModeFromSettings();
    const data = await invoke<PlaylistsData>('playlists_load');
    libraryTracks = data.library_tracks ?? [];
    playlists = (data.playlists ?? []).map(repairPlaylistTracks);
    // Open playlist (or All/Liked virtual ids) from last session.
    activePlaylistId = data.active_playlist_id ?? playlists[0]?.id ?? null;
    // Volume: SQLite only. Never pull BASS default (1.0) here — that blasted
    // full volume on cold start when the DB row was missing/null.
    if (typeof data.volume === 'number' && Number.isFinite(data.volume)) {
      volume = Math.max(0, Math.min(1, data.volume));
      volumeHydrated = true;
      await applyVolumeToPlayer(volume, { force: true });
    }
    if (Array.isArray(data.liked_paths)) {
      likedPaths = data.liked_paths.filter((p: any) => typeof p === 'string' && p);
    }
    allPaths = Array.isArray(data.all_paths)
      ? data.all_paths.filter((p: any) => typeof p === 'string' && p)
      : libraryTracks.map((track) => track.path);
    if (typeof data.shuffle_enabled === 'boolean') {
      shuffleEnabled = data.shuffle_enabled;
    }
    if (data.repeat_mode === 'off' || data.repeat_mode === 'all' || data.repeat_mode === 'one') {
      repeatMode = data.repeat_mode;
    }
    if (data.playing_playlist_id) {
      playingPlaylistId = data.playing_playlist_id;
    }
    // Restore last track into UI (+ queue cold-start resume after init).
    if (data.current_file) {
      const track =
        trackByPath.get(data.current_file) ??
        findTrackByPath(libraryTracks, data.current_file) ??
        [...trackByPath.values()].find((t) => sameTrackPath(t.path, data.current_file!));
      if (track) {
        currentFile = track.path;
        currentFileName = trackDisplayTitle(track);
        if (!data.playing_playlist_id) {
          playingPlaylistId = findPlaylistForTrack(track.path);
        }
        const savedPos =
          typeof data.playback_position === 'number' &&
          Number.isFinite(data.playback_position) &&
          data.playback_position > 0
            ? data.playback_position
            : 0;
        position = savedPos;
        const seg = cueSegmentDuration(track);
        if (seg != null && seg > 0) {
          duration = seg;
        } else if (typeof track.duration_secs === 'number' && track.duration_secs > 0) {
          duration = track.duration_secs;
        }
        pendingSessionRestore = {
          file: track.path,
          position: savedPos,
        };
        // So the first Play after restart seeks instead of 0:00.
        if (savedPos > 0.25) {
          pendingResumePosition = { file: track.path, position: savedPos };
        }
        syncWindowTitle();
      }
    }
    syncTrackIndex();
    if (shuffleEnabled) {
      rebuildShuffleOrder(currentTrackIndex >= 0);
      syncShufflePosition();
    }
    // URL-string cache only (no warm decode) — virtual list loads visible thumbs on demand.
    prefetchCoverPaths(
      [
        ...playlists.flatMap((playlist) => playlist.tracks.map((track) => track.cover_path)),
        ...collectPlaylistCoverPaths(playlists),
      ],
      48,
    );
    void enrichTrackMetadata();
  } catch (e) {
    console.error('Failed to load playlists:', e);
  } finally {
    persistReady = true;
  }
}

// --- Playlist Actions ---

function ensurePlaylist(name: string, options?: { select?: boolean }): string {
  const trimmed = name.trim();
  if (!trimmed) {
    return ensurePlaylist(nextPlaylistName(), options);
  }
  const existing = playlists.find(
    (p) => p.name.toLowerCase() === trimmed.toLowerCase()
  );
  if (existing) {
    if (options?.select) {
      activePlaylistId = existing.id;
    }
    return existing.id;
  }

  const playlist: Playlist = {
    id: crypto.randomUUID(),
    name: trimmed,
    tracks: [],
  };
  // Append via functional read of current state so a concurrent import that
  // already inserted another playlist is not wiped by a stale spread.
  playlists = [...playlists, playlist];
  // Re-check: if two creates raced on the same name (shouldn't in single-thread,
  // but dual drop handlers can interleave around awaits), keep the first.
  const sameName = playlists.filter(
    (p) => p.name.toLowerCase() === trimmed.toLowerCase(),
  );
  if (sameName.length > 1) {
    const keeper = sameName[0];
    playlists = playlists.filter(
      (p) => p.name.toLowerCase() !== trimmed.toLowerCase() || p.id === keeper.id,
    );
    if (options?.select) {
      activePlaylistId = keeper.id;
    }
    return keeper.id;
  }
  persistMutation('playlist_create', { id: playlist.id, name: playlist.name });
  if (options?.select) {
    activePlaylistId = playlist.id;
  }
  syncTrackIndex();
  scheduleSave();
  return playlist.id;
}

async function clearAll() {
  await invoke('library_clear_all');
}

function createPlaylist(name?: string): string {
  return ensurePlaylist(name?.trim() || nextPlaylistName(), { select: true });
}

function selectPlaylist(id: string) {
  if (id === VIRTUAL_ALL_ID || id === VIRTUAL_LIKED_ID) {
    activePlaylistId = id;
    scheduleSave();
    return;
  }
  const playlist = playlistById.get(id);
  if (!playlist) return;
  activePlaylistId = id;
  prefetchCoverPaths(playlist.tracks.map((track) => track.cover_path));
  scheduleSave();
}

function deletePlaylist(id: string) {
  const deleted = playlistById.get(id);
  const nextPlaylists = playlists.filter((p) => p.id !== id);
  playlists = nextPlaylists;
  persistMutation('playlist_delete', { id });

  if (deleted && deleted.tracks.length > 0) {
    const remainingKeys = new Set(
      nextPlaylists.flatMap((p) => p.tracks.map((t) => pathKey(t.path)))
    );
    const likedKeys = new Set(likedPaths.map(pathKey));
    const orphanPaths = deleted.tracks
      .map((t) => t.path)
      .filter((p) => !remainingKeys.has(pathKey(p)) && !likedKeys.has(pathKey(p)));

    if (orphanPaths.length > 0) {
      const orphanKeys = new Set(orphanPaths.map(pathKey));
      libraryTracks = libraryTracks.filter((t) => !orphanKeys.has(pathKey(t.path)));
      allPaths = allPaths.filter((p) => !orphanKeys.has(pathKey(p)));
      persistMutation('library_remove_tracks', { paths: orphanPaths });
      notifyTrackPropertiesPathsRemoved(orphanPaths);

      if (currentFile && orphanKeys.has(pathKey(currentFile))) {
        void stop();
        currentFile = null;
        currentFileName = null;
        currentTrackIndex = -1;
        playingPlaylistId = null;
        shuffleOrder = [];
        shufflePosition = 0;
        syncWindowTitle();
      }
    }
  }

  if (playingPlaylistId === id) {
    void stop();
    currentFile = null;
    currentFileName = null;
    currentTrackIndex = -1;
    playingPlaylistId = null;
    shuffleOrder = [];
    shufflePosition = 0;
    syncWindowTitle();
  }

  if (activePlaylistId === id) {
    activePlaylistId = nextPlaylists[0]?.id ?? null;
  }

  scheduleSave();
}

function removeTrack(path: string, playlistId?: string | null) {
  const targetId = playlistId ?? activePlaylistId;
  if (!targetId) return;

  const playlist = playlistById.get(targetId);
  if (!playlist?.tracks.some((track) => track.path === path)) return;

  playlists = playlists.map((p) =>
    p.id === targetId ? { ...p, tracks: p.tracks.filter((track) => track.path !== path) } : p
  );
  persistMutation('playlist_remove_tracks', { playlistId: targetId, paths: [path] });
  notifyTrackPropertiesPathsRemoved([path]);

  if (currentFile === path) {
    void stop();
    currentFile = null;
    currentFileName = null;
    currentTrackIndex = -1;
    if (targetId === playingPlaylistId) {
      playingPlaylistId = null;
    }
    shuffleOrder = [];
    shufflePosition = 0;
    syncWindowTitle();
  } else {
    refreshPlayingQueueAfterMutation(targetId);
  }

  scheduleSave();
}

async function setPlaylistCover(id: string, sourcePath: string) {
  const trimmed = sourcePath.trim();
  if (!trimmed) return;
  try {
    const coverPath = await invoke<string>('playlist_cache_cover', {
      playlistId: id,
      sourcePath: trimmed,
    });
    playlists = playlists.map((p) =>
      p.id === id ? { ...p, cover_path: coverPath } : p
    );
    persistMutation('playlist_set_cover_path', { id, coverPath });
    prefetchCoverPaths([coverPath]);
  } catch (e) {
    console.error('Failed to set playlist cover:', e);
  }
}

async function setPlaylistCoverFromUrl(id: string, url: string) {
  const trimmed = url.trim();
  if (!trimmed || !(trimmed.startsWith('http://') || trimmed.startsWith('https://'))) {
    return;
  }
  try {
    const coverPath = await invoke<string>('playlist_cache_cover_url', {
      playlistId: id,
      url: trimmed,
    });
    playlists = playlists.map((p) =>
      p.id === id ? { ...p, cover_path: coverPath } : p
    );
    persistMutation('playlist_set_cover_path', { id, coverPath });
    prefetchCoverPaths([coverPath]);
  } catch (e) {
    console.error('Failed to set playlist cover from URL:', e);
  }
}

async function clearPlaylistCover(id: string) {
  try {
    await invoke('playlist_remove_cover', { playlistId: id });
  } catch (e) {
    console.error('Failed to remove playlist cover file:', e);
  }
  playlists = playlists.map((p) =>
    p.id === id ? { ...p, cover_path: null } : p
  );
  persistMutation('playlist_set_cover_path', { id, coverPath: null });
}

function renamePlaylist(id: string, name: string) {
  const trimmed = name.trim();
  if (!trimmed) return;
  playlists = playlists.map((p) =>
    p.id === id ? { ...p, name: trimmed } : p
  );
  persistMutation('playlist_rename', { id, name: trimmed });
}

export function isEditablePlaylist(id: string | null | undefined): boolean {
  return !!id && id !== VIRTUAL_ALL_ID && id !== VIRTUAL_LIKED_ID;
}

export function supportsPlaylistReorder(id: string | null | undefined): boolean {
  return !!id;
}

function setPlaylistTrackOrder(playlistId: string, tracks: MusicFile[]) {
  if (!isEditablePlaylist(playlistId)) return;

  playlists = playlists.map((p) =>
    p.id === playlistId ? { ...p, tracks: [...tracks] } : p
  );

  commitTrackOrderMutation(playlistId);
  persistMutation('playlist_reorder', {
    playlistId,
    paths: tracks.map((track) => track.path),
  });
}

function reorderLikedPaths(paths: string[], insertIndex: number) {
  likedPaths = reorderPathList(likedPaths, paths, insertIndex);
  commitTrackOrderMutation(VIRTUAL_LIKED_ID);
  persistMutation('library_reorder_liked', { paths: likedPaths });
}

function reorderAllPaths(paths: string[], insertIndex: number) {
  const base = allPaths.length > 0 ? allPaths : defaultAllPaths();
  allPaths = reorderPathList(base, paths, insertIndex);
  commitTrackOrderMutation(VIRTUAL_ALL_ID);
  persistMutation('library_reorder', { paths: allPaths });
}

function reorderTracksInView(playlistId: string, paths: string[], insertIndex: number) {
  if (playlistId === VIRTUAL_LIKED_ID) {
    reorderLikedPaths(paths, insertIndex);
    return;
  }
  if (playlistId === VIRTUAL_ALL_ID) {
    reorderAllPaths(paths, insertIndex);
    return;
  }
}

function copyTracksToPlaylist(
  paths: string[],
  targetPlaylistId: string,
  sourcePlaylistId: string,
): number {
  if (!isEditablePlaylist(targetPlaylistId)) return 0;
  if (targetPlaylistId === sourcePlaylistId) return 0;

  const tracks = paths
    .map((path) => trackByPath.get(path))
    .filter((track): track is MusicFile => !!track);
  if (tracks.length === 0) return 0;

  return mergeTracksIntoPlaylist(targetPlaylistId, tracks);
}

function removeTracksFromPlaylist(paths: string[], playlistId: string) {
  if (!isEditablePlaylist(playlistId)) return;

  const pathSet = new Set(paths);
  const playlist = playlistById.get(playlistId);
  if (!playlist?.tracks.some((track) => pathSet.has(track.path))) return;

  playlists = playlists.map((p) =>
    p.id === playlistId
      ? { ...p, tracks: p.tracks.filter((track) => !pathSet.has(track.path)) }
      : p
  );
  persistMutation('playlist_remove_tracks', { playlistId, paths });
  notifyTrackPropertiesPathsRemoved(paths);

  if (currentFile && pathSet.has(currentFile)) {
    void stop();
    currentFile = null;
    currentFileName = null;
    currentTrackIndex = -1;
    if (playlistId === playingPlaylistId) {
      playingPlaylistId = null;
    }
    shuffleOrder = [];
    shufflePosition = 0;
    syncWindowTitle();
  } else {
    refreshPlayingQueueAfterMutation(playlistId);
  }

  scheduleSave();
}

function moveTracksToPlaylist(
  paths: string[],
  targetPlaylistId: string,
  sourcePlaylistId: string,
): number {
  if (!isEditablePlaylist(targetPlaylistId)) return 0;
  if (targetPlaylistId === sourcePlaylistId) return 0;

  const tracks = paths
    .map((path) => trackByPath.get(path))
    .filter((track): track is MusicFile => !!track);
  if (tracks.length === 0) return 0;

  mergeTracksIntoPlaylist(targetPlaylistId, tracks);

  const pathSet = new Set(paths);

  if (sourcePlaylistId === VIRTUAL_LIKED_ID) {
    likedPaths = likedPaths.filter((path) => !pathSet.has(path));
    for (const path of paths) {
      persistMutation('library_set_liked', { path, liked: false });
    }
    refreshPlayingQueueAfterMutation(VIRTUAL_LIKED_ID);
  } else if (sourcePlaylistId === VIRTUAL_ALL_ID) {
    const affected = playlists.filter(
      (playlist) =>
        playlist.id !== targetPlaylistId &&
        playlist.tracks.some((track) => pathSet.has(track.path)),
    );
    playlists = playlists.map((playlist) =>
      playlist.id === targetPlaylistId
        ? playlist
        : { ...playlist, tracks: playlist.tracks.filter((track) => !pathSet.has(track.path)) }
    );
    for (const playlist of affected) {
      persistMutation('playlist_remove_tracks', { playlistId: playlist.id, paths });
    }
    refreshPlayingQueueAfterMutation(VIRTUAL_ALL_ID);
  } else if (isEditablePlaylist(sourcePlaylistId)) {
    removeTracksFromPlaylist(paths, sourcePlaylistId);
  }

  return tracks.length;
}

function mergeTracksIntoPlaylist(playlistId: string, files: MusicFile[]): number {
  const existing = new Set(
    (playlistById.get(playlistId)?.tracks ?? []).map((track) => pathKey(track.path))
  );
  const nextLibraryTracks = [...libraryTracks];
  const libraryIndex = new Map(
    nextLibraryTracks.map((track, index) => [pathKey(track.path), index])
  );
  const canonicalFiles: MusicFile[] = [];
  const addedLibraryPaths: string[] = [];

  // Build one index and update one array. The previous implementation searched
  // and remapped the entire library for every imported file (O(n²)).
  for (const file of files) {
    const key = pathKey(file.path);
    const existingIndex = libraryIndex.get(key);
    if (existingIndex !== undefined) {
      const merged = mergeTrackMetadata(nextLibraryTracks[existingIndex], file);
      nextLibraryTracks[existingIndex] = merged;
      canonicalFiles.push(merged);
    } else {
      libraryIndex.set(key, nextLibraryTracks.length);
      nextLibraryTracks.push(file);
      canonicalFiles.push(file);
      addedLibraryPaths.push(file.path);
    }
  }

  libraryTracks = nextLibraryTracks;
  if (addedLibraryPaths.length > 0) {
    allPaths = [...allPaths, ...addedLibraryPaths];
  }

  const appended = new Set<string>();
  const newTracks = canonicalFiles.filter((track) => {
    const key = pathKey(track.path);
    if (existing.has(key) || appended.has(key)) return false;
    appended.add(key);
    return true;
  });
  persistMutation('playlist_add_tracks', { playlistId, tracks: canonicalFiles });
  if (newTracks.length === 0) return 0;

  playlists = playlists.map((p) =>
    p.id === playlistId ? { ...p, tracks: [...p.tracks, ...newTracks] } : p
  );
  refreshPlayingQueueAfterMutation(playlistId);
  prefetchCoverPaths(newTracks.map((track) => track.cover_path));
  void enrichTrackMetadata();
  return newTracks.length;
}

function addScannedTracks(files: MusicFile[], playlistId?: string | null): number {
  if (files.length === 0) return 0;

  let targetId = playlistId ?? activePlaylistId;
  if (!targetId) {
    targetId = createPlaylist();
  } else if (!playlistById.has(targetId)) {
    return 0;
  }

  activePlaylistId = targetId;
  scheduleSave();
  return mergeTracksIntoPlaylist(targetId, files);
}

/** Basename of a filesystem path (folder or file). */
function pathBasename(path: string): string {
  // Strip Windows extended-length prefix so `\\?\C:\Music\Album` → `Album`.
  let cleaned = path.trim();
  if (cleaned.startsWith('\\\\?\\')) cleaned = cleaned.slice(4);
  else if (cleaned.startsWith('//?/')) cleaned = cleaned.slice(4);
  cleaned = cleaned.replace(/[\\/]+$/, '').trim();
  if (!cleaned) return path;
  const parts = cleaned.split(/[\\/]/);
  return parts[parts.length - 1] || cleaned;
}

const MEDIA_DROP_EXTENSIONS = new Set([
  'mp3', 'flac', 'ogg', 'wav', 'aac', 'm4a', 'wma', 'opus', 'ape',
  'mod', 's3m', 'xm', 'it', 'ay', 'ym', 'vgm', 'vgz', 'nsf', 'nsfe',
  'gbs', 'hes', 'sap', 'kss', 'pt2', 'pt3', 'stc', 'stp', 'asc', 'sqt', 'psg',
  'cue',
]);

function looksLikeMediaFile(path: string): boolean {
  const base = pathBasename(path);
  const dot = base.lastIndexOf('.');
  if (dot <= 0) return false;
  return MEDIA_DROP_EXTENSIONS.has(base.slice(dot + 1).toLowerCase());
}

/**
 * Folder/file prefix for grouping drop results.
 * Must match `pathKey` (strip `\\?\`, normalize slashes) — scanned tracks are
 * canonicalized on Windows and otherwise never match the original drop folder.
 */
function normalizePathPrefix(path: string): string {
  return pathKey(path).replace(/[\\/]+$/, '');
}

export interface CreatePlaylistsFromDropResult {
  playlists: number;
  tracks: number;
  names: string[];
}

/**
 * Create one playlist per dropped folder (named after the folder).
 * Loose audio/cue files are gathered into a single new playlist.
 */
async function createPlaylistsFromDroppedPaths(
  paths: string[],
): Promise<CreatePlaylistsFromDropResult> {
  const normalizedPaths = paths.map((path) => path.trim()).filter(Boolean);
  if (normalizedPaths.length === 0) {
    return { playlists: 0, tracks: 0, names: [] };
  }

  let playlistCount = 0;
  let trackCount = 0;
  const names: string[] = [];
  const filePaths: string[] = [];
  let lastId: string | null = null;

  // Count directory paths for progress
  const dirPaths = normalizedPaths.filter((p) => !looksLikeMediaFile(p));
  const dirCount = dirPaths.length;
  const CHUNK_SIZE = 5;

  for (let i = 0; i < normalizedPaths.length; i++) {
    const path = normalizedPaths[i];
    if (looksLikeMediaFile(path)) {
      filePaths.push(path);
      continue;
    }

    try {
      const files: MusicFile[] = await invoke('library_scan', { directory: path });
      if (files.length === 0) continue;
      const name = pathBasename(path) || nextPlaylistName();
      const id = ensurePlaylist(name, { select: true });
      trackCount += mergeTracksIntoPlaylist(id, files);
      playlistCount += 1;
      names.push(name);
      lastId = id;
    } catch {
      // Not a directory — treat as a file path
      filePaths.push(path);
    }

    // Report progress and yield to UI every CHUNK_SIZE dirs
    setImportProgress({ active: true, current: i + 1, total: Math.max(dirCount, 1), label: pathBasename(path) });
    if ((i + 1) % CHUNK_SIZE === 0) {
      await yieldToUI();
    }
  }

  if (filePaths.length > 0) {
    try {
      setImportProgress({ active: true, current: 0, total: 0, label: 'Processing files...' });
      const files: MusicFile[] = await invoke('library_scan_paths', { paths: filePaths });
      if (files.length > 0) {
        const name = nextPlaylistName();
        const id = ensurePlaylist(name, { select: true });
        const added = mergeTracksIntoPlaylist(id, files);
        trackCount += added;
        playlistCount += 1;
        names.push(name);
        lastId = id;
      }
      setImportProgress({ active: true, current: 1, total: 1, label: 'Finishing up...' });
      await yieldToUI();
    } catch (e) {
      console.error('Failed to create playlist from dropped files:', e);
    }
  }

  if (lastId) {
    activePlaylistId = lastId;
  }

  resetImportProgress();
  return { playlists: playlistCount, tracks: trackCount, names };
}

/**
 * Create playlists from already-scanned tracks, grouping by original drop paths when available.
 * Each dropped folder becomes a playlist named after that folder.
 */
function createPlaylistsFromScannedTracks(
  files: MusicFile[],
  sourcePaths?: string[] | null,
): CreatePlaylistsFromDropResult {
  if (files.length === 0) {
    return { playlists: 0, tracks: 0, names: [] };
  }

  const paths = (sourcePaths ?? []).map((p) => p.trim()).filter(Boolean);
  const dirSources = paths.filter((source) => !looksLikeMediaFile(source));

  if (paths.length === 0) {
    const name = nextPlaylistName();
    const id = ensurePlaylist(name, { select: true });
    const added = mergeTracksIntoPlaylist(id, files);
    return { playlists: 1, tracks: added, names: [name] };
  }

  const claimed = new Set<string>();
  let playlistCount = 0;
  let trackCount = 0;
  const names: string[] = [];
  let lastId: string | null = null;

  for (const source of dirSources) {
    const prefix = normalizePathPrefix(source);
    if (!prefix) continue;

    const group = files.filter((file) => {
      // file.path may be CUE virtual (`…cue#cue:N`) or `\\?\…` — pathKey handles both.
      const fileKey = normalizePathPrefix(file.path.split('#')[0] ?? file.path);
      const audioKey = file.audio_path ? normalizePathPrefix(file.audio_path) : '';
      return (
        fileKey === prefix ||
        fileKey.startsWith(`${prefix}\\`) ||
        (audioKey !== '' &&
          (audioKey === prefix || audioKey.startsWith(`${prefix}\\`)))
      );
    });
    if (group.length === 0) continue;

    for (const file of group) claimed.add(pathKey(file.path));
    const name = pathBasename(source) || nextPlaylistName();
    const id = ensurePlaylist(name, { select: true });
    trackCount += mergeTracksIntoPlaylist(id, group);
    playlistCount += 1;
    names.push(name);
    lastId = id;
  }

  const rest = files.filter((file) => !claimed.has(pathKey(file.path)));
  if (rest.length > 0) {
    // One dropped folder → exactly one playlist. Leftovers from imperfect path
    // matching (\\?\ vs normal, CUE virtual paths) must NOT spawn "Playlist N".
    // That second row was written to SQLite while a race could hide it in the UI
    // until Ctrl+R.
    if (dirSources.length === 1) {
      const name = pathBasename(dirSources[0]) || nextPlaylistName();
      const id = lastId ?? ensurePlaylist(name, { select: true });
      if (!lastId) {
        playlistCount += 1;
        names.push(name);
        lastId = id;
      }
      trackCount += mergeTracksIntoPlaylist(id, rest);
    } else if (playlistCount === 0) {
      const name = nextPlaylistName();
      const id = ensurePlaylist(name, { select: true });
      trackCount += mergeTracksIntoPlaylist(id, rest);
      playlistCount += 1;
      names.push(name);
      lastId = id;
    } else {
      // Multi-folder drop: attach leftovers to the last successful group rather
      // than inventing a second silent playlist.
      if (lastId) {
        trackCount += mergeTracksIntoPlaylist(lastId, rest);
      }
    }
  }

  if (lastId) {
    activePlaylistId = lastId;
  }

  return { playlists: playlistCount, tracks: trackCount, names };
}

async function addFolderToActivePlaylist(directory: string) {
  if (!activePlaylistId) return;

  try {
    setImportProgress({ active: true, current: 0, total: 0, label: 'Scanning folder...' });
    await yieldToUI();
    const files: MusicFile[] = await invoke('library_scan', { directory });
    mergeTracksIntoPlaylist(activePlaylistId, files);
    resetImportProgress();
  } catch (e) {
    console.error('Failed to add folder to playlist:', e);
    resetImportProgress();
  }
}

async function addDroppedPaths(paths: string[], playlistId?: string | null) {
  const normalizedPaths = paths.map((path) => path.trim()).filter(Boolean);
  if (normalizedPaths.length === 0) return 0;

  try {
    setImportProgress({ active: true, current: 0, total: 0, label: 'Scanning files...' });
    await yieldToUI();
    const files: MusicFile[] = await invoke('library_scan_paths', { paths: normalizedPaths });
    setImportProgress({ active: true, current: 1, total: 1, label: 'Adding to playlist...' });
    await yieldToUI();
    const added = addScannedTracks(files, playlistId);

    resetImportProgress();
    return added;
  } catch (e) {
    console.error('Failed to add dropped paths:', e);
    resetImportProgress();
    return 0;
  }
}

function m3uPlaylistName(path: string): string {
  const base = pathBasename(path);
  return base.replace(/\.m3u8?$/i, '').trim() || nextPlaylistName();
}

/**
 * Import each .m3u/.m3u8 as its own playlist (named after the file stem).
 * Track order follows the playlist file.
 */
async function importM3uPlaylists(
  paths: string[],
): Promise<CreatePlaylistsFromDropResult> {
  const normalizedPaths = paths.map((path) => path.trim()).filter(Boolean);
  if (normalizedPaths.length === 0) {
    return { playlists: 0, tracks: 0, names: [] };
  }

  let playlistCount = 0;
  let trackCount = 0;
  const names: string[] = [];
  let lastId: string | null = null;

  try {
    setImportProgress({
      active: true,
      current: 0,
      total: normalizedPaths.length,
      label: 'Importing M3U...',
    });
    await yieldToUI();

    for (let i = 0; i < normalizedPaths.length; i++) {
      const path = normalizedPaths[i];
      setImportProgress({
        active: true,
        current: i + 1,
        total: normalizedPaths.length,
        label: pathBasename(path),
      });
      await yieldToUI();

      const files: MusicFile[] = await invoke('library_scan_paths', { paths: [path] });
      if (files.length === 0) continue;

      const name = m3uPlaylistName(path);
      const id = ensurePlaylist(name, { select: true });
      trackCount += mergeTracksIntoPlaylist(id, files);
      playlistCount += 1;
      names.push(name);
      lastId = id;
    }

    if (lastId) {
      activePlaylistId = lastId;
    }
  } catch (e) {
    console.error('Failed to import M3U playlist:', e);
  }

  resetImportProgress();
  return { playlists: playlistCount, tracks: trackCount, names };
}

// --- Player Actions ---

async function init() {
  // Already initialized: only re-push volume once we know the real value.
  // Settings bootstrap often calls ensureInit() before loadPlaylists finishes —
  // never stamp the JS default 0.8 over a live session.
  if (isInitialized) {
    if (volumeHydrated) {
      await applyVolumeToPlayer(volume, { force: true });
    }
    return;
  }
  if (initPromise) {
    await initPromise;
    if (volumeHydrated) {
      await applyVolumeToPlayer(volume, { force: true });
    }
    return;
  }

  initPromise = (async () => {
    await invoke('player_init');
    // Only push volume if we already know it from SQLite (loadPlaylists first).
    // Never adopt BASS default 1.0 here — that caused full-blast on cold start.
    if (volumeHydrated) {
      await applyVolumeToPlayer(volume, { force: true });
    }

    // Restore persisted playback rate from settings (so rate survives app restart)
    try {
      const s = await invoke<{ playback_rate?: number }>('settings_load');
      if (typeof s?.playback_rate === 'number' && s.playback_rate > 0) {
        const r = Math.max(0.25, Math.min(2, s.playback_rate));
        playbackRate = r;
        setCachedGlobalPlaybackRate(r);
        if (r !== 1) {
          await invoke('player_set_playback_rate', { rate: r }).catch(() => {});
        }
      }
    } catch {}

    isInitialized = true;
  })();

  try {
    await initPromise;
  } catch (e) {
    initPromise = null;
    console.error('Failed to initialize player:', e);
    throw e;
  }
}

function ensureInit() {
  return init();
}

/**
 * Cold start restore.
 *
 * IMPORTANT:
 * - Never "play then pause" — that blasts a half-second of audio and feels broken.
 * - If the session was not actively playing, only restore UI (track + seekbar).
 * - Seek is handled inside play() via pendingResumePosition (play always opens at 0).
 * - Volume must already be applied from SQLite before any resume.
 */
async function restorePlaybackSession(session: {
  file: string;
  position: number;
}) {
  // UI: remembered track + seekbar. Do NOT set isPaused=true without a BASS stream —
  // that makes the first Play call resume() (fails) → play() from 0:00 → second click
  // pauses → third click finally works. Use stopped+selected instead.
  isPlaying = false;
  isPaused = false;
  position = Math.max(0, session.position);
  if (session.position > 0.25) {
    pendingResumePosition = {
      file: session.file,
      position: session.position,
    };
  }
  syncTrackIndex();
  syncWindowTitle();
}

/** Seek after stream open; retry once if BASS wasn't ready yet. */
async function applyResumeSeek(filePath: string, pos: number) {
  if (pos <= 0.25) return;
  const trySeek = async () => {
    position = pos;
    seekGuardPosition = pos;
    // Keep guard while we snap — block stale 0.0 position events from play().
    seekGuardUntil = Date.now() + (isCueVirtualPath(filePath) ? 900 : 500);
    await invoke('player_seek', { position: pos });
    position = pos;
    scheduleSave();
  };
  try {
    await trySeek();
  } catch {
    await new Promise((r) => setTimeout(r, 120));
    try {
      await trySeek();
    } catch (e) {
      console.error('Failed to apply resume seek:', e);
      seekGuardUntil = 0;
    }
  }
}

async function bootstrap() {
  await loadPlaylists();
  await syncDownloadPlaylistFromLibrary();
  await init();
  // Sync frontend with backend after reload (Ctrl+R) or cold-start restore.
  try {
    const state = await invoke<PlayerState>('player_get_state');
    if (state.is_playing || state.is_paused) {
      // Backend still has the session — only refresh UI (do not re-play).
      isPlaying = state.is_playing;
      isPaused = state.is_paused;
      position = state.position;
      duration = state.duration;
      // Prefer SQLite volume; only fall back to live BASS if DB had none.
      if (
        !volumeHydrated &&
        typeof state.volume === 'number' &&
        Number.isFinite(state.volume)
      ) {
        volume = Math.max(0, Math.min(1, state.volume));
        volumeHydrated = true;
      }
      if (volumeHydrated) {
        await applyVolumeToPlayer(volume, { force: true });
      }
      if (state.current_file) {
        const track =
          trackByPath.get(state.current_file) ??
          findTrackByPath(libraryTracks, state.current_file);
        currentFile = track?.path ?? state.current_file;
        currentFileName = track
          ? trackDisplayTitle(track)
          : state.current_file_name ?? currentFileName;
        syncTrackIndex();
        syncWindowTitle();
      }
      pendingSessionRestore = null;
    } else if (pendingSessionRestore) {
      const session = pendingSessionRestore;
      pendingSessionRestore = null;
      await restorePlaybackSession(session);
    }
  } catch (e) {
    console.error('Failed to sync player state after init:', e);
    if (pendingSessionRestore) {
      const session = pendingSessionRestore;
      pendingSessionRestore = null;
      await restorePlaybackSession(session);
    }
  }
}

function repairCueTrack(track: MusicFile): MusicFile {
  if (!track.path.includes('#cue:')) return track;

  const marker = '#cue:';
  const markerPos = track.path.lastIndexOf(marker);
  if (markerPos <= 0) return track;

  const audioPath = track.path.slice(0, markerPos);
  const start = track.cue_start_secs;
  const end = track.cue_end_secs;
  let duration = track.duration_secs;
  // Prefer INDEX length over full-container tags (multi-file ALAC/m4a is often a few
  // seconds longer than the CUE cut — that made the seekbar run past 100% / jump).
  if (typeof start === 'number' && typeof end === 'number' && end > start + 0.05) {
    const seg = end - start;
    if (duration == null || duration <= 0 || duration > seg + 1.0) {
      duration = seg;
    }
  }
  return {
    ...track,
    audio_path: track.audio_path ?? audioPath,
    duration_secs: duration,
  };
}

function repairPlaylistTracks(playlist: Playlist): Playlist {
  const repaired: MusicFile[] = [];

  for (const track of playlist.tracks) {
    if (track.path.toLowerCase().endsWith('.cue')) {
      continue;
    }
    repaired.push(repairCueTrack(track));
  }

  return { ...playlist, tracks: repaired };
}

function audioPathForTrack(track: MusicFile, filePath: string): string {
  if (track.audio_path) return track.audio_path;
  const cueMarker = '#cue:';
  const markerPos = filePath.lastIndexOf(cueMarker);
  if (markerPos > 0) return filePath.slice(0, markerPos);
  return filePath;
}

function gaplessArgsForTrack(track: MusicFile, filePath: string) {
  const repaired = repairCueTrack(track);
  let cueStart = repaired.cue_start_secs ?? undefined;
  let cueEnd = repaired.cue_end_secs ?? undefined;
  // Multi-file CUE rows always start at 0; ensure the backend gets a real end bound
  // even if the scanner only supplied duration_secs for the virtual row.
  if (cueStart == null && isCueVirtualPath(filePath)) {
    cueStart = 0;
  }
  if (cueEnd == null && typeof cueStart === 'number' && typeof repaired.duration_secs === 'number') {
    cueEnd = cueStart + repaired.duration_secs;
  }
  return {
    filePath,
    audioPath: audioPathForTrack(repaired, filePath),
    cueStart,
    cueEnd,
  };
}

function playOptionsForTrack(track: MusicFile | undefined, filePath: string) {
  const resolved =
    track ?? ({ path: filePath, file_name: '', extension: '', size: 0 } as MusicFile);
  return gaplessArgsForTrack(resolved, filePath);
}

// Limit sent to backend to keep manual switches fast even on huge playlists.
// We only need the next couple for gapless anyway (refresh happens on advance).
const MAX_GAPLESS_FOLLOWING = 4;

function orderedTracksFrom(filePath: string): MusicFile[] {
  if (!hasPlayingTracks) return [];

  if (shuffleEnabled) {
    ensureShuffleOrder();
    const trackIdx = findTrackIndexByPath(playingTracks, filePath);
    if (trackIdx < 0) return [];
    const orderPos = shuffleOrder.indexOf(trackIdx);
    if (orderPos < 0) return [];
    return shuffleOrder.slice(orderPos, orderPos + MAX_GAPLESS_FOLLOWING).map((index) => playingTracks[index]);
  }

  const index = findTrackIndexByPath(playingTracks, filePath);
  if (index < 0) return [];
  return playingTracks.slice(index, index + MAX_GAPLESS_FOLLOWING);
}

/** Upcoming play-order tracks after `filePath` (shuffle-aware), for lyrics/cover prefetch. */
function getUpcomingTracks(filePath: string | null, limit = 2): MusicFile[] {
  if (!filePath || limit <= 0) return [];
  const ordered = orderedTracksFrom(filePath);
  if (ordered.length <= 1) {
    // End of list with repeat-all: warm the start of the next cycle.
    if (repeatMode === 'all' && hasPlayingTracks) {
      if (shuffleEnabled) {
        ensureShuffleOrder();
        return shuffleOrder.slice(0, limit).map((i) => playingTracks[i]).filter(Boolean);
      }
      return playingTracks.slice(0, limit);
    }
    return [];
  }
  return ordered.slice(1, 1 + limit);
}

function buildGaplessQueue(filePath: string) {
  if (repeatMode === 'one') {
    const track =
      findTrackByPath(playingTracks, filePath) ??
      trackByPath.get(filePath) ??
      [...trackByPath.values()].find((t) => sameTrackPath(t.path, filePath));
    if (track) {
      return [gaplessArgsForTrack(track, track.path || filePath)];
    }
    return [gaplessArgsForTrack({ path: filePath } as MusicFile, filePath)];
  }
  const ordered = orderedTracksFrom(filePath).map((track) =>
    gaplessArgsForTrack(track, track.path)
  );
  if (ordered.length > 0) return ordered;
  // Fallback when the track is not yet in playingTracks (playlist switch race).
  const track =
    trackByPath.get(filePath) ??
    [...trackByPath.values()].find((t) => sameTrackPath(t.path, filePath));
  return [gaplessArgsForTrack(track ?? ({ path: filePath } as MusicFile), filePath)];
}

async function prepareGaplessNext(filePath: string) {
  const requestId = ++gaplessPrepareRequestId;
  const queue = buildGaplessQueue(filePath);
  try {
    await invoke('player_prepare_next', { currentFile: filePath, queue });
    if (
      requestId === gaplessPrepareRequestId &&
      currentFile &&
      sameTrackPath(currentFile, filePath)
    ) {
      rememberGaplessQueue(queue);
    }
  } catch (e) {
    console.error('Failed to prepare gapless queue:', e);
  }
}

let playRequestId = 0;
let gaplessPrepareRequestId = 0;

type PlayUiSnapshot = {
  currentFile: string | null;
  currentFileName: string | null;
  isPlaying: boolean;
  isPaused: boolean;
  position: number;
  duration: number;
  playingPlaylistId: string | null;
  lastPlayedFile: string;
};

function snapshotPlayUi(): PlayUiSnapshot {
  return {
    currentFile,
    currentFileName,
    isPlaying,
    isPaused,
    position,
    duration,
    playingPlaylistId,
    lastPlayedFile,
  };
}

function restorePlayUi(snap: PlayUiSnapshot) {
  currentFile = snap.currentFile;
  currentFileName = snap.currentFileName;
  isPlaying = snap.isPlaying;
  isPaused = snap.isPaused;
  position = snap.position;
  duration = snap.duration;
  playingPlaylistId = snap.playingPlaylistId;
  lastPlayedFile = snap.lastPlayedFile;
  seekGuardUntil = 0;
  syncTrackIndex();
  scheduleSave();
  syncWindowTitle();
}

async function play(filePath: string) {
  const requestId = ++playRequestId;
  // Invalidate queue refreshes started for the previous track. The backend also
  // checks currentFile atomically, so a late IPC completion cannot replace this queue.
  gaplessPrepareRequestId += 1;
  // Capture previous UI so a failed play can roll back (audio never switched).
  const previousUi = snapshotPlayUi();
  try {
    await ensureInit();
    if (requestId !== playRequestId) return;

    // Resume-position is only for the remembered track — drop it on another title.
    if (
      pendingResumePosition &&
      !sameTrackPath(pendingResumePosition.file, filePath)
    ) {
      pendingResumePosition = null;
    }

    lastManualPlayAt = Date.now();
    lastPlayedFile = filePath;
    lastPauseRequestAt = 0;
    isPlaying = true;
    isPaused = false;
    // Smart shuffle: count this track as heard once playback is requested.
    markSmartPlayed(filePath);

    // DON'T call player_stop before player_play!
    // The backend's play_inner handles transitions properly:
    //   - CUE tracks in the same audio file → seek within the open stream (instant)
    //   - Preloaded next track → activate preloaded source (fast)
    //   - Different file → teardown + open new stream
    // Calling stop() first destroys the current_source and current_audio_path,
    // which prevents the CUE reuse optimization and causes glitches/delays.

    const track = trackByPath.get(filePath);
    // Warm covers + shrink legacy multi‑MB fulls so fullscreen opens instantly.
    if (track) {
      // Current track only: allow full + warm for transport / fullscreen.
      prefetchCoverPaths([track.cover_path, track.cover_path_full], 2, {
        includeFull: true,
        warm: true,
      });
    }
    void invoke<string | null>('library_resolve_full_cover', { path: filePath })
      .then((fullPath) => {
        if (!fullPath) return;
        prefetchCoverPaths([fullPath], 1, { includeFull: true, warm: true });
        // Keep playlist metadata in sync if we just created/shrank a full cover.
        const t = trackByPath.get(filePath);
        if (t && t.cover_path_full !== fullPath) {
          mergeMetadataIntoPlaylists([
            { ...t, cover_path_full: fullPath },
          ]);
        }
      })
      .catch(() => {});
    // Prefer the playlist the user is currently viewing when the track is in that list
    // (real playlists, All, Liked). That keeps next/prev aligned with table sort.
    // Fall back to the track's home playlist only when playing from search / elsewhere.
    let playlistId: string | null = null;
    if (activePlaylistId && tracks.some((t) => sameTrackPath(t.path, filePath))) {
      playlistId = activePlaylistId;
    } else {
      playlistId = findPlaylistForTrack(filePath);
    }
    if (playlistId) {
      playingPlaylistId = playlistId;
    }

    // Prefer the table's sorted order when playing from the list the user is viewing.
    adoptViewOrderForPlayingPlaylist();

    // Build gapless queue from the active play order (incl. shuffle / table sort).
    // playingPlaylistId is already set above so derived playingTracks matches next/prev.
    // Clear the remembered queue during the transition — we'll confirm it once the
    // backend acknowledges the play command. This prevents stale track-changed events
    // from the old queue being accepted via isSentQueueAdvance against the new queue
    // (a path present in both old and new queue would incorrectly pass the check).
    const queueToSend = buildGaplessQueue(filePath);
    lastGaplessQueuePaths = [];

    // Block stale position events BEFORE the invoke so they can't flash
    // the seekbar. This is critical for CUE tracks: when switching backward
    // in the same audio file, the backend buffer still holds the old absolute
    // position (e.g. 130s), but cue_start gets updated to 0 → relative
    // position = 130 → seekbar jumps to max for a frame before resetting.
    // If we will seek after open, keep UI on the target time (don't flash 0:00).
    const resumeAt =
      pendingResumePosition &&
      sameTrackPath(pendingResumePosition.file, filePath) &&
      pendingResumePosition.position > 0.25
        ? pendingResumePosition.position
        : 0;

    seekGuardPosition = resumeAt;
    // CUE same-image seeks need a longer guard — BASS can report the previous
    // absolute offset for several poll ticks after apply_segment.
    seekGuardUntil = Date.now() + (isCueVirtualPath(filePath) ? 900 : 600);
    position = resumeAt;

    // Update UI immediately — don't wait for IPC (file open + Discord sync can take 100ms+).
    const resolvedTrack =
      track ??
      findTrackByPath(playingTracks, filePath) ??
      [...trackByPath.values()].find((t) => sameTrackPath(t.path, filePath));
    currentFile = resolvedTrack?.path ?? filePath;
    currentFileName = resolvedTrack
      ? trackDisplayTitle(resolvedTrack)
      : filePath.split(/[\\/]/).pop()?.replace(/\.[^/.]+$/, '') ?? null;
    const segDur = cueSegmentDuration(resolvedTrack);
    if (segDur != null) {
      duration = segDur;
    } else if (resolvedTrack?.duration_secs != null) {
      duration = resolvedTrack.duration_secs;
    }
    syncTrackIndex();
    lastGaplessChangeAt = Date.now();
    scheduleSave();
    syncWindowTitle();

    if (requestId !== playRequestId) return;

    try {
      // Set rate before open so the new stream inherits the right tempo/FREQ.
      await applyEffectivePlaybackRate(currentFile ?? filePath);
      if (requestId !== playRequestId) return;

      await invoke('player_play', {
        ...playOptionsForTrack(resolvedTrack, currentFile ?? filePath),
        queue: queueToSend,
      });
      // Confirm queue only after backend accepted the play command.
      // Before this callback, stale track-changed from the old gapless queue
      // are rejected by the queueConfirmedForPlay guard.
      if (requestId === playRequestId) {
        rememberGaplessQueue(queueToSend);
        queueConfirmedForPlay = requestId;

        // Seek immediately after open (saved position after restart / first Play).
        if (resumeAt > 0.25) {
          pendingResumePosition = null;
          await new Promise((r) => setTimeout(r, 30));
          if (requestId === playRequestId) {
            await applyResumeSeek(currentFile ?? filePath, resumeAt);
          }
        }
      }
    } catch (e) {
      if (requestId !== playRequestId) return;
      const message = typeof e === 'string' ? e : String(e);
      console.error('Failed to play:', message);
      // Backend did not switch audio — restore previous track in the UI.
      restorePlayUi(previousUi);
    }
  } catch (e) {
    if (requestId !== playRequestId) return;
    const message = typeof e === 'string' ? e : String(e);
    console.error('Failed to play:', message);
    restorePlayUi(previousUi);
  }
}

function pause() {
  lastPauseRequestAt = Date.now();
  isPaused = true;
  isPlaying = false;
  scheduleSave();
  void invoke('player_pause').catch((e) => {
    console.error('Failed to pause:', e);
    isPaused = false;
    isPlaying = true;
  });
}

/**
 * Resume if BASS still has a paused stream; otherwise open via play()
 * (and honor pendingResumePosition for post-restart seek).
 */
async function resume() {
  lastPauseRequestAt = 0;
  try {
    await ensureInit();
    const state = await invoke<PlayerState>('player_get_state');
    // Only call BASS resume when something is actually paused in the engine.
    if (state.is_paused) {
      isPaused = false;
      isPlaying = true;
      scheduleSave();
      await invoke('player_resume');
      return;
    }
    if (state.is_playing) {
      isPaused = false;
      isPlaying = true;
      return;
    }
    // Cold start / stopped: no stream — must open via play() (+ optional seek).
    if (currentFile) {
      await play(currentFile);
      return;
    }
    isPlaying = false;
    isPaused = false;
  } catch (e) {
    if (currentFile) {
      await play(currentFile);
      return;
    }
    isPlaying = false;
    isPaused = false;
    console.error('Failed to resume:', e);
  }
}

async function stop() {
  try {
    await invoke('player_stop');
    isPlaying = false;
    isPaused = false;
    lastPauseRequestAt = 0;
    position = 0;
  } catch (e) {
    console.error('Failed to stop:', e);
  }
}

let seekGuardUntil = 0;
let seekGuardPosition = 0;

async function seek(pos: number) {
  // Prefer CUE segment length so scrubbing never targets past INDEX end.
  const track = currentTrack;
  const seg = cueSegmentDuration(track);
  const maxDur = seg != null && seg > 0 ? seg : duration;
  const clamped = Math.max(0, maxDur > 0 ? Math.min(pos, maxDur) : pos);
  position = clamped;
  seekGuardPosition = clamped;
  seekGuardUntil = Date.now() + (isCueVirtualPath(currentFile) ? 700 : 400);

  try {
    await invoke('player_seek', { position: clamped });
    scheduleSave();
  } catch (e) {
    seekGuardUntil = 0;
    console.error('Failed to seek:', e);
  }
}

function setVolume(vol: number) {
  volume = Math.max(0, Math.min(1, vol));
  volumeHydrated = true;
  void applyVolumeToPlayer(volume, { force: true });
  scheduleSave();
}

function setPlaybackRate(rate: number) {
  playbackRate = Math.max(0.25, Math.min(2, rate));
  void invoke('player_set_playback_rate', { rate: playbackRate }).catch((e) => {
    console.error('Failed to set playback rate:', e);
  });
  // Persistence is handled by the settings store. Avoid loading and rewriting the
  // whole settings file here, which can race with equalizer/download settings saves.
}

function toggleLike(path: string) {
  const index = likedPaths.indexOf(path);
  const liked = index === -1;
  if (index !== -1) {
    likedPaths = likedPaths.filter((p) => p !== path);
  } else {
    likedPaths = [...likedPaths, path];
  }
  persistMutation('library_set_liked', { path, liked });
  refreshPlayingQueueAfterMutation(VIRTUAL_LIKED_ID);
}

function isLiked(path: string): boolean {
  return likedPaths.includes(path);
}

function togglePlayPause() {
  if (isPlaying) {
    pause();
    return;
  }
  // Prefer play() when we still owe a post-restart seek — resume() has no stream.
  if (
    currentFile &&
    pendingResumePosition &&
    sameTrackPath(pendingResumePosition.file, currentFile)
  ) {
    void play(currentFile);
    return;
  }
  if (isPaused) {
    void resume();
    return;
  }
  if (currentFile) {
    void play(currentFile);
    return;
  }
  if (hasPlayingTracks && currentTrackIndex >= 0) {
    void play(playingTracks[currentTrackIndex].path);
    return;
  }
  if (hasPlayingTracks) {
    void play(playingTracks[0].path);
  }
}

async function nextTrack() {
  if (shuffleEnabled) {
    if (!advanceShufflePosition()) return;
    const idx = shuffleOrder[shufflePosition];
    if (idx >= 0 && idx < playingTracks.length) {
      await play(playingTracks[idx].path);
    }
    return;
  }

  // Use a fresh index lookup to avoid stale currentTrackIndex after rapid switches.
  const idx = findTrackIndexByPath(playingTracks, currentFile);
  if (idx >= 0 && idx < playingTracks.length - 1) {
    const targetPath = playingTracks[idx + 1].path;
    await play(targetPath);
  } else if (repeatMode === 'all' && playingTracks.length > 0) {
    await play(playingTracks[0].path);
  }
}

async function prevTrack() {
  if (position > 3 && currentFile) {
    await seek(0);
    return;
  }

  if (shuffleEnabled) {
    ensureShuffleOrder();
    if (shufflePosition > 0) {
      shufflePosition -= 1;
      const idx = shuffleOrder[shufflePosition];
      if (idx >= 0 && idx < playingTracks.length) {
        await play(playingTracks[idx].path);
      }
    }
    return;
  }

  // Capture the target path before calling play() which may mutate state.
  // Use a fresh index lookup to avoid stale currentTrackIndex.
  const idx = findTrackIndexByPath(playingTracks, currentFile);
  if (idx > 0) {
    const targetPath = playingTracks[idx - 1].path;
    await play(targetPath);
  }
}

function toggleShuffle() {
  shuffleEnabled = !shuffleEnabled;
  if (shuffleEnabled) {
    // Keep smart history when re-enabling so the cycle continues.
    shuffleMode = readShuffleMode();
    rebuildShuffleOrder(currentTrackIndex >= 0);
    syncShufflePosition();
  } else {
    shuffleOrder = [];
    shufflePosition = 0;
  }
  scheduleSave();
}

function toggleRepeat() {
  repeatMode = repeatMode === 'off' ? 'all' : repeatMode === 'all' ? 'one' : 'off';
  // Rebuild gapless queue so backend respects the new repeat mode (esp. 'one' to avoid unwanted advance)
  if (currentFile && (isPlaying || isPaused)) {
    void prepareGaplessNext(currentFile);
  }
  scheduleSave();
}

function applyStoreSync(payload: StoreSyncPayload) {
  applyingExternalSync = true;
  try {
    // Remote sync updates the playback queue only — not the playlist shown in the UI.
    if (payload.playingPlaylistId !== undefined) {
      playingPlaylistId = payload.playingPlaylistId;
    }
    if (typeof payload.shuffleEnabled === 'boolean') {
      shuffleEnabled = payload.shuffleEnabled;
      if (shuffleEnabled) {
        rebuildShuffleOrder(currentTrackIndex >= 0);
        syncShufflePosition();
      } else {
        shuffleOrder = [];
        shufflePosition = 0;
      }
    }
    if (payload.repeatMode === 'off' || payload.repeatMode === 'all' || payload.repeatMode === 'one') {
      repeatMode = payload.repeatMode;
    }
    if (typeof payload.volume === 'number') {
      volume = payload.volume;
    }
    if (payload.currentFile !== undefined) {
      currentFile = payload.currentFile;
      if (currentFile) {
        const track = trackByPath.get(currentFile);
        currentFileName = track
          ? trackDisplayTitle(track)
          : currentFile.split(/[\\/]/).pop()?.replace(/\.[^/.]+$/, '') ?? null;
        if (track?.duration_secs != null) {
          duration = track.duration_secs;
        }
      } else {
        currentFileName = null;
      }
      syncTrackIndex();
      syncWindowTitle();
    }
    if (typeof payload.isPlaying === 'boolean' || typeof payload.isPaused === 'boolean') {
      applyBackendPlaybackState({
        is_playing: payload.isPlaying,
        is_paused: payload.isPaused,
      });
    }
    if (typeof payload.position === 'number') {
      position = payload.position;
    }
    if (typeof payload.duration === 'number') {
      duration = payload.duration;
    }
  } finally {
    applyingExternalSync = false;
  }
}

// --- Event Listeners ---

function setupListeners() {
  if (listenersSetup) return;
  listenersSetup = true;

  listen<{ current: number; total: number; label?: string }>('library:scan-progress', (event) => {
    const { current, total, label } = event.payload;
    if (!Number.isFinite(current) || !Number.isFinite(total)) return;
    setImportProgress({
      active: true,
      current: Math.max(0, current),
      total: Math.max(0, total),
      label: label || 'Scanning music...',
    });
  });

  listen('library:scan-finished', () => {
    resetImportProgress();
  });

  listen<{ shuffle_mode?: ShuffleMode }>('settings:updated', (event) => {
    const mode = applyShuffleModeFromSettings(event.payload?.shuffle_mode);
    applyShuffleMode(mode);
  });

  listen<StoreSyncPayload>('player:store-sync', (event) => {
    const prevPlayingId = playingPlaylistId;
    const prevFile = currentFile;
    applyStoreSync(event.payload);
    if (
      currentFile &&
      (isPlaying || isPaused) &&
      (currentFile !== prevFile || playingPlaylistId !== prevPlayingId)
    ) {
      void prepareGaplessNext(currentFile);
    }
  });

  listen<{ path: string }>('player:track-changed', (event) => {
    const path = event.payload.path;

    // Protect recent manual plays from *stale* track-changed events (e.g. old gapless
    // poll advancing the previous queue right after the user clicked a different track).
    // Real gapless next after a short track ends inside this window and must update the UI.
    // Match against the queue we actually sent to the backend — not only the UI play order
    // (those can diverge under shuffle or right after playlist switches).
    if (Date.now() - lastManualPlayAt < 800 && lastPlayedFile && !sameTrackPath(path, lastPlayedFile)) {
      // Queue not yet confirmed by backend → reject all non-matching events.
      // The backend's suppress_gapless_until (400ms) guarantees no legitimate
      // advance can happen before the IPC round-trip (~1-5ms) completes.
      if (queueConfirmedForPlay !== playRequestId) {
        return;
      }
      if (!isLegitimateTrackAdvance(lastPlayedFile, path)) {
        return;
      }
    }

    const track =
      trackByPath.get(path) ??
      findTrackByPath(playingTracks, path) ??
      [...trackByPath.values()].find((t) => sameTrackPath(t.path, path));
    // Record what we're transitioning from (for track-ended double-advance guard).
    lastTrackChangedFromPath = currentFile ?? '';
    lastTrackChangedAt = Date.now();
    currentFile = track?.path ?? path;
    lastPlayedFile = currentFile ?? path;
    markSmartPlayed(currentFile ?? path);
    currentFileName = track
      ? trackDisplayTitle(track)
      : path.split(/[\\/]/).pop()?.replace(/\.[^/.]+$/, '') ?? null;
    position = 0;
    // Don't let seek-guard from the previous manual play swallow the next track's position.
    seekGuardPosition = 0;
    seekGuardUntil = Date.now() + (isCueVirtualPath(path) ? 350 : 200);
    const segDur = cueSegmentDuration(track);
    if (segDur != null) {
      duration = segDur;
    } else if (track?.duration_secs != null) {
      duration = track.duration_secs;
    }
    const playlistId = findPlaylistForTrack(currentFile ?? path);
    // Prefer to keep the current playingPlaylistId if the track is already in the current que/playing list.
    // This prevents findPlaylistForTrack (which returns the *first* matching playlist) from
    // switching us to a different playlist on manual clicks or gapless advances within the que.
    const isInCurrentPlaying =
      playingPlaylistId && playingTracks.some((t) => sameTrackPath(t.path, path));
    if (!isInCurrentPlaying && playlistId && playingPlaylistId !== VIRTUAL_ALL_ID && playingPlaylistId !== VIRTUAL_LIKED_ID) {
      playingPlaylistId = playlistId;
    }
    syncTrackIndex();
    // Keep shuffle cursor aligned after gapless auto-advance.
    if (shuffleEnabled) {
      ensureShuffleOrder();
      syncShufflePosition();
    }
    isPlaying = true;
    isPaused = false;
    lastPauseRequestAt = 0;
    scheduleSave();
    lastGaplessChangeAt = Date.now();
    void prepareGaplessNext(currentFile ?? path);
    // Apply per-track speed (or restore global) when gapless advances.
    void applyEffectivePlaybackRate(currentFile ?? path);
    syncWindowTitle();
  });

  // Backend already throttles when unfocused; this is a second guard so a
  // hidden/minimized WebView does not thrash Svelte reactivity during games.
  let lastHiddenPositionApply = 0;
  listen<{ position: number; duration: number; state?: string }>('player:position', (event) => {
    const newPos = event.payload.position;
    const backendDur = event.payload.duration;
    // Never let a full-file duration overwrite a tighter CUE INDEX length —
    // that is what made the seekbar "fly" on multi-file / image CUE albums.
    const track = currentTrack;
    const segDur = cueSegmentDuration(track);
    if (segDur != null && segDur > 0) {
      if (
        typeof backendDur === 'number' &&
        backendDur > 0 &&
        Math.abs(backendDur - segDur) <= 1.0
      ) {
        duration = backendDur;
      } else {
        duration = segDur;
      }
    } else if (typeof backendDur === 'number' && backendDur > 0) {
      duration = backendDur;
    }
    if (event.payload.state) {
      applyBackendPlaybackState({ state: event.payload.state });
    }

    if (
      Date.now() < seekGuardUntil &&
      Math.abs(newPos - seekGuardPosition) > 1
    ) {
      return;
    }

    if (Date.now() >= seekGuardUntil) {
      seekGuardUntil = 0;
    }

    if (typeof document !== 'undefined' && document.visibilityState === 'hidden') {
      const now = Date.now();
      if (now - lastHiddenPositionApply < 500) {
        return;
      }
      lastHiddenPositionApply = now;
    }

    // Clamp to known duration so a single bad absolute read cannot pin the bar at 100%+.
    if (duration > 0 && newPos > duration + 0.5) {
      if (Date.now() - lastManualPlayAt < 1200 || Date.now() - lastGaplessChangeAt < 800) {
        return;
      }
      position = Math.min(newPos, duration);
      return;
    }
    position = newPos;

    // Persist seekbar while playing (throttled) so cold start resumes mid-track.
    if (isPlaying && !isPaused) {
      const now = Date.now();
      if (now - lastPositionPersistAt >= POSITION_PERSIST_MS) {
        lastPositionPersistAt = now;
        scheduleSave();
      }
    }
  });

  listen<{ is_playing: boolean; is_paused: boolean }>('player:state', (event) => {
    applyBackendPlaybackState(event.payload);
  });

  listen('library:cleared', () => {
    playlists = [];
    libraryTracks = [];
    allPaths = [];
    likedPaths = [];
    currentFile = null;
    currentFileName = null;
    currentTrackIndex = -1;
    playingPlaylistId = null;
    activePlaylistId = null;
    shuffleOrder = [];
    shufflePosition = 0;
    isPlaying = false;
    isPaused = false;
    position = 0;
    syncWindowTitle();
    notifyTrackPropertiesCloseAll();
  });

  listen('covers:rebuilt', () => {
    void refreshCoversAfterRebuild();
  });

  listen<MusicFile>('track:metadata-updated', (event) => {
    const updated = event.payload;
    if (!updated?.path) return;
    // Serde omits None optionals — force-clear so the editor can wipe tags.
    const normalized: MusicFile = {
      ...updated,
      title: updated.title ?? null,
      artist: updated.artist ?? null,
      album: updated.album ?? null,
      genre: updated.genre ?? null,
      year: updated.year ?? null,
      track_number: updated.track_number ?? null,
    };
    mergeMetadataIntoPlaylists([normalized]);
    if (currentFile && pathKey(currentFile) === pathKey(updated.path)) {
      syncWindowTitle();
    }
  });

  listen<{
    files: MusicFile[];
    playlistId: string | null;
    namedPlaylist?: string | null;
    coverUrl?: string | null;
  }>('ytdlp:downloaded', (event) => {
    const files = event.payload.files ?? [];
    if (files.length === 0) return;

    const named = event.payload.namedPlaylist?.trim();
    const targetId = named
      ? ensurePlaylist(named, { select: true })
      : resolveDownloadPlaylistId(event.payload.playlistId);

    addScannedTracks(files, targetId);

    // Apply source cover for imported playlists/albums (VK, Spotify, SoundCloud, …)
    const coverUrl = event.payload.coverUrl?.trim();
    if (named && coverUrl) {
      void setPlaylistCoverFromUrl(targetId, coverUrl);
    } else if (named) {
      // Fallback: first track cover once tags are loaded
      const firstCover = files.find((f) => f.cover_path?.trim())?.cover_path?.trim();
      if (firstCover) {
        void setPlaylistCover(targetId, firstCover);
      }
    }
  });

  listen<{ path?: string }>('player:track-ended', (event) => {
    const endedPath = event.payload?.path;

    // Stale ended from a previous track (manual skip / gapless already advanced).
    // Prefer path match over a fixed time window so sub-second tracks can still
    // auto-advance after they legitimately finish.
    if (endedPath && currentFile && !sameTrackPath(endedPath, currentFile)) {
      return;
    }
    // Fallback when backend omits path: only ignore right after a gapless change,
    // and never for short tracks that finish inside that window.
    if (!endedPath && Date.now() - lastGaplessChangeAt < 600 && duration > 0.7) {
      return;
    }
    // If track-changed already processed the transition from this track
    // (gapless auto-advance), don't double-advance from track-ended.
    if (endedPath && lastTrackChangedFromPath &&
        sameTrackPath(endedPath, lastTrackChangedFromPath) &&
        Date.now() - lastTrackChangedAt < 2000) {
      return;
    }

    isPlaying = false;
    isPaused = false;
    lastPauseRequestAt = 0;
    position = 0;

    if (repeatMode === 'one' && currentFile) {
      void play(currentFile);
      return;
    }

    if (shuffleEnabled) {
      if (advanceShufflePosition()) {
        const idx = shuffleOrder[shufflePosition];
        if (idx >= 0 && idx < playingTracks.length) {
          void play(playingTracks[idx].path);
        }
      }
      return;
    }

    // Use a fresh index lookup — currentTrackIndex may be stale if
    // playingTracks was reordered or the playlist changed.
    const endedIdx = findTrackIndexByPath(playingTracks, currentFile);
    if (endedIdx >= 0 && endedIdx < playingTracks.length - 1) {
      void nextTrack();
    } else if (repeatMode === 'all' && playingTracks.length > 0) {
      void play(playingTracks[0].path);
    }
  });
}

// --- Store Export ---

export function createPlayerStore() {
  setupListeners();
  void setupTaskbar();
  void bootstrap();

  if (typeof window !== 'undefined') {
    // Flush the small SQLite app-state row before the main window closes.
    window.addEventListener('pagehide', flushSave);
    window.addEventListener('beforeunload', flushSave);
  }

  return {
    // State (getters)
    get isPlaying() { return isPlaying; },
    get isPaused() { return isPaused; },
    get position() { return position; },
    get duration() { return duration; },
    get volume() { return volume; },
    get currentFile() { return currentFile; },
    get currentFileName() { return currentFileName; },
    get currentTrack() { return currentTrack; },
    get tracks() { return tracks; },
    get playlists() { return playlists; },
    get activePlaylistId() { return activePlaylistId; },
    get activePlaylist() { return activePlaylist; },
    get activePlaylistName() { return activePlaylistName; },
    get playingPlaylistId() { return playingPlaylistId; },
    get playingPlaylist() { return playingPlaylist; },
    get playingTracks() { return playingTracks; },
    get currentTrackIndex() { return currentTrackIndex; },
    get shuffleEnabled() { return shuffleEnabled; },
    get repeatMode() { return repeatMode; },

    // Derived (getters)
    get progress() { return progress; },
    get hasTrack() { return hasTrack; },
    get hasCurrentTrack() { return hasCurrentTrack; },
    get hasTracks() { return hasTracks; },
    get hasPlayingTracks() { return hasPlayingTracks; },
    get hasAnyTracks() { return hasAnyTracks; },
    get hasPlaylists() { return hasPlaylists; },
    get hasNext() { return hasNext; },
    get hasPrev() { return hasPrev; },
    get allCount() { return allTracks.length; },
    get likedCount() { return likedTracks.length; },
    get playbackRate() { return playbackRate; },
    get formattedPosition() { return formattedPosition; },
    get formattedDuration() { return formattedDuration; },

    // Playlist actions
    createPlaylist,
    selectPlaylist,
    deletePlaylist,
    renamePlaylist,
    setPlaylistCover,
    clearPlaylistCover,
    removeTrack,
    setPlaylistTrackOrder,
    reorderTracksInView,
    copyTracksToPlaylist,
    moveTracksToPlaylist,
    addFolderToActivePlaylist,
    addDroppedPaths,
    addScannedTracks,
    createPlaylistsFromDroppedPaths,
    createPlaylistsFromScannedTracks,
    importM3uPlaylists,
    clearAll,

    // Player actions
    play,
    pause,
    resume,
    stop,
    seek,
    setVolume,
    togglePlayPause,
    nextTrack,
    prevTrack,
    toggleShuffle,
    toggleRepeat,
    setPlaybackRate,
    toggleLike,
    isLiked,
    setViewPlayOrder,
    getUpcomingTracks,
    init,
    ensureInit,
  };
}

// Singleton instance
let _instance: ReturnType<typeof createPlayerStore> | null = null;

export function getPlayerStore() {
  if (!_instance) {
    _instance = createPlayerStore();
  }
  return _instance;
}
