<script lang="ts">
  import { onMount } from 'svelte';
  import { isTauri } from '@tauri-apps/api/core';
  import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import {
    filterIncomingDropPaths,
    getExportTrackSession,
    isExportDragActive,
    shouldSuppressDropOverlay,
  } from '$lib/fileDrag';
  import { getPlayerStore, supportsPlaylistReorder, type MusicFile } from '$lib/stores/player.svelte';
  import {
    endExportTrackDragUi,
    setTrackDragCopyTarget,
    setTrackDragTargets,
  } from '$lib/stores/trackDrag.svelte';
  import { applyTrackDrop, hitTestTrackDrop } from '$lib/trackDrop';


  const player = getPlayerStore();

  let isDragging = $state(false);
  let toast = $state<string | null>(null);
  let scaleFactor = $state(1);
  let pendingPaths = $state<string[]>([]);
  let toastTimer: ReturnType<typeof setTimeout> | null = null;
  let lastHandledDropAt = 0;

  interface DroppedTracksPayload {
    files: MusicFile[];
    position: [number, number];
    message?: string | null;
  }

  function showToast(message: string) {
    toast = message;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
      toast = null;
      toastTimer = null;
    }, 3200);
  }

  function normalizePaths(paths: unknown): string[] {
    if (!Array.isArray(paths)) return [];
    return paths
      .map((entry) => (typeof entry === 'string' ? entry : String(entry)))
      .map((entry) => entry.trim())
      .filter(Boolean);
  }

  function playlistIdAt(x: number, y: number): string | null {
    const el = document.elementFromPoint(x, y);
    return el?.closest('[data-playlist-id]')?.getAttribute('data-playlist-id') ?? null;
  }

  function playlistNameAt(x: number, y: number): string | null {
    const el = document.elementFromPoint(x, y);
    return el?.closest('[data-playlist-name]')?.getAttribute('data-playlist-name') ?? null;
  }

  function updateDropOverlay(active: boolean, paths: string[] = pendingPaths) {
    if (getExportTrackSession()) {
      isDragging = false;
      return;
    }
    isDragging = active && !isExportDragActive() && !shouldSuppressDropOverlay(paths);
  }

  function updateExportTrackDropTarget(x: number, y: number) {
    const session = getExportTrackSession();
    if (!session) return;

    const canReorder = supportsPlaylistReorder(session.sourcePlaylistId);
    const target = hitTestTrackDrop(x, y, session.sourcePlaylistId, canReorder);
    setTrackDragCopyTarget(target.dropPlaylistId);
    setTrackDragTargets(target.dropPlaylistId, target.dropIndex);
  }

  function handleExportTrackDrop(x: number, y: number) {
    const session = getExportTrackSession();
    if (!session || !shouldHandleDrop()) return;

    const canReorder = supportsPlaylistReorder(session.sourcePlaylistId);
    const target = hitTestTrackDrop(x, y, session.sourcePlaylistId, canReorder);
    applyTrackDrop(player, session, target, showToast);
    endExportTrackDragUi();
  }

  function shouldHandleDrop(): boolean {
    const now = Date.now();
    if (now - lastHandledDropAt < 400) return false;
    lastHandledDropAt = now;
    return true;
  }

  function finishDrop(
    files: MusicFile[],
    position: [number, number],
    message?: string | null,
    sourcePaths?: string[],
  ) {
    if (!shouldHandleDrop()) return;

    if (sourcePaths) {
      const importPaths = filterIncomingDropPaths(sourcePaths);
      if (!importPaths) return;
      if (importPaths.length < sourcePaths.length) {
        const allowed = new Set(importPaths.map((path) => path.toLowerCase()));
        files = files.filter((file) => allowed.has(file.path.toLowerCase()));
        if (files.length === 0) return;
      }
    }

    if (message && files.length === 0) {
      showToast(message);
      return;
    }

    const x = position[0] / scaleFactor;
    const y = position[1] / scaleFactor;
    const playlistId = playlistIdAt(x, y);
    const playlistName = playlistNameAt(x, y);
    const added = player.addScannedTracks(files, playlistId);

    if (added > 0) {
      const target = playlistName ?? player.activePlaylist?.name ?? 'playlist';
      showToast(`Added ${added} track${added !== 1 ? 's' : ''} to ${target}`);
    } else if (files.length > 0) {
      showToast('Tracks are already in this playlist');
    } else if (message) {
      showToast(message);
    } else {
      showToast('No supported audio files found');
    }
  }

  async function handleNativeDrop(paths: string[], x: number, y: number) {
    if (!shouldHandleDrop()) return;

    const importPaths = filterIncomingDropPaths(paths);
    if (!importPaths) return;

    const playlistId = playlistIdAt(x / scaleFactor, y / scaleFactor);
    const added = await player.addDroppedPaths(importPaths, playlistId);

    if (added > 0) {
      const target = playlistNameAt(x / scaleFactor, y / scaleFactor)
        ?? player.activePlaylist?.name
        ?? 'playlist';
      showToast(`Added ${added} track${added !== 1 ? 's' : ''} to ${target}`);
    } else if (importPaths.length > 0) {
      showToast('No supported audio files found');
    }
  }

  onMount(() => {
    if (!isTauri()) {
      showToast('Drag & drop works only in the desktop app (npm run tauri dev)');
      return;
    }

    const unlisteners: Array<() => void> = [];
    const webviewWindow = getCurrentWebviewWindow();

    void getCurrentWindow()
      .scaleFactor()
      .then((scale) => {
        scaleFactor = scale;
      });

    void webviewWindow.listen<boolean>('muzeeka:drag-active', (event) => {
      updateDropOverlay(event.payload);
    }).then((unlisten) => unlisteners.push(unlisten));

    void webviewWindow.listen<DroppedTracksPayload>('muzeeka:dropped-tracks', (event) => {
      const { files, position, message } = event.payload;
      const sourcePaths = files.map((file) => file.path);
      finishDrop(files, position, message, sourcePaths);
    }).then((unlisten) => unlisteners.push(unlisten));

    // Fallback: native Tauri drag-drop API (in case Rust emit path fails)
    void webviewWindow.onDragDropEvent((event) => {
      const payload = event.payload;

      if (payload.type === 'enter') {
        const x = payload.position.x / scaleFactor;
        const y = payload.position.y / scaleFactor;
        pendingPaths = normalizePaths(payload.paths);
        if (getExportTrackSession()) {
          updateExportTrackDropTarget(x, y);
          updateDropOverlay(false);
          return;
        }
        updateDropOverlay(true, pendingPaths);
        return;
      }

      if (payload.type === 'over') {
        const x = payload.position.x / scaleFactor;
        const y = payload.position.y / scaleFactor;
        if (getExportTrackSession()) {
          updateExportTrackDropTarget(x, y);
          updateDropOverlay(false);
          return;
        }
        updateDropOverlay(true, pendingPaths);
        return;
      }

      if (payload.type === 'leave') {
        if (getExportTrackSession()) {
          setTrackDragCopyTarget(null);
          setTrackDragTargets(null, null);
        }
        updateDropOverlay(false);
        pendingPaths = [];
        return;
      }

      if (payload.type === 'drop') {
        updateDropOverlay(false);
        const x = payload.position.x / scaleFactor;
        const y = payload.position.y / scaleFactor;
        const dropped = normalizePaths(payload.paths);
        const paths = dropped.length > 0 ? dropped : pendingPaths;
        pendingPaths = [];

        if (paths.length === 0) {
          showToast('Drop failed: no file paths received');
          return;
        }

        if (getExportTrackSession() && !filterIncomingDropPaths(paths)) {
          handleExportTrackDrop(x, y);
          return;
        }

        const importPaths = filterIncomingDropPaths(paths);
        if (!importPaths) return;

        void handleNativeDrop(importPaths, payload.position.x, payload.position.y);
      }
    }).then((unlisten) => unlisteners.push(unlisten));

    return () => {
      for (const unlisten of unlisteners) unlisten();
      if (toastTimer) clearTimeout(toastTimer);
    };
  });
</script>

{#if isDragging}
  <div class="drop-overlay" aria-hidden="true">
    <div class="drop-card">
      <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
        <polyline points="17 8 12 3 7 8"/>
        <line x1="12" y1="3" x2="12" y2="15"/>
      </svg>
      <p class="drop-title">
        {#if player.activePlaylist}
          Add to «{player.activePlaylist.name}»
        {:else}
          Drop to create playlist
        {/if}
      </p>
      <p class="drop-hint">Drop files or folders here</p>
    </div>
  </div>
{/if}

{#if toast}
  <div class="drop-toast" role="status">{toast}</div>
{/if}

<style>
  @import './DragDropHandler.css';
</style>
