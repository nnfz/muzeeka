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
  import {
    getPlayerStore,
    isEditablePlaylist,
    supportsPlaylistReorder,
    type MusicFile,
  } from '$lib/stores/player.svelte';
  import {
    externalDrop,
    resetExternalDrop,
    setExternalDropActive,
    setExternalDropCtrl,
    setExternalDropHover,
  } from '$lib/stores/externalDrop.svelte';
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
  let ctrlHeld = $state(false);
  let sidebarHintStyle = $state('');
  let tracksHintStyle = $state('');

  interface DroppedTracksPayload {
    files: MusicFile[];
    position: [number, number];
    message?: string | null;
    paths?: string[] | null;
  }

  interface DragActivePayload {
    active: boolean;
    position?: [number, number] | null;
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

  function toClientPoint(physicalX: number, physicalY: number): { x: number; y: number } {
    return {
      x: physicalX / scaleFactor,
      y: physicalY / scaleFactor,
    };
  }

  /** Rect hit-test — more reliable than elementFromPoint during native OS drags. */
  function pointInElement(x: number, y: number, el: Element | null): boolean {
    if (!(el instanceof HTMLElement)) return false;
    const rect = el.getBoundingClientRect();
    return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
  }

  function isOverPlaylistSidebar(x: number, y: number): boolean {
    return pointInElement(x, y, document.querySelector('[data-playlist-sidebar]'));
  }

  function isOverTrackZone(x: number, y: number): boolean {
    return pointInElement(x, y, document.querySelector('[data-track-drop-zone]'));
  }

  function editablePlaylistAt(x: number, y: number): { id: string; name: string } | null {
    const rows = document.querySelectorAll('[data-playlist-id][data-playlist-name]');
    for (const row of rows) {
      if (!pointInElement(x, y, row)) continue;
      const id = row.getAttribute('data-playlist-id');
      const name = row.getAttribute('data-playlist-name');
      if (!id || !isEditablePlaylist(id)) continue;
      return { id, name: name ?? player.playlists.find((p) => p.id === id)?.name ?? 'playlist' };
    }
    return null;
  }

  function zoneHintStyle(selector: string): string {
    const node = document.querySelector(selector);
    if (!(node instanceof HTMLElement)) return '';
    const rect = node.getBoundingClientRect();
    return [
      `left:${Math.round(rect.left + rect.width / 2)}px`,
      `top:${Math.round(rect.top + Math.min(140, rect.height * 0.4))}px`,
      `max-width:${Math.max(160, Math.round(rect.width - 24))}px`,
    ].join(';');
  }

  function updateSidebarHintPosition() {
    sidebarHintStyle = zoneHintStyle('[data-playlist-sidebar]');
  }

  function updateTracksHintPosition() {
    tracksHintStyle = zoneHintStyle('[data-track-drop-zone]');
  }

  function updateExternalHover(x: number, y: number) {
    if (isOverPlaylistSidebar(x, y)) {
      const target = editablePlaylistAt(x, y);
      setExternalDropHover({
        zone: 'sidebar',
        playlistId: target?.id ?? null,
        playlistName: target?.name ?? null,
        ctrlHeld,
      });
      updateSidebarHintPosition();
      return;
    }

    if (isOverTrackZone(x, y)) {
      setExternalDropHover({
        zone: 'tracks',
        playlistId: null,
        playlistName: null,
        ctrlHeld,
      });
      updateTracksHintPosition();
      return;
    }

    setExternalDropHover({
      zone: 'none',
      playlistId: null,
      playlistName: null,
      ctrlHeld,
    });
  }

  function updateDropOverlay(active: boolean, paths: string[] = pendingPaths) {
    if (getExportTrackSession()) {
      isDragging = false;
      setExternalDropActive(false);
      return;
    }
    const show = active && !isExportDragActive() && !shouldSuppressDropOverlay(paths);
    isDragging = show;
    setExternalDropActive(show);
    if (!show) {
      resetExternalDrop();
    }
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

  function toastForCreateResult(result: { playlists: number; tracks: number; names: string[] }) {
    if (result.playlists <= 0) {
      showToast('No supported audio files found');
      return;
    }
    if (result.playlists === 1) {
      const name = result.names[0] ?? 'playlist';
      showToast(
        result.tracks > 0
          ? `Created «${name}» · ${result.tracks} track${result.tracks !== 1 ? 's' : ''}`
          : `Opened «${name}» (tracks already in playlist)`,
      );
      return;
    }
    showToast(
      `Created ${result.playlists} playlists · ${result.tracks} track${result.tracks !== 1 ? 's' : ''}`,
    );
  }

  async function handleSidebarDrop(
    paths: string[] | null,
    files: MusicFile[] | null,
    x: number,
    y: number,
  ) {
    if (ctrlHeld) {
      const target = editablePlaylistAt(x, y);
      if (!target) {
        showToast('Hold Ctrl and drop on a playlist to import');
        return;
      }

      let added = 0;
      if (paths && paths.length > 0) {
        added = await player.addDroppedPaths(paths, target.id);
      } else if (files && files.length > 0) {
        added = player.addScannedTracks(files, target.id);
      }

      if (added > 0) {
        showToast(`Added ${added} track${added !== 1 ? 's' : ''} to «${target.name}»`);
      } else if ((files && files.length > 0) || (paths && paths.length > 0)) {
        showToast('Tracks are already in this playlist');
      } else {
        showToast('No supported audio files found');
      }
      return;
    }

    // Create playlist(s) from folder / files
    if (paths && paths.length > 0) {
      const result = await player.createPlaylistsFromDroppedPaths(paths);
      toastForCreateResult(result);
      return;
    }

    if (files && files.length > 0) {
      const result = player.createPlaylistsFromScannedTracks(files, paths);
      toastForCreateResult(result);
      return;
    }

    showToast('No supported audio files found');
  }

  function finishDrop(
    files: MusicFile[],
    position: [number, number],
    message?: string | null,
    sourcePaths?: string[],
  ) {
    if (!shouldHandleDrop()) return;

    let paths = sourcePaths?.map((p) => p.trim()).filter(Boolean) ?? [];
    if (paths.length > 0) {
      const importPaths = filterIncomingDropPaths(paths);
      if (!importPaths) return;
      if (importPaths.length < paths.length) {
        const allowed = new Set(importPaths.map((path) => path.toLowerCase()));
        files = files.filter((file) => allowed.has(file.path.toLowerCase()));
        paths = importPaths;
        if (files.length === 0 && paths.length === 0) return;
      } else {
        paths = importPaths;
      }
    }

    if (message && files.length === 0 && paths.length === 0) {
      showToast(message);
      return;
    }

    const x = position[0] / scaleFactor;
    const y = position[1] / scaleFactor;

    if (isOverPlaylistSidebar(x, y)) {
      void handleSidebarDrop(paths.length > 0 ? paths : null, files, x, y);
      return;
    }

    // Regular import only inside the track list panel
    if (!isOverTrackZone(x, y)) {
      return;
    }

    const added = player.addScannedTracks(files, player.activePlaylistId);

    if (added > 0) {
      const target = player.activePlaylist?.name ?? 'playlist';
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

    const clientX = x / scaleFactor;
    const clientY = y / scaleFactor;

    if (isOverPlaylistSidebar(clientX, clientY)) {
      await handleSidebarDrop(importPaths, null, clientX, clientY);
      return;
    }

    // Regular import only inside the track list panel
    if (!isOverTrackZone(clientX, clientY)) {
      return;
    }

    const added = await player.addDroppedPaths(importPaths, player.activePlaylistId);

    if (added > 0) {
      const target = player.activePlaylist?.name ?? 'playlist';
      showToast(`Added ${added} track${added !== 1 ? 's' : ''} to ${target}`);
    } else if (importPaths.length > 0) {
      showToast('No supported audio files found');
    }
  }

  function isCtrlKey(e: KeyboardEvent): boolean {
    return e.key === 'Control' || e.key === 'Meta';
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

    const onKeyDown = (e: KeyboardEvent) => {
      if (isCtrlKey(e) || e.ctrlKey || e.metaKey) {
        ctrlHeld = true;
        setExternalDropCtrl(true);
      }
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (isCtrlKey(e) || (!e.ctrlKey && !e.metaKey)) {
        // On keyup of Ctrl/Meta, or any keyup that clears modifiers
        if (!e.ctrlKey && !e.metaKey) {
          ctrlHeld = false;
          setExternalDropCtrl(false);
        }
      }
    };
    const onBlur = () => {
      ctrlHeld = false;
      setExternalDropCtrl(false);
    };

    window.addEventListener('keydown', onKeyDown, true);
    window.addEventListener('keyup', onKeyUp, true);
    window.addEventListener('blur', onBlur);

    void webviewWindow.listen<DragActivePayload | boolean>('muzeeka:drag-active', (event) => {
      const payload = event.payload;
      // Support both { active, position } (current) and legacy boolean payload
      const active = typeof payload === 'boolean' ? payload : !!payload?.active;
      const position =
        typeof payload === 'object' && payload && Array.isArray(payload.position)
          ? payload.position
          : null;

      updateDropOverlay(active);
      if (!active) {
        pendingPaths = [];
        return;
      }
      if (position) {
        const { x, y } = toClientPoint(position[0], position[1]);
        updateExternalHover(x, y);
      }
    }).then((unlisten) => unlisteners.push(unlisten));

    void webviewWindow.listen<DroppedTracksPayload>('muzeeka:dropped-tracks', (event) => {
      const { files, position, message, paths } = event.payload;
      const sourcePaths = normalizePaths(paths);
      finishDrop(files, position, message, sourcePaths.length > 0 ? sourcePaths : undefined);
      updateDropOverlay(false);
    }).then((unlisten) => unlisteners.push(unlisten));

    // Fallback: native Tauri drag-drop API (in case Rust emit path fails)
    void webviewWindow.onDragDropEvent((event) => {
      const payload = event.payload;

      if (payload.type === 'enter') {
        const { x, y } = toClientPoint(payload.position.x, payload.position.y);
        pendingPaths = normalizePaths(payload.paths);
        if (getExportTrackSession()) {
          updateExportTrackDropTarget(x, y);
          updateDropOverlay(false);
          return;
        }
        updateDropOverlay(true, pendingPaths);
        updateExternalHover(x, y);
        return;
      }

      if (payload.type === 'over') {
        const { x, y } = toClientPoint(payload.position.x, payload.position.y);
        if (getExportTrackSession()) {
          updateExportTrackDropTarget(x, y);
          updateDropOverlay(false);
          return;
        }
        updateDropOverlay(true, pendingPaths);
        updateExternalHover(x, y);
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
        const { x, y } = toClientPoint(payload.position.x, payload.position.y);
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
      window.removeEventListener('keydown', onKeyDown, true);
      window.removeEventListener('keyup', onKeyUp, true);
      window.removeEventListener('blur', onBlur);
      if (toastTimer) clearTimeout(toastTimer);
      resetExternalDrop();
    };
  });

  const dropTitle = $derived.by(() => {
    if (externalDrop.zone === 'sidebar') {
      if (externalDrop.ctrlHeld) {
        if (externalDrop.targetPlaylistName) {
          return `Import into «${externalDrop.targetPlaylistName}»`;
        }
        return 'Import into playlist';
      }
      return 'Create playlist from folder';
    }
    if (externalDrop.zone === 'tracks') {
      if (player.activePlaylist) {
        return `Add to «${player.activePlaylist.name}»`;
      }
      return 'Drop to create playlist';
    }
    return '';
  });

  const dropHint = $derived.by(() => {
    if (externalDrop.zone === 'sidebar') {
      if (externalDrop.ctrlHeld) {
        return externalDrop.targetPlaylistId
          ? 'Release to add tracks'
          : 'Drop on a playlist while holding Ctrl';
      }
      return 'Release to create · hold Ctrl to import into a playlist';
    }
    if (externalDrop.zone === 'tracks') {
      return 'Drop files or folders here';
    }
    return '';
  });
</script>

{#if isDragging && externalDrop.zone === 'sidebar'}
  <div
    class="drop-zone-hint"
    class:import-mode={externalDrop.ctrlHeld}
    style={sidebarHintStyle}
    aria-hidden="true"
  >
    <p class="drop-title">{dropTitle}</p>
    <p class="drop-hint">{dropHint}</p>
  </div>
{:else if isDragging && externalDrop.zone === 'tracks'}
  <div
    class="drop-zone-hint"
    style={tracksHintStyle}
    aria-hidden="true"
  >
    <p class="drop-title">{dropTitle}</p>
    <p class="drop-hint">{dropHint}</p>
  </div>
{/if}

{#if toast}
  <div class="drop-toast" role="status">{toast}</div>
{/if}

<style>
  @import './DragDropHandler.css';
</style>
