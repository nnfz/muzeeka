<script lang="ts">
  import ContextMenu from './ContextMenu.svelte';
  import TrackCover from './TrackCover.svelte';
  import { openContextMenuFromEvent, type ContextMenuItem } from '$lib/contextMenu';
  import { resolvePlaylistCoverTrack } from '$lib/playlistCover';
  import { getPlayerStore, type Playlist, VIRTUAL_ALL_ID, VIRTUAL_LIKED_ID } from '$lib/stores/player.svelte';
  import { externalDrop } from '$lib/stores/externalDrop.svelte';
  import { trackDrag } from '$lib/stores/trackDrag.svelte';
  import { open } from '@tauri-apps/plugin-dialog';

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

    const items: ContextMenuItem[] = [
      {
        id: 'cover',
        label: 'Set cover image',
        icon: 'image',
        onSelect: () => void pickPlaylistCover(target),
      },
    ];

    if (target.cover_path) {
      items.push({
        id: 'clear-cover',
        label: 'Remove cover image',
        icon: 'delete',
        onSelect: () => void player.clearPlaylistCover(target.id),
      });
    }

    items.push(
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
    );

    return items;
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

  async function pickPlaylistCover(playlist: Playlist) {
    const selected = await open({
      multiple: false,
      filters: [
        {
          name: 'Images',
          extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp'],
        },
      ],
    });
    if (!selected || typeof selected !== 'string') return;
    await player.setPlaylistCover(playlist.id, selected);
  }

  function playPlaylist(playlistId: string, firstTrackPath?: string | null) {
    player.selectPlaylist(playlistId);
    const path = firstTrackPath ?? player.tracks[0]?.path;
    if (path) void player.play(path);
  }

  function playPlaylistFromButton(e: MouseEvent, playlistId: string, firstTrackPath?: string | null) {
    e.stopPropagation();
    playPlaylist(playlistId, firstTrackPath);
  }

  function handlePlaylistItemKeydown(e: KeyboardEvent, playlistId: string) {
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
    if (e.key !== 'Enter' && e.key !== ' ') return;

    e.preventDefault();
    player.selectPlaylist(playlistId);
  }

  /**
   * Focus the rename input and place the caret at the END of the text.
   * Uses multiple passes (immediate + rAF + setTimeout) to ensure the caret
   * is visible even after Svelte conditional rendering and value binding.
   */
  function focusRenameInput(node: HTMLInputElement) {
    const placeCaretAtEnd = () => {
      node.focus();
      const len = node.value ? node.value.length : 0;
      // Place caret at the very end (no text selection)
      try {
        node.setSelectionRange(len, len);
      } catch {}
      node.selectionStart = node.selectionEnd = len;
    };

    // Immediate attempt
    placeCaretAtEnd();

    // After DOM updates / value binding
    requestAnimationFrame(() => {
      placeCaretAtEnd();
      // Extra pass for timing in webview / Tauri
      setTimeout(placeCaretAtEnd, 0);
    });
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
  class:external-create-target={
    externalDrop.active && externalDrop.zone === 'sidebar' && !externalDrop.ctrlHeld
  }
  class:external-import-target={
    externalDrop.active && externalDrop.zone === 'sidebar' && externalDrop.ctrlHeld
  }
  data-playlist-sidebar
  style:width="{sidebarWidth}px"
>
  <div class="sidebar-header">
    <div class="section-label">Library</div>
    <button class="icon-btn" onclick={() => player.createPlaylist()} aria-label="New playlist" title="New playlist">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <line x1="12" y1="5" x2="12" y2="19"/>
        <line x1="5" y1="12" x2="19" y2="12"/>
      </svg>
    </button>
  </div>

  <div class="playlist-list" role="list">
    <!-- Virtual special playlists: All and Liked -->
    <div
      class="playlist-row virtual"
      role="listitem"
      class:active={player.activePlaylistId === VIRTUAL_ALL_ID}
      onmouseenter={() => (hoveredPlaylistId = VIRTUAL_ALL_ID)}
      onmouseleave={() => {
        if (hoveredPlaylistId === VIRTUAL_ALL_ID) hoveredPlaylistId = null;
      }}
    >
      <button
        class="playlist-item virtual-item"
        onclick={() => player.selectPlaylist(VIRTUAL_ALL_ID)}
        title="All tracks from every playlist"
      >
        <div class="playlist-icon virtual-icon">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <rect x="3" y="3" width="7" height="7" />
            <rect x="14" y="3" width="7" height="7" />
            <rect x="3" y="14" width="7" height="7" />
            <rect x="14" y="14" width="7" height="7" />
          </svg>
        </div>
        <div class="playlist-details">
          <span class="playlist-name">All tracks</span>
          <span class="playlist-count">{player.allCount} track{player.allCount !== 1 ? 's' : ''}</span>
        </div>
      </button>
    </div>

    <div
      class="playlist-row virtual"
      role="listitem"
      class:active={player.activePlaylistId === VIRTUAL_LIKED_ID}
      onmouseenter={() => (hoveredPlaylistId = VIRTUAL_LIKED_ID)}
      onmouseleave={() => {
        if (hoveredPlaylistId === VIRTUAL_LIKED_ID) hoveredPlaylistId = null;
      }}
    >
      <button
        class="playlist-item virtual-item"
        onclick={() => player.selectPlaylist(VIRTUAL_LIKED_ID)}
        title="Liked tracks"
      >
        <div class="playlist-icon virtual-icon liked-icon">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
          </svg>
        </div>
        <div class="playlist-details">
          <span class="playlist-name">Liked</span>
          <span class="playlist-count">{player.likedCount} track{player.likedCount !== 1 ? 's' : ''}</span>
        </div>
      </button>
    </div>

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
          (player.isPlaying || player.isPaused) &&
          player.currentFile !== null &&
          playlist.tracks.some((t) => t.path === player.currentFile)}
        {@const hasCurrentStopped =
          player.hasCurrentTrack &&
          playlist.tracks.some((t) => t.path === player.currentFile)}
        {@const firstTrack = playlist.tracks[0] ?? null}
        {@const coverTrack = resolvePlaylistCoverTrack(playlist)}
        <div
          class="playlist-row"
          role="listitem"
          class:active={isActive}
          class:playing={isPlayingFrom}
          class:has-current={hasCurrentStopped}
          class:drop-target={
            (trackDrag.isDraggingTracks && trackDrag.copyTargetPlaylistId === playlist.id) ||
            (externalDrop.active &&
              externalDrop.ctrlHeld &&
              externalDrop.targetPlaylistId === playlist.id)
          }
          data-playlist-id={playlist.id}
          data-playlist-name={playlist.name}
          onmouseenter={() => (hoveredPlaylistId = playlist.id)}
          onmouseleave={() => {
            if (hoveredPlaylistId === playlist.id) hoveredPlaylistId = null;
          }}
        >
          <div
            class="playlist-item"
            role="button"
            tabindex="0"
            onclick={() => player.selectPlaylist(playlist.id)}
            onkeydown={(e) => handlePlaylistItemKeydown(e, playlist.id)}
            oncontextmenu={(e) => openPlaylistContextMenu(e, playlist)}
            title={playlist.name}
          >
            <div class="playlist-icon">
              <TrackCover track={coverTrack} />
              {#if firstTrack}
                <button
                  type="button"
                  class="playlist-play-btn"
                  onclick={(e) => playPlaylistFromButton(e, playlist.id, firstTrack.path)}
                  aria-label={`Play ${playlist.name}`}
                  title={`Play ${playlist.name}`}
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                    <path d="M8 5v14l11-7z" />
                  </svg>
                </button>
              {/if}
            </div>

            <div class="playlist-details">
              {#if editingId === playlist.id}
                <input
                  class="rename-input"
                  use:focusRenameInput
                  bind:value={editingName}
                  onblur={commitRename}
                  onkeydown={handleRenameKeydown}
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
          </div>
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