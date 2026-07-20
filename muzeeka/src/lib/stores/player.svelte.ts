import { clearCoverSrcCache, prefetchCoverPaths } from '$lib/coverCache';
import { collectPlaylistCoverPaths } from '$lib/playlistCover';
import { setupTaskbar } from '$lib/taskbar';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

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
  active_playlist_id: string | null;
  playing_playlist_id?: string | null;
  current_file: string | null;
  volume: number | null;
  liked_paths?: string[];
  all_paths?: string[];
  shuffle_enabled?: boolean;
  repeat_mode?: RepeatMode;
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
let currentFile = $state<string | null>(null);
let currentFileName = $state<string | null>(null);
let playlists = $state<Playlist[]>([]);
let activePlaylistId = $state<string | null>(null);
let playingPlaylistId = $state<string | null>(null);
let currentTrackIndex = $state(-1);
let shuffleEnabled = $state(false);
let shuffleOrder = $state<number[]>([]);
let shufflePosition = $state(0);
let repeatMode = $state<RepeatMode>('off');
let playbackRate = $state(1.0);
let likedPaths = $state<string[]>([]);
let allPaths = $state<string[]>([]);
let isInitialized = $state(false);
let initPromise: Promise<void> | null = null;
let persistReady = $state(false);
let saveTimer: ReturnType<typeof setTimeout> | null = null;
let lastGaplessChangeAt = 0;
let lastManualPlayAt = 0;
let lastPlayedFile = '';
/** Paths last sent to the backend gapless queue (same order the player will advance). */
let lastGaplessQueuePaths: string[] = [];
let lastPauseRequestAt = 0;
let applyingExternalSync = false;
let listenersSetup = false;

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
  const trackByPath = new Map<string, MusicFile>();
  for (const playlist of playlists) {
    for (const track of playlist.tracks) {
      if (!trackByPath.has(track.path)) {
        trackByPath.set(track.path, track);
      }
    }
  }
  return trackByPath;
}

function defaultAllPaths(): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const playlist of playlists) {
    for (const track of playlist.tracks) {
      if (!seen.has(track.path)) {
        seen.add(track.path);
        result.push(track.path);
      }
    }
  }
  return result;
}

function reorderPathList(list: string[], paths: string[], insertIndex: number): string[] {
  const movingSet = new Set(paths);
  const moving = list.filter((path) => movingSet.has(path));
  if (moving.length === 0) return list;

  const remaining = list.filter((path) => !movingSet.has(path));
  const insertAt = Math.max(0, Math.min(insertIndex, remaining.length));
  return [...remaining.slice(0, insertAt), ...moving, ...remaining.slice(insertAt)];
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

let playingTracks = $derived.by(() => {
  if (!playingPlaylistId) return [];
  if (playingPlaylistId === VIRTUAL_ALL_ID) return allTracks;
  if (playingPlaylistId === VIRTUAL_LIKED_ID) return likedTracks;
  return playlistById.get(playingPlaylistId)?.tracks ?? [];
});

// Search across ALL playlists so metadata survives playlist switches
let currentTrack = $derived.by(() => {
  if (!currentFile) return null;
  return trackByPath.get(currentFile) ?? null;
});

let progress = $derived(duration > 0 ? position / duration : 0);
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
  return track.duration_secs == null || !track.cover_path || isStaleCoverPath(track.cover_path);
}



function mergeMetadataIntoPlaylists(enriched: MusicFile[]) {
  if (enriched.length === 0) return;

  const byPath = new Map(enriched.map((track) => [track.path, track]));
  playlists = playlists.map((playlist) => ({
    ...playlist,
    tracks: playlist.tracks.map((track) => byPath.get(track.path) ?? track),
  }));

  if (currentFile && byPath.has(currentFile)) {
    syncWindowTitle();
  }
}

