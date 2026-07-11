import { prefetchCoverPaths } from '$lib/coverCache';
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
let lastPauseRequestAt = 0;
let applyingExternalSync = false;

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

let allTracks = $derived.by(() => {
  const trackByPath = buildTrackByPathMap();
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
  const trackByPath = buildTrackByPathMap();
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
  return playlists.find((p) => p.id === activePlaylistId)?.tracks ?? [];
});

let activePlaylist = $derived(
  playlists.find((p) => p.id === activePlaylistId) ?? null
);

let activePlaylistName = $derived.by(() => {
  if (activePlaylistId === VIRTUAL_ALL_ID) return 'All tracks';
  if (activePlaylistId === VIRTUAL_LIKED_ID) return 'Liked';
  return playlists.find((p) => p.id === activePlaylistId)?.name ?? null;
});

let playingPlaylist = $derived(
  playingPlaylistId
    ? (playlists.find((p) => p.id === playingPlaylistId) ?? null)
    : null
);

let playingTracks = $derived.by(() => {
  if (!playingPlaylistId) return [];
  if (playingPlaylistId === VIRTUAL_ALL_ID) return allTracks;
  if (playingPlaylistId === VIRTUAL_LIKED_ID) return likedTracks;
  return playlists.find((p) => p.id === playingPlaylistId)?.tracks ?? [];
});

