import { prefetchCoverPaths } from '$lib/coverCache';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

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
}

export interface Playlist {
  id: string;
  name: string;
  tracks: MusicFile[];
}

interface PlaylistsData {
  playlists: Playlist[];
  active_playlist_id: string | null;
  current_file: string | null;
  volume: number | null;
}

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
let currentTrackIndex = $state(-1);
let shuffleEnabled = $state(false);
let shuffleOrder = $state<number[]>([]);
let shufflePosition = $state(0);
let repeatMode = $state<'off' | 'all' | 'one'>('off');
let isInitialized = $state(false);
let initPromise: Promise<void> | null = null;
let persistReady = $state(false);
let saveTimer: ReturnType<typeof setTimeout> | null = null;

// --- Derived ---

let tracks = $derived.by(() => {
  if (!activePlaylistId) return [];
  return playlists.find((p) => p.id === activePlaylistId)?.tracks ?? [];
});

let activePlaylist = $derived(
  playlists.find((p) => p.id === activePlaylistId) ?? null
);

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
let hasPlaylists = $derived(playlists.length > 0);
let hasNext = $derived(
  repeatMode === 'all' && hasTracks
    ? true
    : shuffleEnabled
      ? shufflePosition < shuffleOrder.length - 1
      : currentTrackIndex < tracks.length - 1
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

function syncTrackIndex() {
  currentTrackIndex = tracks.findIndex((t) => t.path === currentFile);
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
  if (tracks.length === 0) {
    shuffleOrder = [];
    shufflePosition = 0;
    return;
  }

  const indices = shuffleIndices(tracks.length);
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
    shuffleOrder.length !== tracks.length ||
    shuffleOrder.some((index) => index < 0 || index >= tracks.length)
  ) {
    rebuildShuffleOrder(currentTrackIndex >= 0);
    syncShufflePosition();
  }
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
    current_file: currentFile,
    volume,
  };
}

function scheduleSave() {
  if (!persistReady) return;
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
    playlists = data.playlists ?? [];
    activePlaylistId = data.active_playlist_id ?? playlists[0]?.id ?? null;
    if (typeof data.volume === 'number') {
      volume = data.volume;
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
        // Note: isPaused stays false — player is freshly started, track is just "remembered"
      }
    }
    syncTrackIndex();
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

function createPlaylist(name?: string): string {
  const playlist: Playlist = {
    id: crypto.randomUUID(),
    name: name?.trim() || nextPlaylistName(),
    tracks: [],
  };
  playlists = [...playlists, playlist];
  activePlaylistId = playlist.id;
  syncTrackIndex();
  scheduleSave();
  return playlist.id;
}

function selectPlaylist(id: string) {
  if (!playlists.some((p) => p.id === id)) return;
  activePlaylistId = id;
  syncTrackIndex();
  if (shuffleEnabled) rebuildShuffleOrder(currentTrackIndex >= 0);
  prefetchCoverPaths(
    playlists.find((p) => p.id === id)?.tracks.map((track) => track.cover_path) ?? []
  );
  scheduleSave();
}

function deletePlaylist(id: string) {
  const nextPlaylists = playlists.filter((p) => p.id !== id);
  playlists = nextPlaylists;

  if (activePlaylistId !== id) return;

  activePlaylistId = nextPlaylists[0]?.id ?? null;
  syncTrackIndex();
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

async function play(filePath: string) {
  try {
    await ensureInit();
    await invoke('player_play', { filePath });
    currentFile = filePath;
    const file = tracks.find((t) => t.path === filePath);
    currentFileName = file
      ? trackDisplayTitle(file)
      : filePath.split(/[\\/]/).pop()?.replace(/\.[^/.]+$/, '') ?? null;
    currentTrackIndex = tracks.findIndex((t) => t.path === filePath);
    syncShufflePosition();
    isPlaying = true;
    isPaused = false;
    scheduleSave(); // persist current_file so it survives restart
  } catch (e) {
    console.error('Failed to play:', e);
  }
}

async function pause() {
  try {
    await invoke('player_pause');
    isPaused = true;
    isPlaying = false;
  } catch (e) {
    console.error('Failed to pause:', e);
  }
}

async function resume() {
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

async function togglePlayPause() {
  if (isPlaying) {
    await pause();
  } else if (isPaused) {
    await resume();
  } else if (hasTracks && currentTrackIndex >= 0) {
    await play(tracks[currentTrackIndex].path);
  } else if (hasTracks) {
    await play(tracks[0].path);
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
    await play(tracks[shuffleOrder[shufflePosition]].path);
    return;
  }

  if (currentTrackIndex < tracks.length - 1) {
    await play(tracks[currentTrackIndex + 1].path);
  } else if (repeatMode === 'all' && tracks.length > 0) {
    await play(tracks[0].path);
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
      await play(tracks[shuffleOrder[shufflePosition]].path);
    }
    return;
  }

  if (hasPrev) {
    await play(tracks[currentTrackIndex - 1].path);
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
}

function toggleRepeat() {
  repeatMode = repeatMode === 'off' ? 'all' : repeatMode === 'all' ? 'one' : 'off';
}

// --- Event Listeners ---

function setupListeners() {
  listen<{ position: number; duration: number }>('player:position', (event) => {
    const newPos = event.payload.position;
    duration = event.payload.duration;

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
    isPlaying = event.payload.is_playing;
    isPaused = event.payload.is_paused;
  });

  listen('player:track-ended', () => {
    isPlaying = false;
    isPaused = false;
    position = 0;

    if (repeatMode === 'one' && currentFile) {
      void play(currentFile);
      return;
    }

    if (shuffleEnabled) {
      ensureShuffleOrder();
      if (shufflePosition < shuffleOrder.length - 1) {
        shufflePosition += 1;
        void play(tracks[shuffleOrder[shufflePosition]].path);
      } else if (repeatMode === 'all') {
        shufflePosition = 0;
        void play(tracks[shuffleOrder[0]].path);
      }
      return;
    }

    if (currentTrackIndex < tracks.length - 1) {
      void nextTrack();
    } else if (repeatMode === 'all' && tracks.length > 0) {
      void play(tracks[0].path);
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
    get currentTrackIndex() { return currentTrackIndex; },
    get shuffleEnabled() { return shuffleEnabled; },
    get repeatMode() { return repeatMode; },

    // Derived (getters)
    get progress() { return progress; },
    get hasTrack() { return hasTrack; },
    get hasCurrentTrack() { return hasCurrentTrack; },
    get hasTracks() { return hasTracks; },
    get hasPlaylists() { return hasPlaylists; },
    get hasNext() { return hasNext; },
    get hasPrev() { return hasPrev; },
    get formattedPosition() { return formattedPosition; },
    get formattedDuration() { return formattedDuration; },

    // Playlist actions
    createPlaylist,
    selectPlaylist,
    deletePlaylist,
    renamePlaylist,
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