async function enrichTrackMetadata() {
  const paths = [
    ...new Set(
      playlists.flatMap((playlist) =>
        playlist.tracks.filter(needsMetadata).map((track) => track.path)
      )
    ),
  ];

  // After collecting which paths need work, mark cache as validated so
  // subsequent calls (e.g. after adding new tracks) don't re-scan everything.
  coverCacheValidated = true;

  if (paths.length === 0) return;

  try {
    const enriched = await invoke<MusicFile[]>('library_fetch_metadata', { paths });
    mergeMetadataIntoPlaylists(enriched);
    prefetchCoverPaths([
      ...enriched.map((track) => track.cover_path_full),
      ...enriched.map((track) => track.cover_path),
    ]);
    scheduleSave();
  } catch (e) {
    console.error('Failed to fetch track metadata:', e);
  }

}

/** Reload playlists + covers after Settings → Rebuild covers. */
async function refreshCoversAfterRebuild() {
  clearCoverSrcCache();
  coverCacheValidated = false;
  try {
    const data = await invoke<PlaylistsData>('playlists_load');
    const byId = new Map((data.playlists ?? []).map((p) => [p.id, p]));
    playlists = playlists.map((local) => {
      const remote = byId.get(local.id);
      if (!remote) return local;
      const remoteByPath = new Map(remote.tracks.map((t) => [t.path, t]));
      return {
        ...local,
        cover_path: remote.cover_path ?? null,
        tracks: local.tracks.map((t) => {
          const r = remoteByPath.get(t.path);
          if (!r) return { ...t, cover_path: null, cover_path_full: null };
          return {
            ...t,
            cover_path: r.cover_path,
            cover_path_full: r.cover_path_full,
          };
        }),
      };
    });
    // Also pick up playlists that only exist on disk (shouldn't normally differ).
    for (const remote of data.playlists ?? []) {
      if (!playlists.some((p) => p.id === remote.id)) {
        playlists = [...playlists, repairPlaylistTracks(remote)];
      }
    }
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
  if (playingPlaylistId && playingTracks.some((t) => t.path === path)) {
    return playingPlaylistId;
  }
  return playlistIdByTrackPath.get(path) ?? null;
}

function syncTrackIndex() {
  currentTrackIndex = playingTracks.findIndex((t) => t.path === currentFile);
  syncShufflePosition();
}

/** True when `toPath` is the immediate next track after `fromPath` in the active play order. */
function isNaturalQueueAdvance(fromPath: string, toPath: string): boolean {
  if (!fromPath || !toPath || fromPath === toPath || !hasPlayingTracks) return false;

  if (shuffleEnabled) {
    ensureShuffleOrder();
    const fromIdx = playingTracks.findIndex((t) => t.path === fromPath);
    if (fromIdx < 0) return false;
    const orderPos = shuffleOrder.indexOf(fromIdx);
    if (orderPos < 0) return false;
    if (orderPos < shuffleOrder.length - 1) {
      return playingTracks[shuffleOrder[orderPos + 1]]?.path === toPath;
    }
    return repeatMode === 'all' && playingTracks[shuffleOrder[0]]?.path === toPath;
  }

  const idx = playingTracks.findIndex((t) => t.path === fromPath);
  if (idx < 0) return false;
  if (idx < playingTracks.length - 1) {
    return playingTracks[idx + 1]?.path === toPath;
  }
  return repeatMode === 'all' && playingTracks[0]?.path === toPath;
}

/**
 * True when `toPath` is the next entry after `fromPath` in the queue we last sent to the backend.
 * Prefer this over UI play-order when deciding whether a track-changed event is a real gapless
 * advance — the backend only knows the queue we sent (critical for sub-second tracks that
 * finish inside the manual-play guard window).
 */
function isSentQueueAdvance(fromPath: string, toPath: string): boolean {
  if (!fromPath || !toPath || fromPath === toPath || lastGaplessQueuePaths.length < 2) {
    return false;
  }
  const fromIdx = lastGaplessQueuePaths.indexOf(fromPath);
  const toIdx = lastGaplessQueuePaths.indexOf(toPath);
  // Any forward step in the queue we sent counts — intermediate track-changed
  // events can be missed when several sub-second tracks fire in one poll window.
  return fromIdx >= 0 && toIdx > fromIdx;
}

/** Accept a track-changed event as a legitimate auto-advance (not a stale gapless poll). */
function isLegitimateTrackAdvance(fromPath: string, toPath: string): boolean {
  if (!fromPath || !toPath || fromPath === toPath) return false;
  // Backend queue is authoritative for what gapless will actually play next.
  if (isSentQueueAdvance(fromPath, toPath)) return true;
  if (isNaturalQueueAdvance(fromPath, toPath)) return true;
  // Also accept advance from the UI's current file (may lag lastPlayedFile by one event).
  if (currentFile && currentFile !== fromPath && isSentQueueAdvance(currentFile, toPath)) {
    return true;
  }
  if (currentFile && currentFile !== fromPath && isNaturalQueueAdvance(currentFile, toPath)) {
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

function shuffleIndices(count: number): number[] {
  const indices = Array.from({ length: count }, (_, i) => i);
  for (let i = indices.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [indices[i], indices[j]] = [indices[j], indices[i]];
  }
  return indices;
}

function rebuildShuffleOrder(keepCurrent = true) {
  if (playingTracks.length === 0) {
    shuffleOrder = [];
    shufflePosition = 0;
    return;
  }

  const indices = shuffleIndices(playingTracks.length);
  if (keepCurrent && currentTrackIndex >= 0) {
    const at = indices.indexOf(currentTrackIndex);
    if (at > 0) {
      indices.splice(at, 1);
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
  if (
    shuffleOrder.length !== playingTracks.length ||
    shuffleOrder.some((index) => index < 0 || index >= playingTracks.length)
  ) {
    rebuildShuffleOrder(currentTrackIndex >= 0);
    syncShufflePosition();
  }
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

function buildSaveData(): PlaylistsData {
  return {
    playlists,
    active_playlist_id: activePlaylistId,
    playing_playlist_id: playingPlaylistId,
    current_file: currentFile,
    volume,
    liked_paths: likedPaths,
    all_paths: allPaths,
    shuffle_enabled: shuffleEnabled,
    repeat_mode: repeatMode,
  };
}

function scheduleSave() {
  if (!persistReady || applyingExternalSync) return;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = null;
    invoke('playlists_save', { data: buildSaveData() }).catch((e) => {
      console.error('Failed to save playlists:', e);
    });
  }, 250);
}

async function loadPlaylists() {
  try {
    const data = await invoke<PlaylistsData>('playlists_load');
    playlists = (data.playlists ?? []).map(repairPlaylistTracks);
    activePlaylistId = data.active_playlist_id ?? playlists[0]?.id ?? null;
    if (typeof data.volume === 'number') {
      volume = data.volume;
    }
    if (Array.isArray(data.liked_paths)) {
      likedPaths = data.liked_paths.filter((p: any) => typeof p === 'string' && p);
    }
    if (Array.isArray(data.all_paths)) {
      allPaths = data.all_paths.filter((p: any) => typeof p === 'string' && p);
    }
    if (typeof data.shuffle_enabled === 'boolean') {
      shuffleEnabled = data.shuffle_enabled;
    }
    if (data.repeat_mode === 'off' || data.repeat_mode === 'all' || data.repeat_mode === 'one') {
      repeatMode = data.repeat_mode;
    }
    if (data.playing_playlist_id) {
      playingPlaylistId = data.playing_playlist_id;
    }
    // Restore last playing track so metadata/status survive Ctrl+R
    if (data.current_file) {
      const track = trackByPath.get(data.current_file);
      if (track) {
        currentFile = data.current_file;
        currentFileName = track ? trackDisplayTitle(track) : data.current_file.split(/[\\/]/).pop()?.replace(/\.[^/.]+$/, '') ?? null;
        if (!data.playing_playlist_id) {
          playingPlaylistId = findPlaylistForTrack(data.current_file);
        }
        // Note: isPaused stays false — player is freshly started, track is just "remembered"
        syncWindowTitle();
      }
    }
    syncTrackIndex();
    if (shuffleEnabled) {
      rebuildShuffleOrder(currentTrackIndex >= 0);
      syncShufflePosition();
    }
    prefetchCoverPaths([
      ...playlists.flatMap((playlist) =>
        playlist.tracks.flatMap((track) => [track.cover_path_full, track.cover_path])
      ),
      ...collectPlaylistCoverPaths(playlists),
    ]);
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
  const existing = playlists.find(
    (p) => p.name.toLowerCase() === trimmed.toLowerCase()
  );
  if (existing) return existing.id;

  const playlist: Playlist = {
    id: crypto.randomUUID(),
    name: trimmed,
    tracks: [],
  };
  playlists = [...playlists, playlist];
  if (options?.select) {
    activePlaylistId = playlist.id;
  }
  syncTrackIndex();
  scheduleSave();
  return playlist.id;
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
  const nextPlaylists = playlists.filter((p) => p.id !== id);
  playlists = nextPlaylists;

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
  } else if (targetId === playingPlaylistId) {
    syncTrackIndex();
    if (shuffleEnabled) {
      rebuildShuffleOrder(currentTrackIndex >= 0);
      syncShufflePosition();
    }
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
    prefetchCoverPaths([coverPath]);
    scheduleSave();
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
    prefetchCoverPaths([coverPath]);
    scheduleSave();
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
  scheduleSave();
}

function renamePlaylist(id: string, name: string) {
  const trimmed = name.trim();
  if (!trimmed) return;
  playlists = playlists.map((p) =>
    p.id === id ? { ...p, name: trimmed } : p
  );
  scheduleSave();
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

  if (playlistId === playingPlaylistId) {
    syncTrackIndex();
    if (shuffleEnabled) {
      rebuildShuffleOrder(currentTrackIndex >= 0);
      syncShufflePosition();
    }
  }

  scheduleSave();
}

function reorderLikedPaths(paths: string[], insertIndex: number) {
  likedPaths = reorderPathList(likedPaths, paths, insertIndex);
  scheduleSave();
}

function reorderAllPaths(paths: string[], insertIndex: number) {
  const base = allPaths.length > 0 ? allPaths : defaultAllPaths();
  allPaths = reorderPathList(base, paths, insertIndex);
  scheduleSave();
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
  } else if (playlistId === playingPlaylistId) {
    syncTrackIndex();
    if (shuffleEnabled) {
      rebuildShuffleOrder(currentTrackIndex >= 0);
      syncShufflePosition();
    }
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
    scheduleSave();
  } else if (sourcePlaylistId === VIRTUAL_ALL_ID) {
    playlists = playlists.map((p) =>
      p.id === targetPlaylistId
        ? p
        : { ...p, tracks: p.tracks.filter((track) => !pathSet.has(track.path)) }
    );
    if (allPaths.length > 0) {
      allPaths = allPaths.filter((path) => !pathSet.has(path));
    }
    scheduleSave();
  } else if (isEditablePlaylist(sourcePlaylistId)) {
    removeTracksFromPlaylist(paths, sourcePlaylistId);
  }

  return tracks.length;
}

function mergeTracksIntoPlaylist(playlistId: string, files: MusicFile[]): number {
  const existing = new Set(
    playlistById.get(playlistId)?.tracks.map((t) => t.path) ?? []
  );
  const newTracks = files.filter((f) => !existing.has(f.path));
  if (newTracks.length === 0) return 0;

  playlists = playlists.map((p) =>
    p.id === playlistId ? { ...p, tracks: [...p.tracks, ...newTracks] } : p
  );
  syncTrackIndex();
  if (shuffleEnabled) rebuildShuffleOrder(currentTrackIndex >= 0);
  prefetchCoverPaths(newTracks.map((track) => track.cover_path));
  scheduleSave();
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
  return mergeTracksIntoPlaylist(targetId, files);
}

/** Basename of a filesystem path (folder or file). */
function pathBasename(path: string): string {
  const cleaned = path.replace(/[\\/]+$/, '').trim();
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

function normalizePathPrefix(path: string): string {
  return path.trim().replace(/[\\/]+$/, '').replace(/\//g, '\\').toLowerCase();
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

  for (const path of normalizedPaths) {
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
  }

  if (filePaths.length > 0) {
    try {
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
    } catch (e) {
      console.error('Failed to create playlist from dropped files:', e);
    }
  }

  if (lastId) {
    activePlaylistId = lastId;
  }

  return { playlists: playlistCount, tracks: trackCount, names };
}

/**
 * Create playlists from already-scanned tracks, grouping by original drop paths when available.
 */
function createPlaylistsFromScannedTracks(
  files: MusicFile[],
  sourcePaths?: string[] | null,
): CreatePlaylistsFromDropResult {
  if (files.length === 0) {
    return { playlists: 0, tracks: 0, names: [] };
  }

  const paths = (sourcePaths ?? []).map((p) => p.trim()).filter(Boolean);
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

  for (const source of paths) {
    if (looksLikeMediaFile(source)) continue;
    const prefix = normalizePathPrefix(source);
    if (!prefix) continue;

    const group = files.filter((file) => {
      const fileKey = normalizePathPrefix(file.path);
      return fileKey === prefix || fileKey.startsWith(`${prefix}\\`);
    });
    if (group.length === 0) continue;

    for (const file of group) claimed.add(file.path);
    const name = pathBasename(source) || nextPlaylistName();
    const id = ensurePlaylist(name, { select: true });
    trackCount += mergeTracksIntoPlaylist(id, group);
    playlistCount += 1;
    names.push(name);
    lastId = id;
  }

  const rest = files.filter((file) => !claimed.has(file.path));
  if (rest.length > 0) {
    const name = nextPlaylistName();
    const id = ensurePlaylist(name, { select: true });
    trackCount += mergeTracksIntoPlaylist(id, rest);
    playlistCount += 1;
    names.push(name);
    lastId = id;
  }

  if (lastId) {
    activePlaylistId = lastId;
  }

  return { playlists: playlistCount, tracks: trackCount, names };
}

async function addFolderToActivePlaylist(directory: string) {
  if (!activePlaylistId) return;

  try {
    const files: MusicFile[] = await invoke('library_scan', { directory });
    mergeTracksIntoPlaylist(activePlaylistId, files);
  } catch (e) {
    console.error('Failed to add folder to playlist:', e);
  }
}

async function addDroppedPaths(paths: string[], playlistId?: string | null) {
  const normalizedPaths = paths.map((path) => path.trim()).filter(Boolean);
  if (normalizedPaths.length === 0) return 0;

  try {
    const files: MusicFile[] = await invoke('library_scan_paths', { paths: normalizedPaths });
    return addScannedTracks(files, playlistId);
  } catch (e) {
    console.error('Failed to add dropped paths:', e);
    return 0;
  }
}

// --- Player Actions ---

async function init() {
  if (isInitialized) return;
  if (initPromise) {
    await initPromise;
    return;
  }

  initPromise = (async () => {
    await invoke('player_init');
    await invoke('player_set_volume', { volume });

    // Restore persisted playback rate from settings (so rate survives app restart)
    try {
      const s = await invoke<{ playback_rate?: number }>('settings_load');
      if (typeof s?.playback_rate === 'number' && s.playback_rate > 0 && s.playback_rate !== 1) {
        const r = Math.max(0.25, Math.min(2, s.playback_rate));
        playbackRate = r;
        await invoke('player_set_playback_rate', { rate: r }).catch(() => {});
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

async function bootstrap() {
  await loadPlaylists();
  await syncDownloadPlaylistFromLibrary();
  await init();
  // Sync frontend state with backend after reload (Ctrl+R).
  // The backend may still be playing audio, but the frontend starts
  // with isPlaying=false / isPaused=false. Query the actual state.
  try {
    const state = await invoke<PlayerState>('player_get_state');
    if (state.is_playing || state.is_paused) {
      isPlaying = state.is_playing;
      isPaused = state.is_paused;
      position = state.position;
      duration = state.duration;
    }
  } catch (e) {
    console.error('Failed to sync player state after init:', e);
  }
}

function repairCueTrack(track: MusicFile): MusicFile {
  if (!track.path.includes('#cue:')) return track;

  const marker = '#cue:';
  const markerPos = track.path.lastIndexOf(marker);
  if (markerPos <= 0) return track;

  const audioPath = track.path.slice(0, markerPos);
  return {
    ...track,
    audio_path: track.audio_path ?? audioPath,
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
  return {
    filePath,
    audioPath: audioPathForTrack(track, filePath),
    cueStart: track.cue_start_secs ?? undefined,
    cueEnd: track.cue_end_secs ?? undefined,
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
    const trackIdx = playingTracks.findIndex((t) => t.path === filePath);
    if (trackIdx < 0) return [];
    const orderPos = shuffleOrder.indexOf(trackIdx);
    if (orderPos < 0) return [];
    return shuffleOrder.slice(orderPos, orderPos + MAX_GAPLESS_FOLLOWING).map((index) => playingTracks[index]);
  }

  const index = playingTracks.findIndex((t) => t.path === filePath);
  if (index < 0) return [];
  return playingTracks.slice(index, index + MAX_GAPLESS_FOLLOWING);
}

function buildGaplessQueue(filePath: string) {
  if (repeatMode === 'one') {
    const track = playingTracks.find((t) => t.path === filePath) ?? trackByPath.get(filePath);
    if (track) {
      return [gaplessArgsForTrack(track, filePath)];
    }
    return [gaplessArgsForTrack({ path: filePath } as MusicFile, filePath)];
  }
  const ordered = orderedTracksFrom(filePath).map((track) =>
    gaplessArgsForTrack(track, track.path)
  );
  if (ordered.length > 0) return ordered;
  // Fallback when the track is not yet in playingTracks (playlist switch race).
  const track = trackByPath.get(filePath);
  return [gaplessArgsForTrack(track ?? ({ path: filePath } as MusicFile), filePath)];
}

async function prepareGaplessNext(filePath: string) {
  const queue = buildGaplessQueue(filePath);
  rememberGaplessQueue(queue);
  try {
    await invoke('player_prepare_next', { queue });
  } catch (e) {
    console.error('Failed to prepare gapless queue:', e);
  }
}

let playRequestId = 0;

type PlayUiSnapshot = {
  currentFile: string | null;
  currentFileName: string | null;
  isPlaying: boolean;
  isPaused: boolean;
  position: number;
  duration: number;
  playingPlaylistId: string | null;
  lastPlayedFile: string | null;
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
  // Capture previous UI so a failed play can roll back (audio never switched).
  const previousUi = snapshotPlayUi();
  try {
    await ensureInit();
    if (requestId !== playRequestId) return;

    lastManualPlayAt = Date.now();
    lastPlayedFile = filePath;
    lastPauseRequestAt = 0;
    isPlaying = true;
    isPaused = false;

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
      prefetchCoverPaths([track.cover_path_full, track.cover_path], 2);
    }
    void invoke<string | null>('library_resolve_full_cover', { path: filePath })
      .then((fullPath) => {
        if (!fullPath) return;
        prefetchCoverPaths([fullPath], 1);
        // Keep playlist metadata in sync if we just created/shrank a full cover.
        const t = trackByPath.get(filePath);
        if (t && t.cover_path_full !== fullPath) {
          mergeMetadataIntoPlaylists([
            { ...t, cover_path_full: fullPath },
          ]);
        }
      })
      .catch(() => {});
    // Prefer the currently viewed playlist (incl. virtual All/Liked) so that next/prev
    // operate over the collected list when playing from All or Liked views.
    let playlistId = activePlaylistId;
    if (!playlistId || (playlistId !== VIRTUAL_ALL_ID && playlistId !== VIRTUAL_LIKED_ID)) {
      playlistId = findPlaylistForTrack(filePath);
    }
    if (playlistId) {
      playingPlaylistId = playlistId;
    }

    // Build gapless queue from the active play order (incl. shuffle). playingPlaylistId is
    // already set above so derived playingTracks matches what next/prev and the UI use.
    // Remember the exact paths we send — track-changed guards must match backend order,
    // otherwise sub-second tracks finish inside the manual-play window and the UI never advances.
    const queueToSend = buildGaplessQueue(filePath);
    rememberGaplessQueue(queueToSend);

    // Block stale position events BEFORE the invoke so they can't flash
    // the seekbar. This is critical for CUE tracks: when switching backward
    // in the same audio file, the backend buffer still holds the old absolute
    // position (e.g. 130s), but cue_start gets updated to 0 → relative
    // position = 130 → seekbar jumps to max for a frame before resetting.
    seekGuardPosition = 0;
    seekGuardUntil = Date.now() + 600;
    position = 0;

    // Update UI immediately — don't wait for IPC (file open + Discord sync can take 100ms+).
    currentFile = filePath;
    currentFileName = track
      ? trackDisplayTitle(track)
      : filePath.split(/[\\/]/).pop()?.replace(/\.[^/.]+$/, '') ?? null;
    const meta = track ?? trackByPath.get(filePath);
    if (meta?.duration_secs != null) {
      duration = meta.duration_secs;
    }
    syncTrackIndex();
    lastGaplessChangeAt = Date.now();
    scheduleSave();
    syncWindowTitle();

    if (requestId !== playRequestId) return;

    void invoke('player_play', {
      ...playOptionsForTrack(track, filePath),
      queue: queueToSend,
    }).catch((e) => {
      if (requestId !== playRequestId) return;
      const message = typeof e === 'string' ? e : String(e);
      console.error('Failed to play:', message);
      // Backend did not switch audio — restore previous track in the UI.
      restorePlayUi(previousUi);
    });
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
  void invoke('player_pause').catch((e) => {
    console.error('Failed to pause:', e);
    isPaused = false;
    isPlaying = true;
  });
}

function resume() {
  lastPauseRequestAt = 0;
  isPaused = false;
  isPlaying = true;
  void invoke('player_resume').catch((e) => {
    if (currentFile) {
      void play(currentFile);
      return;
    }
    isPlaying = false;
    console.error('Failed to resume:', e);
  });
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
  const clamped = Math.max(0, duration > 0 ? Math.min(pos, duration) : pos);
  position = clamped;
  seekGuardPosition = clamped;
  seekGuardUntil = Date.now() + 400;

  try {
    await invoke('player_seek', { position: clamped });
  } catch (e) {
    seekGuardUntil = 0;
    console.error('Failed to seek:', e);
  }
}

function setVolume(vol: number) {
  volume = Math.max(0, Math.min(1, vol));
  void invoke('player_set_volume', { volume }).catch((e) => {
    console.error('Failed to set volume:', e);
  });
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
  if (index !== -1) {
    likedPaths = likedPaths.filter((p) => p !== path);
  } else {
    likedPaths = [...likedPaths, path];
  }
  scheduleSave();
}

function isLiked(path: string): boolean {
  return likedPaths.includes(path);
}

function togglePlayPause() {
  if (isPlaying) {
    pause();
  } else if (isPaused) {
    resume();
  } else if (currentFile) {
    void play(currentFile);
  } else if (hasPlayingTracks && currentTrackIndex >= 0) {
    void play(playingTracks[currentTrackIndex].path);
  } else if (hasPlayingTracks) {
    void play(playingTracks[0].path);
  }
}

async function nextTrack() {
  if (shuffleEnabled) {
    ensureShuffleOrder();
    if (shufflePosition < shuffleOrder.length - 1) {
      shufflePosition += 1;
    } else if (repeatMode === 'all') {
      shufflePosition = 0;
    } else {
      return;
    }
    const idx = shuffleOrder[shufflePosition];
    if (idx >= 0 && idx < playingTracks.length) {
      await play(playingTracks[idx].path);
    }
    return;
  }

  // Use a fresh index lookup to avoid stale currentTrackIndex after rapid switches.
  const idx = playingTracks.findIndex((t) => t.path === currentFile);
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
  const idx = playingTracks.findIndex((t) => t.path === currentFile);
  if (idx > 0) {
    const targetPath = playingTracks[idx - 1].path;
    await play(targetPath);
  }
}

function toggleShuffle() {
  shuffleEnabled = !shuffleEnabled;
  if (shuffleEnabled) {
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
    if (Date.now() - lastManualPlayAt < 600 && lastPlayedFile && path !== lastPlayedFile) {
      if (!isLegitimateTrackAdvance(lastPlayedFile, path)) {
        return;
      }
    }

    currentFile = path;
    lastPlayedFile = path;
    const track = trackByPath.get(path);
    currentFileName = track
      ? trackDisplayTitle(track)
      : path.split(/[\\/]/).pop()?.replace(/\.[^/.]+$/, '') ?? null;
    position = 0;
    // Don't let seek-guard from the previous manual play swallow the next track's position.
    seekGuardPosition = 0;
    seekGuardUntil = Date.now() + 200;
    if (track?.duration_secs != null) {
      duration = track.duration_secs;
    }
    const playlistId = findPlaylistForTrack(path);
    // Prefer to keep the current playingPlaylistId if the track is already in the current que/playing list.
    // This prevents findPlaylistForTrack (which returns the *first* matching playlist) from
    // switching us to a different playlist on manual clicks or gapless advances within the que.
    const isInCurrentPlaying = playingPlaylistId && playingTracks.some((t) => t.path === path);
    if (!isInCurrentPlaying && playlistId && playingPlaylistId !== VIRTUAL_ALL_ID && playingPlaylistId !== VIRTUAL_LIKED_ID) {
      playingPlaylistId = playlistId;
    }
    syncTrackIndex();
    isPlaying = true;
    isPaused = false;
    lastPauseRequestAt = 0;
    scheduleSave();
    lastGaplessChangeAt = Date.now();
    void prepareGaplessNext(path);
    syncWindowTitle();
  });

  listen<{ position: number; duration: number; state?: string }>('player:position', (event) => {
    const newPos = event.payload.position;
    duration = event.payload.duration;
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

    position = newPos;
  });

  listen<{ is_playing: boolean; is_paused: boolean }>('player:state', (event) => {
    applyBackendPlaybackState(event.payload);
  });

  listen('covers:rebuilt', () => {
    void refreshCoversAfterRebuild();
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
    if (endedPath && currentFile && endedPath !== currentFile) {
      return;
    }
    // Fallback when backend omits path: only ignore right after a gapless change,
    // and never for short tracks that finish inside that window.
    if (!endedPath && Date.now() - lastGaplessChangeAt < 600 && duration > 0.7) {
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
      ensureShuffleOrder();
      if (shufflePosition < shuffleOrder.length - 1) {
        shufflePosition += 1;
        void play(playingTracks[shuffleOrder[shufflePosition]].path);
      } else if (repeatMode === 'all') {
        shufflePosition = 0;
        void play(playingTracks[shuffleOrder[0]].path);
      }
      return;
    }

    if (currentTrackIndex < playingTracks.length - 1) {
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