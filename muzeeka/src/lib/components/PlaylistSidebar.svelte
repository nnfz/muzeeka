<script lang="ts">
  import ContextMenu from './ContextMenu.svelte';
  import TrackCover from './TrackCover.svelte';
  import { openContextMenuFromEvent, type ContextMenuItem } from '$lib/contextMenu';
  import { getPlayerStore, type Playlist } from '$lib/stores/player.svelte';

  const player = getPlayerStore();

  const STORAGE_WIDTH_KEY = 'muzeeka:sidebar-width';
  const DEFAULT_WIDTH = 220;
  const MIN_WIDTH = 200;
  const MAX_WIDTH = 300;

  function maxWidth(): number {
    return Math.min(MAX_WIDTH, Math.floor(window.innerWidth * 0.55));
  }

  function clampWidth(width: number): number {
    return Math.max(MIN_WIDTH, Math.min(maxWidth(), width));
  }

  function readStoredWidth(): number {
    const stored = localStorage.getItem(STORAGE_WIDTH_KEY);
    if (!stored) return DEFAULT_WIDTH;
    const parsed = Number.parseInt(stored, 10);
    return Number.isFinite(parsed) ? clampWidth(parsed) : DEFAULT_WIDTH;
  }

  let sidebarWidth = $state(readStoredWidth());
  let isResizing = $state(false);
  let editingId = $state<string | null>(null);
  let editingName = $state('');
  let hoveredPlaylistId = $state<string | null>(null);
  let contextMenu = $state<{ playlist: Playlist; x: number; y: number } | null>(null);

  let playlistMenuItems = $derived.by((): ContextMenuItem[] => {
    const target = contextMenu?.playlist;
    if (!target) return [];

    return [
      {
        id: 'rename',
        label: 'Rename',
        icon: 'rename',
        onSelect: () => startRename(target),
      },
      {
        id: 'delete',
        label: 'Delete',
        icon: 'delete',
        danger: true,
        onSelect: () => player.deletePlaylist(target.id),
      },
    ];
  });

  function persist() {
    localStorage.setItem(STORAGE_WIDTH_KEY, String(sidebarWidth));
  }

  function startResize(e: MouseEvent) {
    e.preventDefault();
    isResizing = true;

    const startX = e.clientX;
    const startWidth = sidebarWidth;

    function onMove(moveEvent: MouseEvent) {
      sidebarWidth = clampWidth(startWidth + (moveEvent.clientX - startX));
    }

    function onUp() {
      isResizing = false;
      persist();
      document.body.classList.remove('sidebar-resizing');
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    }

    document.body.classList.add('sidebar-resizing');
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  }

  function resetWidth() {
    sidebarWidth = DEFAULT_WIDTH;
    persist();
  }

  function closeContextMenu() {
    contextMenu = null;
  }

  function openPlaylistContextMenu(e: MouseEvent, playlist: Playlist) {
    const position = openContextMenuFromEvent(e);
    contextMenu = { playlist, ...position };
  }

  function startRename(playlist: Playlist) {
    editingId = playlist.id;
    editingName = playlist.name;
  }

  function commitRename() {
    if (editingId) {
      player.renamePlaylist(editingId, editingName);
    }
    editingId = null;
    editingName = '';
  }

  function handleRenameKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      commitRename();
    } else if (e.key === 'Escape') {
      editingId = null;
      editingName = '';
    }
  }

  function handleSidebarKeydown(e: KeyboardEvent) {
    if (e.key !== 'F2') return;
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
    if (editingId) return;

    const playlist = player.playlists.find((p) => p.id === hoveredPlaylistId);
    if (!playlist) return;

    e.preventDefault();
    closeContextMenu();
    startRename(playlist);
  }
</script>

<svelte:window onkeydown={handleSidebarKeydown} />

<aside
  class="sidebar glass"
  class:resizing={isResizing}
  style:width="{sidebarWidth}px"
>
  <div class="sidebar-header">
    <div class="section-label" data-tauri-drag-region>Muzeeka</div>
    <div class="header-drag" data-tauri-drag-region></div>
    <button class="icon-btn" onclick={() => player.createPlaylist()} aria-label="New playlist" title="New playlist">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <line x1="12" y1="5" x2="12" y2="19"/>
        <line x1="5" y1="12" x2="19" y2="12"/>
      </svg>
    </button>
  </div>

  <div class="playlist-list">
    {#if !player.hasPlaylists}
      <div class="empty-state" data-tauri-drag-region>
        <div class="empty-icon">
          <svg width="36" height="36" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <line x1="8" y1="6" x2="21" y2="6"/>
            <line x1="8" y1="12" x2="21" y2="12"/>
            <line x1="8" y1="18" x2="21" y2="18"/>
            <line x1="3" y1="6" x2="3.01" y2="6"/>
            <line x1="3" y1="12" x2="3.01" y2="12"/>
            <line x1="3" y1="18" x2="3.01" y2="18"/>
          </svg>
        </div>
        <p class="empty-title">No playlists yet</p>
        <p class="empty-hint">Create a playlist or drop music here</p>
        <button class="empty-btn" onclick={() => player.createPlaylist()}>
          New Playlist
        </button>
      </div>
    {:else}
      {#each player.playlists as playlist (playlist.id)}
        {@const isActive = playlist.id === player.activePlaylistId}
        {@const isPlayingFrom =
          player.isPlaying &&
          player.currentFile !== null &&
          playlist.tracks.some((t) => t.path === player.currentFile)}
        {@const firstTrack = playlist.tracks[0] ?? null}
        <div
          class="playlist-row"
          class:active={isActive}
          class:playing={isPlayingFrom}
          data-playlist-id={playlist.id}
          data-playlist-name={playlist.name}
          onmouseenter={() => (hoveredPlaylistId = playlist.id)}
          onmouseleave={() => {
            if (hoveredPlaylistId === playlist.id) hoveredPlaylistId = null;
          }}
        >
          <button
            class="playlist-item"
            onclick={() => player.selectPlaylist(playlist.id)}
            oncontextmenu={(e) => openPlaylistContextMenu(e, playlist)}
            title={playlist.name}
          >
            <div class="playlist-icon">
              <TrackCover track={firstTrack} />
            </div>

            <div class="playlist-details">
              {#if editingId === playlist.id}
                <!-- svelte-ignore a11y_autofocus -->
                <input
                  class="rename-input"
                  bind:value={editingName}
                  onblur={commitRename}
                  onkeydown={handleRenameKeydown}
                  autofocus
                />
              {:else}
                <span
                  class="playlist-name"
                  role="button"
                  tabindex="0"
                  ondblclick={() => startRename(playlist)}
                  onkeydown={(e) => e.key === 'Enter' && startRename(playlist)}
                >
                  {playlist.name}
                </span>
              {/if}
              <span class="playlist-count">
                {playlist.tracks.length} track{playlist.tracks.length !== 1 ? 's' : ''}
              </span>
            </div>
          </button>
        </div>
      {/each}
    {/if}
  </div>

  <button
    type="button"
    class="resize-handle"
    aria-label="Resize sidebar"
    onmousedown={startResize}
    ondblclick={resetWidth}
    title="Drag to resize, double-click to reset"
  ></button>
</aside>

<ContextMenu
  open={contextMenu !== null}
  x={contextMenu?.x ?? 0}
  y={contextMenu?.y ?? 0}
  items={playlistMenuItems}
  onclose={closeContextMenu}
/>

<style>
  @import './PlaylistSidebar.css';
</style>