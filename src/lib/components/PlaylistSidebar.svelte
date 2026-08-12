<script lang="ts">
  import ContextMenu from "./ContextMenu.svelte";
  import TrackCover from "./TrackCover.svelte";
  import {
    openContextMenuFromEvent,
    type ContextMenuItem,
  } from "$lib/contextMenu";
  import { resolvePlaylistCoverTrack } from "$lib/playlistCover";
  import {
    getPlayerStore,
    isEditablePlaylist,
    type Playlist,
    VIRTUAL_ALL_ID,
    VIRTUAL_LIKED_ID,
  } from "$lib/stores/player.svelte";
  import { externalDrop } from "$lib/stores/externalDrop.svelte";
  import { trackDrag } from "$lib/stores/trackDrag.svelte";
  import { reorderItemsAtBoundary } from "$lib/trackOrder";
  import { open } from "@tauri-apps/plugin-dialog";
  import { flip } from "svelte/animate";

  const player = getPlayerStore();

  const STORAGE_WIDTH_KEY = "muzeeka:sidebar-width";
  const DEFAULT_WIDTH = 220;
  const MIN_WIDTH = 200;
  const MAX_WIDTH = 300;

  /** Pointer travel before a click on a playlist becomes a reorder drag (px). */
  const DRAG_THRESHOLD = 5;
  /** Overshoot needed to move the drop slot, so boundaries don't flicker (px). */
  const DROP_INDEX_HYSTERESIS = 4;
  /** Distance from the list edges where dragging starts auto-scrolling (px). */
  const AUTOSCROLL_EDGE = 32;
  const AUTOSCROLL_MAX_SPEED = 18;
  /** Slack around the list where the drag keeps a drop target (px). */
  const DROP_SLACK = 60;
  const REORDER_FLIP_MS = 170;

  const AUDIO_DIALOG_EXTENSIONS = [
    "mp3",
    "flac",
    "ogg",
    "wav",
    "aac",
    "m4a",
    "wma",
    "opus",
    "ape",
    "mod",
    "s3m",
    "xm",
    "it",
    "cue",
  ];

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
  let editingName = $state("");
  let hoveredPlaylistId = $state<string | null>(null);
  let contextMenu = $state<{ playlist: Playlist; x: number; y: number } | null>(
    null,
  );
  let addMenu = $state<{ x: number; y: number } | null>(null);

  type PlaylistDragState = {
    id: string;
    startX: number;
    startY: number;
    pointerY: number;
    active: boolean;
    /** Insert boundary in the *original* list, or null when off target. */
    dropIndex: number | null;
    /** Row centers in list content space, frozen when the drag starts. */
    slots: number[];
  };

  let listEl = $state<HTMLDivElement | null>(null);
  let playlistDrag = $state<PlaylistDragState | null>(null);
  let dragPointerId: number | null = null;
  let autoScrollFrame: number | null = null;
  let suppressPlaylistClick = false;
  let suppressClickTimer: ReturnType<typeof setTimeout> | null = null;

  /** Live preview: the dragged playlist sits at its drop slot while dragging. */
  let displayedPlaylists = $derived.by((): Playlist[] => {
    const drag = playlistDrag;
    if (!drag?.active || drag.dropIndex === null) return player.playlists;
    return reorderItemsAtBoundary(
      player.playlists,
      [drag.id],
      drag.dropIndex,
      (playlist) => playlist.id,
    );
  });

  let playlistMenuItems = $derived.by((): ContextMenuItem[] => {
    const target = contextMenu?.playlist;
    if (!target) return [];

    const items: ContextMenuItem[] = [
      {
        id: "mix-mode",
        label: target.mix_mode ? "Disable Mix mode" : "Enable Mix mode",
        icon: "mix",
        onSelect: () => player.setPlaylistMixMode(target.id, !target.mix_mode),
      },
      {
        id: "cover",
        label: "Set cover image",
        icon: "image",
        onSelect: () => void pickPlaylistCover(target),
      },
    ];

    if (target.cover_path) {
      items.push({
        id: "clear-cover",
        label: "Remove cover image",
        icon: "delete",
        onSelect: () => void player.clearPlaylistCover(target.id),
      });
    }

    items.push(
      {
        id: "rename",
        label: "Rename",
        icon: "rename",
        onSelect: () => startRename(target),
      },
      {
        id: "delete",
        label: "Delete",
        icon: "delete",
        danger: true,
        onSelect: () => player.deletePlaylist(target.id),
      },
    );

    return items;
  });

  let addMenuItems = $derived.by((): ContextMenuItem[] => {
    if (!addMenu) return [];

    const canAddToCurrent = isEditablePlaylist(player.activePlaylistId);

    return [
      {
        id: "add-files-to-current",
        label: "Add files to current playlist",
        icon: "file",
        disabled: !canAddToCurrent,
        onSelect: () => void addFilesToCurrentPlaylist(),
      },
      {
        id: "add-folder-to-current",
        label: "Add folder to current playlist",
        icon: "folder",
        disabled: !canAddToCurrent,
        onSelect: () => void addFolderToCurrentPlaylist(),
      },
      {
        id: "add-folder-as-playlist",
        label: "Add folder as playlist",
        icon: "playlist",
        onSelect: () => void addFolderAsPlaylist(),
      },
      {
        id: "add-m3u",
        label: "Add M3U playlist",
        icon: "import",
        onSelect: () => void addM3uPlaylists(),
      },
    ];
  });

  function normalizeOpenPaths(selected: string | string[] | null): string[] {
    if (!selected) return [];
    if (Array.isArray(selected)) {
      return selected.map((p) => p.trim()).filter(Boolean);
    }
    const path = selected.trim();
    return path ? [path] : [];
  }

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
      document.body.classList.remove("sidebar-resizing");
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    }

    document.body.classList.add("sidebar-resizing");
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  function resetWidth() {
    sidebarWidth = DEFAULT_WIDTH;
    persist();
  }

  function closeContextMenu() {
    contextMenu = null;
  }

  function closeAddMenu() {
    addMenu = null;
  }

  function openPlaylistContextMenu(e: MouseEvent, playlist: Playlist) {
    closeAddMenu();
    const position = openContextMenuFromEvent(e);
    contextMenu = { playlist, ...position };
  }

  function openAddMenu(e: MouseEvent) {
    closeContextMenu();
    const position = openContextMenuFromEvent(e, { width: 260, height: 160 });
    addMenu = position;
  }

  async function pickPlaylistCover(playlist: Playlist) {
    const selected = await open({
      multiple: false,
      filters: [
        {
          name: "Images",
          extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp"],
        },
      ],
    });
    if (!selected || typeof selected !== "string") return;
    await player.setPlaylistCover(playlist.id, selected);
  }

  async function addFilesToCurrentPlaylist() {
    if (!isEditablePlaylist(player.activePlaylistId)) return;

    const paths = normalizeOpenPaths(
      await open({
        multiple: true,
        title: "Add files to playlist",
        filters: [
          { name: "Audio files", extensions: AUDIO_DIALOG_EXTENSIONS },
          { name: "All files", extensions: ["*"] },
        ],
      }),
    );
    if (paths.length === 0) return;
    await player.addDroppedPaths(paths, player.activePlaylistId);
  }

  async function addFolderToCurrentPlaylist() {
    if (!isEditablePlaylist(player.activePlaylistId)) return;

    const paths = normalizeOpenPaths(
      await open({
        multiple: true,
        directory: true,
        title: "Add folder to playlist",
      }),
    );
    if (paths.length === 0) return;
    await player.addDroppedPaths(paths, player.activePlaylistId);
  }

  async function addFolderAsPlaylist() {
    const paths = normalizeOpenPaths(
      await open({
        multiple: true,
        directory: true,
        title: "Add folder as playlist",
      }),
    );
    if (paths.length === 0) return;
    await player.createPlaylistsFromDroppedPaths(paths);
  }

  async function addM3uPlaylists() {
    const paths = normalizeOpenPaths(
      await open({
        multiple: true,
        title: "Import M3U playlist",
        filters: [
          { name: "M3U playlists", extensions: ["m3u", "m3u8"] },
          { name: "All files", extensions: ["*"] },
        ],
      }),
    );
    if (paths.length === 0) return;
    await player.importM3uPlaylists(paths);
  }

  function playPlaylist(playlistId: string, firstTrackPath?: string | null) {
    player.selectPlaylist(playlistId);
    const path = firstTrackPath ?? player.tracks[0]?.path;
    if (path) void player.play(path);
  }

  function playPlaylistFromButton(
    e: MouseEvent,
    playlistId: string,
    firstTrackPath?: string | null,
  ) {
    e.stopPropagation();
    playPlaylist(playlistId, firstTrackPath);
  }

  function handlePlaylistItemKeydown(e: KeyboardEvent, playlistId: string) {
    if (
      e.target instanceof HTMLInputElement ||
      e.target instanceof HTMLTextAreaElement
    )
      return;
    if (e.key !== "Enter" && e.key !== " ") return;

    e.preventDefault();
    player.selectPlaylist(playlistId);
  }

  function selectPlaylistFromClick(playlistId: string) {
    // Swallow the ghost click that ends a reorder drag.
    if (suppressPlaylistClick) return;
    player.selectPlaylist(playlistId);
  }

  function armSuppressPlaylistClick() {
    suppressPlaylistClick = true;
    if (suppressClickTimer) clearTimeout(suppressClickTimer);
    suppressClickTimer = setTimeout(() => {
      suppressPlaylistClick = false;
      suppressClickTimer = null;
    }, 80);
  }

  /**
   * Row centers in list content space (scroll-independent), in the order the
   * playlists are stored. Captured once per drag: the geometry must not move
   * with the live preview, or the drop slot would oscillate.
   */
  function captureDragSlots(): number[] {
    const el = listEl;
    if (!el) return [];

    const listTop = el.getBoundingClientRect().top;
    return [...el.querySelectorAll<HTMLElement>(".playlist-row")].map((row) => {
      const rect = row.getBoundingClientRect();
      return rect.top - listTop + el.scrollTop + rect.height / 2;
    });
  }

  function nextDropIndex(clientY: number): number | null {
    const el = listEl;
    const drag = playlistDrag;
    if (!el || !drag || drag.slots.length === 0) return null;

    const rect = el.getBoundingClientRect();
    if (clientY < rect.top - DROP_SLACK || clientY > rect.bottom + DROP_SLACK) {
      return null;
    }

    const y = clientY - rect.top + el.scrollTop;
    let index = 0;
    while (index < drag.slots.length && y > drag.slots[index]) index += 1;

    const prev = drag.dropIndex;
    if (prev === null || prev === index) return index;

    // Both slots share one boundary — require a clear crossing before flipping.
    const boundary = drag.slots[Math.min(prev, index)];
    if (index > prev && y - boundary < DROP_INDEX_HYSTERESIS) return prev;
    if (index < prev && boundary - y < DROP_INDEX_HYSTERESIS) return prev;
    return index;
  }

  function autoScrollStep() {
    autoScrollFrame = null;
    const el = listEl;
    const drag = playlistDrag;
    if (!el || !drag?.active) return;

    const rect = el.getBoundingClientRect();
    const above = rect.top + AUTOSCROLL_EDGE - drag.pointerY;
    const below = drag.pointerY - (rect.bottom - AUTOSCROLL_EDGE);
    const delta =
      above > 0 ? -scrollSpeed(above) : below > 0 ? scrollSpeed(below) : 0;

    if (delta !== 0) {
      const before = el.scrollTop;
      el.scrollTop = before + delta;
      if (el.scrollTop !== before) {
        drag.dropIndex = nextDropIndex(drag.pointerY);
      }
    }

    autoScrollFrame = requestAnimationFrame(autoScrollStep);
  }

  function scrollSpeed(distance: number): number {
    return Math.min(AUTOSCROLL_MAX_SPEED, 2 + distance / 3);
  }

  function cleanupPlaylistDrag() {
    window.removeEventListener("pointermove", onDragPointerMove);
    window.removeEventListener("pointerup", onDragPointerUp);
    window.removeEventListener("pointercancel", onDragPointerUp);
    dragPointerId = null;
    if (autoScrollFrame !== null) {
      cancelAnimationFrame(autoScrollFrame);
      autoScrollFrame = null;
    }
    document.body.classList.remove("playlist-reorder-dragging");
    playlistDrag = null;
  }

  function onPlaylistPointerDown(e: PointerEvent, playlist: Playlist) {
    if (e.button !== 0) return;
    if (editingId === playlist.id) return;
    if (player.playlists.length < 2) return;

    const target = e.target as HTMLElement | null;
    if (target?.closest(".playlist-play-btn") || target?.closest(".rename-input")) {
      return;
    }

    if (playlistDrag) cleanupPlaylistDrag();

    playlistDrag = {
      id: playlist.id,
      startX: e.clientX,
      startY: e.clientY,
      pointerY: e.clientY,
      active: false,
      dropIndex: null,
      slots: [],
    };
    dragPointerId = e.pointerId;
    // No setPointerCapture — capture breaks the drop when the row moves away.
    window.addEventListener("pointermove", onDragPointerMove);
    window.addEventListener("pointerup", onDragPointerUp);
    window.addEventListener("pointercancel", onDragPointerUp);
  }

  function onDragPointerMove(e: PointerEvent) {
    const drag = playlistDrag;
    if (!drag) return;

    if (!drag.active) {
      const travelled = Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY);
      if (travelled < DRAG_THRESHOLD) return;

      drag.active = true;
      drag.slots = captureDragSlots();
      document.body.classList.add("playlist-reorder-dragging");
      closeContextMenu();
      closeAddMenu();
    }

    drag.pointerY = e.clientY;
    drag.dropIndex = nextDropIndex(e.clientY);
    if (autoScrollFrame === null) {
      autoScrollFrame = requestAnimationFrame(autoScrollStep);
    }
  }

  /**
   * Also handles `pointercancel` — the WebView cancels the pointer when the
   * cursor leaves the window, and dropping where the preview showed it is
   * friendlier than silently losing the gesture.
   */
  function onDragPointerUp(e: PointerEvent) {
    if (dragPointerId !== null && e.pointerId !== dragPointerId) return;

    const drag = playlistDrag;
    const dropped =
      drag?.active && drag.dropIndex !== null
        ? { id: drag.id, dropIndex: drag.dropIndex }
        : null;
    const wasActive = drag?.active ?? false;
    cleanupPlaylistDrag();

    if (!wasActive) return;
    armSuppressPlaylistClick();
    if (dropped) commitPlaylistOrder(dropped.id, dropped.dropIndex);
  }

  function commitPlaylistOrder(id: string, dropIndex: number) {
    const current = player.playlists;
    const next = reorderItemsAtBoundary(
      current,
      [id],
      dropIndex,
      (playlist) => playlist.id,
    );
    if (next.every((playlist, index) => playlist.id === current[index]?.id)) {
      return;
    }
    player.reorderPlaylists(next.map((playlist) => playlist.id));
  }

  $effect(() => () => cleanupPlaylistDrag());

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
    editingName = "";
  }

  function handleRenameKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      commitRename();
    } else if (e.key === "Escape") {
      editingId = null;
      editingName = "";
    }
  }

  function handleSidebarKeydown(e: KeyboardEvent) {
    if (e.key !== "F2") return;
    if (
      e.target instanceof HTMLInputElement ||
      e.target instanceof HTMLTextAreaElement
    )
      return;
    if (editingId) return;

    const playlist = player.playlists.find((p) => p.id === hoveredPlaylistId);
    if (!playlist) return;

    e.preventDefault();
    closeContextMenu();
    closeAddMenu();
    startRename(playlist);
  }