// Search across ALL playlists so metadata survives playlist switches
let currentTrack = $derived.by(() => {
  if (!currentFile) return null;
  for (const playlist of playlists) {
    const found = playlist.tracks.find((t) => t.path === currentFile);
    if (found) return found;
  }
  return null;
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

function needsMetadata(track: MusicFile): boolean {
  return track.duration_secs == null || !track.cover_path;
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

  if (paths.length === 0) return;

  try {
    const enriched = await invoke<MusicFile[]>('library_fetch_metadata', { paths });
    mergeMetadataIntoPlaylists(enriched);
    prefetchCoverPaths(enriched.map((track) => track.cover_path));
    scheduleSave();
  } catch (e) {
    console.error('Failed to fetch track metadata:', e);
  }
}

function findPlaylistForTrack(path: string): string | null {
  // Prefer the current playing playlist if the track exists in it (important for "que" clicks and gapless).
  if (playingPlaylistId && playingTracks.some((t) => t.path === path)) {
    return playingPlaylistId;
  }
  for (const playlist of playlists) {
    if (playlist.tracks.some((track) => track.path === path)) {
      return playlist.id;
    }
  }
  return null;
}

function syncTrackIndex() {
  currentTrackIndex = playingTracks.findIndex((t) => t.path === currentFile);
  syncShufflePosition();
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
    if (configured && playlists.some((p) => p.id === configured)) return;

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
  if (configuredId && playlists.some((p) => p.id === configuredId)) {
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
      const exists = playlists.some((p) => p.tracks.some((t) => t.path === data.current_file));
      if (exists) {
        currentFile = data.current_file;
        const track = playlists
          .flatMap((p) => p.tracks)
          .find((t) => t.path === data.current_file);
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
    prefetchCoverPaths(
      playlists.flatMap((playlist) => playlist.tracks.map((track) => track.cover_path))
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
  if (!playlists.some((p) => p.id === id)) return;
  activePlaylistId = id;
  prefetchCoverPaths(
    playlists.find((p) => p.id === id)?.tracks.map((track) => track.cover_path) ?? []
  );
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

  const playlist = playlists.find((p) => p.id === targetId);
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

  const trackByPath = buildTrackByPathMap();
  const tracks = paths
    .map((path) => trackByPath.get(path))
    .filter((track): track is MusicFile => !!track);
  if (tracks.length === 0) return 0;

  return mergeTracksIntoPlaylist(targetPlaylistId, tracks);
}

function removeTracksFromPlaylist(paths: string[], playlistId: string) {
  if (!isEditablePlaylist(playlistId)) return;

  const pathSet = new Set(paths);
  const playlist = playlists.find((p) => p.id === playlistId);
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

  const trackByPath = buildTrackByPathMap();
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
    playlists.find((p) => p.id === playlistId)?.tracks.map((t) => t.path) ?? []
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
  } else if (!playlists.some((p) => p.id === targetId)) {
    return 0;
  }

  activePlaylistId = targetId;
  return mergeTracksIntoPlaylist(targetId, files);
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
    const track = playingTracks.find((t) => t.path === filePath) ??
      playlists.flatMap((p) => p.tracks).find((t) => t.path === filePath);
    if (track) {
      return [gaplessArgsForTrack(track, filePath)];
    }
    return [];
  }
  return orderedTracksFrom(filePath).map((track) =>
    gaplessArgsForTrack(track, track.path)
  );
}

async function prepareGaplessNext(filePath: string) {
  const queue = buildGaplessQueue(filePath);
  try {
    await invoke('player_prepare_next', { queue });
  } catch (e) {
    console.error('Failed to prepare gapless queue:', e);
  }
}

async function play(filePath: string) {
  try {
    await ensureInit();
    lastManualPlayAt = Date.now();
    lastPlayedFile = filePath;

    // DON'T call player_stop before player_play!
    // The backend's play_inner handles transitions properly:
    //   - CUE tracks in the same audio file → seek within the open stream (instant)
    //   - Preloaded next track → activate preloaded source (fast)
    //   - Different file → teardown + open new stream
    // Calling stop() first destroys the current_source and current_audio_path,
    // which prevents the CUE reuse optimization and causes glitches/delays.

    const track = playlists
      .flatMap((p) => p.tracks)
      .find((t) => t.path === filePath);
    // Prefer the currently viewed playlist (incl. virtual All/Liked) so that next/prev
    // operate over the collected list when playing from All or Liked views.
    let playlistId = activePlaylistId;
    if (!playlistId || (playlistId !== VIRTUAL_ALL_ID && playlistId !== VIRTUAL_LIKED_ID)) {
      playlistId = findPlaylistForTrack(filePath);
    }
    if (playlistId) {
      playingPlaylistId = playlistId;
    }

    // Compute queue using explicit tracks from the playlist at click time to avoid stale derived
    // playingTracks / currentTrackIndex. This fixes random jumps and wrong UI track after clicking
    // different tracks in the playing ("que") list.
    let queueToSend: any[];
    if (repeatMode === 'one') {
      queueToSend = [gaplessArgsForTrack(track || ({ path: filePath } as any), filePath)];
    } else if (playlistId) {
      const pl = playlists.find((p) => p.id === playlistId);
      if (pl) {
        const idx = pl.tracks.findIndex((t) => t.path === filePath);
        const slice = (idx >= 0) ? pl.tracks.slice(idx, idx + MAX_GAPLESS_FOLLOWING) : [track || {path: filePath} as any];
        queueToSend = slice.map((t) => gaplessArgsForTrack(t, t.path || filePath));
      } else {
        queueToSend = buildGaplessQueue(filePath);
      }
    } else {
      queueToSend = buildGaplessQueue(filePath);
    }

    // Block stale position events BEFORE the invoke so they can't flash
    // the seekbar. This is critical for CUE tracks: when switching backward
    // in the same audio file, the backend buffer still holds the old absolute
    // position (e.g. 130s), but cue_start gets updated to 0 → relative
    // position = 130 → seekbar jumps to max for a frame before resetting.
    seekGuardPosition = 0;
    seekGuardUntil = Date.now() + 600;
    position = 0;

    await invoke('player_play', {
      ...playOptionsForTrack(track, filePath),
      queue: queueToSend,
    });
    currentFile = filePath;
    const file = track;
    currentFileName = file
      ? trackDisplayTitle(file)
      : filePath.split(/[\\/]/).pop()?.replace(/\.[^/.]+$/, '') ?? null;
    position = 0;
    const meta = track || playlists.flatMap((p) => p.tracks).find((t) => t.path === filePath);
    if (meta?.duration_secs != null) {
      duration = meta.duration_secs;
    }
    syncTrackIndex();
    isPlaying = true;
    isPaused = false;
    lastPauseRequestAt = 0;
    lastGaplessChangeAt = Date.now();
    scheduleSave(); // persist current_file so it survives restart
    syncWindowTitle();
  } catch (e) {
    seekGuardUntil = 0;
    const message = typeof e === 'string' ? e : String(e);
    console.error('Failed to play:', message);
    isPlaying = false;
    isPaused = false;
  }
}

async function pause() {
  lastPauseRequestAt = Date.now();
  try {
    await invoke('player_pause');
    isPaused = true;
    isPlaying = false;
  } catch (e) {
    console.error('Failed to pause:', e);
  }
}

async function resume() {
  lastPauseRequestAt = 0;
  try {
    await invoke('player_resume');
    isPaused = false;
    isPlaying = true;
  } catch (e) {
    // Backend has no audio loaded (e.g. after app restart) — fall back to playing from start
    if (currentFile) {
      isPaused = false;
      await play(currentFile);
    } else {
      console.error('Failed to resume:', e);
    }
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
  // Persist to settings (without clobbering EQ etc)
  void (async () => {
    try {
      const current = await invoke<any>('settings_load');
      await invoke('settings_save', { data: { ...current, playback_rate: playbackRate } });
    } catch {}
  })();
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

async function togglePlayPause() {
  if (isPlaying) {
    await pause();
  } else if (isPaused) {
    await resume();
  } else if (currentFile) {
    await play(currentFile);
  } else if (hasPlayingTracks && currentTrackIndex >= 0) {
    await play(playingTracks[currentTrackIndex].path);
  } else if (hasPlayingTracks) {
    await play(playingTracks[0].path);
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
    const q = buildGaplessQueue(currentFile);
    void invoke('player_prepare_next', { queue: q }).catch(() => {});
  }
  scheduleSave();
}

function applyStoreSync(payload: StoreSyncPayload) {
  applyingExternalSync = true;
  try {
    if (payload.activePlaylistId !== undefined) {
      activePlaylistId = payload.activePlaylistId;
    }
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
        const track = playlists.flatMap((p) => p.tracks).find((t) => t.path === currentFile);
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
    if (typeof payload.isPlaying === 'boolean') {
      const inPauseWindow = (Date.now() - lastPauseRequestAt) < 800;
      if (payload.isPlaying && !inPauseWindow && !isPaused) {
        isPlaying = true;
        isPaused = false;
        lastPauseRequestAt = 0;
      } else if (!payload.isPlaying) {
        isPlaying = false;
      }
    }
    if (typeof payload.isPaused === 'boolean') {
      isPaused = payload.isPaused;
      if (payload.isPaused) lastPauseRequestAt = 0;
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
  listen<StoreSyncPayload>('player:store-sync', (event) => {
    applyStoreSync(event.payload);
    if (currentFile && (isPlaying || isPaused)) {
      void prepareGaplessNext(currentFile);
    }
  });

  listen<{ path: string }>('player:track-changed', (event) => {
    const path = event.payload.path;

    // Protect recent manual plays from stale track-changed events
    // (e.g. old gapless poll deciding to advance right after user clicked a different track in the que list).
    // This fixes UI showing wrong track (9) while playing the manually chosen one (4), and random jumps.
    if (Date.now() - lastManualPlayAt < 600 && lastPlayedFile && path !== lastPlayedFile) {
      return;
    }

    currentFile = path;
    const track = playlists.flatMap((p) => p.tracks).find((t) => t.path === path);
    currentFileName = track
      ? trackDisplayTitle(track)
      : path.split(/[\\/]/).pop()?.replace(/\.[^/.]+$/, '') ?? null;
    position = 0;
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

    if (event.payload.state === 'playing') {
      // Ignore 'playing' reports while we have a pending pause request (fade in progress)
      // or while the local state believes we are paused. This prevents the BASS
      // "still playing during volume fade" reports + any stale immediate notifies
      // (e.g. from remote controller) from flipping the button back to play icon.
      const inPauseWindow = (Date.now() - lastPauseRequestAt) < 800;
      if (!inPauseWindow && !isPaused) {
        isPlaying = true;
        isPaused = false;
        lastPauseRequestAt = 0;
      }
    } else if (event.payload.state === 'paused') {
      isPlaying = false;
      isPaused = true;
      lastPauseRequestAt = 0;
    } else if (event.payload.state === 'stopped') {
      isPlaying = false;
      isPaused = false;
      lastPauseRequestAt = 0;
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
    if (event.payload.is_playing) {
      const inPauseWindow = (Date.now() - lastPauseRequestAt) < 800;
      if (!inPauseWindow && !isPaused) {
        isPlaying = true;
        isPaused = false;
        lastPauseRequestAt = 0;
      }
    } else if (event.payload.is_paused) {
      isPlaying = false;
      isPaused = true;
      lastPauseRequestAt = 0;
    }
  });

  listen<{ files: MusicFile[]; playlistId: string | null }>('ytdlp:downloaded', (event) => {
    const files = event.payload.files ?? [];
    if (files.length === 0) return;
    const targetId = resolveDownloadPlaylistId(event.payload.playlistId);
    addScannedTracks(files, targetId);
  });

  listen('player:track-ended', () => {
    // Gapless advance emits track-changed; ignore a stale ended right after it.
    if (Date.now() - lastGaplessChangeAt < 600) return;

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
    removeTrack,
    setPlaylistTrackOrder,
    reorderTracksInView,
    copyTracksToPlaylist,
    moveTracksToPlaylist,
    addFolderToActivePlaylist,
    addDroppedPaths,
    addScannedTracks,

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