</script>

<svelte:window onkeydown={handleSidebarKeydown} />

<aside
  class="sidebar glass"
  class:resizing={isResizing}
  class:external-create-target={externalDrop.active &&
    externalDrop.zone === "sidebar" &&
    !externalDrop.ctrlHeld}
  data-playlist-sidebar
  style:width="{sidebarWidth}px"
>
  <div class="sidebar-header">
    <div class="section-label">Library</div>
    <button
      class="icon-btn"
      onclick={() => player.createPlaylist()}
      oncontextmenu={openAddMenu}
      aria-label="New playlist"
      title="New playlist — right-click to import"
    >
      <svg
        width="16"
        height="16"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <line x1="12" y1="5" x2="12" y2="19" />
        <line x1="5" y1="12" x2="19" y2="12" />
      </svg>
    </button>
  </div>

  <div class="playlist-list" role="list">
    <div class="virtual-playlists" aria-label="Main playlists">
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
            <svg
              width="18"
              height="18"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <rect x="3" y="3" width="7" height="7" />
              <rect x="14" y="3" width="7" height="7" />
              <rect x="3" y="14" width="7" height="7" />
              <rect x="14" y="14" width="7" height="7" />
            </svg>
          </div>
          <div class="playlist-details">
            <span class="playlist-name">All tracks</span>
            <span class="playlist-count"
              >{player.allCount} track{player.allCount !== 1 ? "s" : ""}</span
            >
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
            <span
              class="playlist-icon-svg"
              style:--playlist-icon-svg={"url('/icons/heartfilled.svg')"}
              aria-hidden="true"
            ></span>
          </div>
          <div class="playlist-details">
            <span class="playlist-name">Liked</span>
            <span class="playlist-count"
              >{player.likedCount} track{player.likedCount !== 1
                ? "s"
                : ""}</span
            >
          </div>
        </button>
      </div>
    </div>

    <div class="user-playlists" bind:this={listEl}>
      {#if !player.hasPlaylists}
        <div class="empty-state" data-tauri-drag-region>
          <div class="empty-icon">
            <svg
              width="36"
              height="36"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <line x1="8" y1="6" x2="21" y2="6" />
              <line x1="8" y1="12" x2="21" y2="12" />
              <line x1="8" y1="18" x2="21" y2="18" />
              <line x1="3" y1="6" x2="3.01" y2="6" />
              <line x1="3" y1="12" x2="3.01" y2="12" />
              <line x1="3" y1="18" x2="3.01" y2="18" />
            </svg>
          </div>
          <p class="empty-title">No playlists yet</p>
          <p class="empty-hint">Create a playlist or drop a folder</p>
          <button class="empty-btn" onclick={() => player.createPlaylist()}>
            New Playlist
          </button>
        </div>
      {:else}
        {#each displayedPlaylists as playlist (playlist.id)}
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
            animate:flip={{ duration: REORDER_FLIP_MS }}
            class:active={isActive}
            class:playing={isPlayingFrom}
            class:has-current={hasCurrentStopped}
            class:mix-mode={!!playlist.mix_mode}
            class:dragging={playlistDrag?.active &&
              playlistDrag.id === playlist.id}
            class:drop-target={(trackDrag.isDraggingTracks &&
              trackDrag.copyTargetPlaylistId === playlist.id) ||
              (externalDrop.active &&
                externalDrop.ctrlHeld &&
                externalDrop.targetPlaylistId === playlist.id)}
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
              onpointerdown={(e) => onPlaylistPointerDown(e, playlist)}
              onclick={() => selectPlaylistFromClick(playlist.id)}
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
                    onclick={(e) =>
                      playPlaylistFromButton(e, playlist.id, firstTrack.path)}
                    aria-label={`Play ${playlist.name}`}
                    title={`Play ${playlist.name}`}
                  >
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 24 24"
                      fill="currentColor"
                      aria-hidden="true"
                    >
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
                    onkeydown={(e) =>
                      e.key === "Enter" && startRename(playlist)}
                  >
                    {playlist.name}
                  </span>
                {/if}
                <span class="playlist-count">
                  {#if playlist.mix_mode}
                    <span class="mix-badge" title="Mix mode" aria-label="Mix mode"
                      >Mix</span
                    >
                  {/if}
                  {playlist.tracks.length} track{playlist.tracks.length !== 1
                    ? "s"
                    : ""}
                </span>
              </div>
            </div>
          </div>
        {/each}
      {/if}
    </div>
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

<ContextMenu
  open={addMenu !== null}
  x={addMenu?.x ?? 0}
  y={addMenu?.y ?? 0}
  items={addMenuItems}
  onclose={closeAddMenu}
/>

<style>
  @import "./PlaylistSidebar.css";
</style>
