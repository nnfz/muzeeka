<script lang="ts">
  import ContextMenu from './ContextMenu.svelte';
  import {
    getPlayerStore,
    trackDisplayArtist,
    trackDisplayTitle,
    trackSearchText,
    VIRTUAL_ALL_ID,
    VIRTUAL_LIKED_ID,
    type MusicFile,
  } from '$lib/stores/player.svelte';
  import { openContextMenuFromEvent, type ContextMenuItem } from '$lib/contextMenu';
  import { open } from '@tauri-apps/plugin-dialog';
  import TrackCover from './TrackCover.svelte';
  import { openDownloadWindow } from '$lib/stores/download.svelte';
  import { looksLikeMediaUrl } from '$lib/urlUtils';


  interface Props {
    searchQuery?: string;
  }

  type ColumnId = 'index' | 'title' | 'album' | 'duration';
  type SortDirection = 'asc' | 'desc';

  interface ListedTrack {
    track: MusicFile;
    playlistId: string;
    playlistName: string;
  }

  let { searchQuery = $bindable('') }: Props = $props();

  interface ColumnLayout {
    index: number;
    duration: number;
    titleShare: number;
  }

  const COLUMN_ORDER: ColumnId[] = ['index', 'title', 'album', 'duration'];
  const COL_GAP = 16;
  const FIXED_INDEX_WIDTH = 28;
  const DEFAULT_LAYOUT: ColumnLayout = {
    index: FIXED_INDEX_WIDTH,
    duration: 64,
    titleShare: 320 / (320 + 200),
  };
  const MIN_COLUMN_WIDTHS: Record<ColumnId, number> = {
    index: 22,
    title: 140,
    album: 80,
    duration: 52,
  };
  const STORAGE_COLUMN_LAYOUT_KEY = 'muzeeka:track-table:column-layout';
  const STORAGE_SORT_KEY = 'muzeeka:track-table:sort';

  const player = getPlayerStore();

  let gridEl = $state<HTMLDivElement | null>(null);
  let gridWidth = $state(0);
  let isNarrow = $state(false);
  let resizingPair = $state<{ left: ColumnId; right: ColumnId } | null>(null);

  function isColumnId(value: unknown): value is ColumnId {
    return value === 'index' || value === 'title' || value === 'album' || value === 'duration';
  }

  function clamp(value: number, min: number, max: number): number {
    return Math.max(min, Math.min(max, value));
  }

  function loadColumnLayout(): ColumnLayout {
    try {
      const raw = localStorage.getItem(STORAGE_COLUMN_LAYOUT_KEY);
      if (!raw) return { ...DEFAULT_LAYOUT, index: FIXED_INDEX_WIDTH };
      const parsed: unknown = JSON.parse(raw);

      if (parsed && typeof parsed === 'object') {
        const data = parsed as Record<string, unknown>;

        if (
          typeof data.duration === 'number' &&
          typeof data.titleShare === 'number'
        ) {
          return {
            index: FIXED_INDEX_WIDTH,
            duration: Math.max(MIN_COLUMN_WIDTHS.duration, Math.round(data.duration)),
            titleShare: clamp(data.titleShare, 0.05, 0.95),
          };
        }

        if (typeof data.title === 'number' && typeof data.album === 'number') {
          const middle = data.title + data.album;
          return {
            index: FIXED_INDEX_WIDTH,
            duration:
              typeof data.duration === 'number'
                ? Math.max(MIN_COLUMN_WIDTHS.duration, Math.round(data.duration))
                : DEFAULT_LAYOUT.duration,
            titleShare: middle > 0 ? clamp(data.title / middle, 0.05, 0.95) : DEFAULT_LAYOUT.titleShare,
          };
        }
      }
    } catch {
      /* ignore */
    }
    return { ...DEFAULT_LAYOUT, index: FIXED_INDEX_WIDTH };
  }

  function loadSort(): { column: ColumnId | null; direction: SortDirection } {
    try {
      const raw = localStorage.getItem(STORAGE_SORT_KEY);
      if (!raw) return { column: null, direction: 'asc' };
      const parsed: unknown = JSON.parse(raw);
      if (
        parsed &&
        typeof parsed === 'object' &&
        'column' in parsed &&
        'direction' in parsed
      ) {
        const column = (parsed as { column: unknown }).column;
        const direction = (parsed as { direction: unknown }).direction;
        if (
          (column === null || isColumnId(column)) &&
          (direction === 'asc' || direction === 'desc')
        ) {
          return { column, direction };
        }
      }
    } catch {
      /* ignore */
    }
    return { column: null, direction: 'asc' };
  }

  const initialSort = loadSort();

  let columnLayout = $state<ColumnLayout>(loadColumnLayout());
  let sortColumn = $state<ColumnId | null>(initialSort.column);
  let sortDirection = $state<SortDirection>(initialSort.direction);
  // --- Multi-selection state ---
  let selectedPaths = $state<Set<string>>(new Set());
  // Anchor index (in displayedTracks) for range selection.
  let selectionAnchor = $state<number | null>(null);

  let contextMenu = $state<{ item: ListedTrack; x: number; y: number } | null>(null);

  let trackMenuItems = $derived.by((): ContextMenuItem[] => {
    const target = contextMenu?.item;
    if (!target) return [];

    const items: ContextMenuItem[] = [];

    // Multi-selection: determine which paths the menu applies to.
    const affectedPaths = selectedPaths.size > 0 && selectedPaths.has(target.track.path)
      ? [...selectedPaths]
      : [target.track.path];
    const multi = affectedPaths.length > 1;

    // Like / Unlike
    const allLiked = affectedPaths.every((p) => player.isLiked(p));
    items.push({
      id: 'like',
      label: allLiked
        ? (multi ? `Remove ${affectedPaths.length} from Liked` : 'Remove from Liked')
        : (multi ? `Add ${affectedPaths.length} to Liked` : 'Add to Liked'),
      icon: 'heart',
      onSelect: () => affectedPaths.forEach((p) => {
        if (allLiked ? player.isLiked(p) : !player.isLiked(p)) player.toggleLike(p);
      }),
    });

    // Delete — only for real playlists; only if all selected are from the same playlist
    const pid = target.playlistId;
    const isRealPlaylist = pid && pid !== VIRTUAL_ALL_ID && pid !== VIRTUAL_LIKED_ID;
    const allSamePlaylist = isRealPlaylist && affectedPaths.every((p) => {
      const lt = listedTracks.find((l) => l.track.path === p);
      return lt && lt.playlistId === pid;
    });
    if (allSamePlaylist) {
      items.push({
        id: 'delete',
        label: multi ? `Delete ${affectedPaths.length} tracks` : 'Delete',
        icon: 'delete',
        danger: true,
        onSelect: () => affectedPaths.forEach((p) => player.removeTrack(p, pid)),
      });
    }
    return items;
  });

  let isGlobalSearch = $derived(searchQuery.trim().length > 0);
  let isUrlSearch = $derived(looksLikeMediaUrl(searchQuery));

  let listedTracks = $derived.by((): ListedTrack[] => {
    if (isUrlSearch) return [];

    if (isGlobalSearch) {
      const query = searchQuery.toLowerCase();
      return player.playlists.flatMap((playlist) =>
        playlist.tracks
          .filter((track) => trackSearchText(track).includes(query))
          .map((track) => ({
            track,
            playlistId: playlist.id,
            playlistName: playlist.name,
          }))
      );
    }

    if (!player.activePlaylistId) return [];

    return player.tracks.map((track) => ({
      track,
      playlistId: player.activePlaylistId!,
      playlistName: player.activePlaylist?.name ?? '',
    }));
  });

  let visibleColumns = $derived(
    isNarrow ? COLUMN_ORDER.filter((id) => id !== 'album') : COLUMN_ORDER
  );

  function availableWidth(columns: ColumnId[]): number {
    const gaps = (columns.length - 1) * COL_GAP;
    return Math.max(0, gridWidth - gaps);
  }

  function minMiddleWidth(columns: ColumnId[]): number {
    return columns
      .filter((id) => id !== 'index' && id !== 'duration')
      .reduce((sum, id) => sum + MIN_COLUMN_WIDTHS[id], 0);
  }

  function computeEffectiveWidths(columns: ColumnId[], layout: ColumnLayout): Record<ColumnId, number> {
    const available = availableWidth(columns);
    const middleMin = minMiddleWidth(columns);

    const index = FIXED_INDEX_WIDTH;
    let duration = layout.duration;

    const maxDuration = available - index - middleMin;
    duration = clamp(duration, MIN_COLUMN_WIDTHS.duration, Math.max(MIN_COLUMN_WIDTHS.duration, maxDuration));

    const middle = available - index - duration;

    let title = middle;
    let album = 0;

    if (columns.includes('album')) {
      title = Math.max(MIN_COLUMN_WIDTHS.title, Math.round(middle * layout.titleShare));
      album = middle - title;

      if (album < MIN_COLUMN_WIDTHS.album) {
        album = MIN_COLUMN_WIDTHS.album;
        title = middle - album;
      }
      if (title < MIN_COLUMN_WIDTHS.title) {
        title = MIN_COLUMN_WIDTHS.title;
        album = middle - title;
      }
    }

    return { index, title, album, duration };
  }

  let effectiveWidths = $derived(computeEffectiveWidths(visibleColumns, columnLayout));

  let gridTemplate = $derived(
    visibleColumns.map((id) => `${effectiveWidths[id]}px`).join(' ')
  );

  let displayedTracks = $derived.by(() => {
    const items = [...listedTracks];
    if (!sortColumn) return items;

    const dir = sortDirection === 'asc' ? 1 : -1;
    return items.sort((a, b) => compareTracks(a, b, sortColumn!) * dir);
  });

  $effect(() => {
    const el = gridEl;
    if (!el) return;

    const observer = new ResizeObserver(([entry]) => {
      gridWidth = entry.contentRect.width;
      isNarrow = entry.contentRect.width < 560;
    });
    observer.observe(el);
    return () => observer.disconnect();
  });

  function persistColumnLayout() {
    localStorage.setItem(STORAGE_COLUMN_LAYOUT_KEY, JSON.stringify(columnLayout));
  }

  function persistSort() {
    localStorage.setItem(
      STORAGE_SORT_KEY,
      JSON.stringify({ column: sortColumn, direction: sortDirection })
    );
  }

  function compareTracks(a: ListedTrack, b: ListedTrack, column: ColumnId): number {
    switch (column) {
      case 'index': {
        const ai = listedTracks.findIndex((item) => item.track.path === a.track.path);
        const bi = listedTracks.findIndex((item) => item.track.path === b.track.path);
        return ai - bi;
      }
      case 'title':
        return trackDisplayTitle(a.track).localeCompare(trackDisplayTitle(b.track), undefined, {
          sensitivity: 'base',
        });
      case 'album':
        return (a.track.album ?? '').localeCompare(b.track.album ?? '', undefined, {
          sensitivity: 'base',
        });
      case 'duration':
        return (a.track.duration_secs ?? -1) - (b.track.duration_secs ?? -1);
    }
  }

  function toggleSort(column: ColumnId) {
    if (sortColumn !== column) {
      sortColumn = column;
      sortDirection = 'asc';
    } else if (sortDirection === 'asc') {
      sortDirection = 'desc';
    } else {
      sortColumn = null;
      sortDirection = 'asc';
    }
    persistSort();
  }

  function startColumnResize(left: ColumnId, right: ColumnId, e: PointerEvent) {
    if (left === 'index') return;
    e.preventDefault();
    e.stopPropagation();
    resizingPair = { left, right };

    const startX = e.clientX;
    const columns = visibleColumns;
    const available = availableWidth(columns);
    const middleMin = minMiddleWidth(columns);
    const startLayout = { ...columnLayout };

    function onMove(moveEvent: PointerEvent) {
      const delta = moveEvent.clientX - startX;

      if (left === 'title' && right === 'album') {
        const middle = available - startLayout.index - startLayout.duration;
        const startTitle = middle * startLayout.titleShare;
        const nextTitle = clamp(startTitle + delta, MIN_COLUMN_WIDTHS.title, middle - MIN_COLUMN_WIDTHS.album);
        columnLayout = {
          ...startLayout,
          titleShare: nextTitle / middle,
        };
        return;
      }

      if (right === 'duration') {
        const maxDuration = available - startLayout.index - middleMin;
        columnLayout = {
          ...startLayout,
          duration: clamp(startLayout.duration - delta, MIN_COLUMN_WIDTHS.duration, maxDuration),
        };
      }
    }

    function onUp() {
      resizingPair = null;
      persistColumnLayout();
      document.body.classList.remove('track-table-resizing');
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
    }

    document.body.classList.add('track-table-resizing');
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function resetColumnPair(left: ColumnId, right: ColumnId) {
    if (left === 'index') return;
    if (left === 'title' && right === 'album') {
      columnLayout = { ...columnLayout, titleShare: DEFAULT_LAYOUT.titleShare };
    } else if (right === 'duration') {
      columnLayout = { ...columnLayout, duration: DEFAULT_LAYOUT.duration };
    }
    persistColumnLayout();
  }

  async function addTracksFromFolder() {
    const selected = await open({ directory: true });
    if (selected) {
      await player.addFolderToActivePlaylist(selected as string);
    }
  }

  function closeContextMenu() {
    contextMenu = null;
  }

  function openTrackContextMenu(e: MouseEvent, item: ListedTrack, index: number) {
    e.preventDefault();
    // If right-clicking an unselected track, focus only that track.
    if (!selectedPaths.has(item.track.path)) {
      selectedPaths = new Set();
      selectionAnchor = index;
    }
    const position = openContextMenuFromEvent(e);
    contextMenu = { item, ...position };
  }

  function handleTrackClick(item: ListedTrack, index: number, e: MouseEvent) {
    closeContextMenu();

    const ctrl = e.ctrlKey || e.metaKey;
    const shift = e.shiftKey;

    if (ctrl && shift && selectionAnchor !== null) {
      // Ctrl+Shift: add range from anchor to current to existing selection.
      e.preventDefault();
      const start = Math.min(selectionAnchor, index);
      const end = Math.max(selectionAnchor, index);
      const next = new Set(selectedPaths);
      for (let i = start; i <= end; i++) {
        next.add(displayedTracks[i].track.path);
      }
      selectedPaths = next;
    } else if (ctrl) {
      // Ctrl: toggle individual track selection.
      e.preventDefault();
      const next = new Set(selectedPaths);
      if (next.has(item.track.path)) {
        next.delete(item.track.path);
      } else {
        next.add(item.track.path);
        selectionAnchor = index;
      }
      selectedPaths = next;
    } else {
      // Regular click: clear selection and play.
      selectedPaths = new Set();
      selectionAnchor = index;
      player.play(item.track.path);
    }
  }

  function formatDuration(seconds: number | null | undefined): string {
    if (seconds == null || !Number.isFinite(seconds) || seconds <= 0) return '—';
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, '0')}`;
  }

  function columnLabel(column: ColumnId): string {
    switch (column) {
      case 'index':
        return '#';
      case 'title':
        return 'Title';
      case 'album':
        return 'Album';
      case 'duration':
        return 'Duration';
    }
  }
</script>

<section class="track-panel">
  <div class="track-list">
    {#if isUrlSearch}
      <div class="empty-state url-download-state" data-tauri-drag-region>
        <p class="empty-title">Download from link?</p>
        <p class="empty-hint">This looks like a media URL</p>
        <button class="empty-btn" onclick={() => openDownloadWindow(searchQuery)}>
          Download
        </button>
      </div>
    {:else if !player.activePlaylistId}
      <div class="empty-state" data-tauri-drag-region>
        <p class="empty-title">Select a playlist</p>
        <p class="empty-hint">Choose a playlist or drop music files here</p>
      </div>
    {:else if !isGlobalSearch && !player.hasTracks}
      <div class="empty-state" data-tauri-drag-region>
        <p class="empty-title">Playlist is empty</p>
        <p class="empty-hint">Drop files or folders here</p>
        <button class="empty-btn" onclick={addTracksFromFolder}>
          Add Tracks
        </button>
      </div>
    {:else if listedTracks.length === 0}
      <div class="empty-state" data-tauri-drag-region>
        <p class="empty-title">No matches</p>
        <p class="empty-hint">Try a different search term</p>
      </div>
    {:else}
      <div
        class="track-table"
        class:is-resizing={resizingPair !== null}
        bind:this={gridEl}
      >
        <div
          class="track-table-header"
          style="grid-template-columns: {gridTemplate}"
        >
          {#each visibleColumns as column, i (column)}
            <div
              class="header-cell"
              class:col-index={column === 'index'}
              class:col-title={column === 'title'}
              class:col-album={column === 'album'}
              class:col-duration={column === 'duration'}
            >
              <button
                type="button"
                class="sort-btn"
                class:sorted={sortColumn === column}
                onclick={() => toggleSort(column)}
                aria-label={`Sort by ${columnLabel(column)}`}
              >
                {#if column === 'duration'}
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                    <path d="M12 2C6.477 2 2 6.477 2 12s4.477 10 10 10 10-4.477 10-10S17.523 2 12 2zm0 18c-4.411 0-8-3.589-8-8s3.589-8 8-8 8 3.589 8 8-3.589 8-8 8zm.5-13H11v6l5.25 3.15.75-1.23-4.5-2.67V7z"/>
                  </svg>
                {:else}
                  <span>{columnLabel(column)}</span>
                {/if}

                {#if sortColumn === column}
                  <span class="sort-indicator" aria-hidden="true">
                    {#if sortDirection === 'asc'}
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
                        <path d="M7 14l5-5 5 5H7z"/>
                      </svg>
                    {:else}
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
                        <path d="M7 10l5 5 5-5H7z"/>
                      </svg>
                    {/if}
                  </span>
                {/if}
              </button>

              {#if i < visibleColumns.length - 1 && column !== 'index'}
                {@const rightColumn = visibleColumns[i + 1]}
                <button
                  type="button"
                  class="col-resizer"
                  class:active={resizingPair?.left === column}
                  aria-label={`Resize between ${columnLabel(column)} and ${columnLabel(rightColumn)}`}
                  onpointerdown={(e) => startColumnResize(column, rightColumn, e)}
                  ondblclick={() => resetColumnPair(column, rightColumn)}
                ></button>
              {/if}
            </div>
          {/each}
        </div>

        <div class="track-rows">
        {#each displayedTracks as item, i (item.track.path)}
          {@const track = item.track}
          {@const isActive = track.path === player.currentFile}
          {@const isSelected = selectedPaths.has(track.path)}
          <button
            class="track-row"
            class:active={isActive}
            class:playing={isActive && player.isPlaying}
            class:paused={isActive && player.isPaused && !player.isPlaying}
            class:selected={isSelected}
            style="grid-template-columns: {gridTemplate}"
            onclick={(e) => handleTrackClick(item, i, e)}
            oncontextmenu={(e) => openTrackContextMenu(e, item, i)}
            title={`${trackDisplayTitle(track)} — ${trackDisplayArtist(track)}${isGlobalSearch ? ` (${item.playlistName})` : ''}`}
          >
            {#each visibleColumns as column (column)}
              {#if column === 'index'}
                <span class="col-index">
                  {#if isActive && player.isPlaying}
                    <span class="mini-eq" aria-label="Playing">
                      <span></span><span></span><span></span>
                    </span>
                  {:else if isActive && player.isPaused}
                    <span class="paused-icon" aria-label="Paused">
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
                        <rect x="6" y="5" width="4" height="14" rx="1"/>
                        <rect x="14" y="5" width="4" height="14" rx="1"/>
                      </svg>
                    </span>
                  {:else}
                    <span class="track-num">{i + 1}</span>
                    <span class="play-icon" aria-hidden="true">
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
                        <path d="M8 5v14l11-7z"/>
                      </svg>
                    </span>
                  {/if}
                </span>
              {:else if column === 'title'}
                <span class="col-title">
                  <TrackCover track={track} />
                  <span class="title-group">
                    <span class="track-name">{trackDisplayTitle(track)}</span>
                    <span class="track-artist">
                      {trackDisplayArtist(track)}
                      {#if isGlobalSearch}
                        <span class="track-playlist"> · {item.playlistName}</span>
                      {/if}
                    </span>
                  </span>
                </span>
              {:else if column === 'album'}
                <span class="col-album">{track.album ?? '—'}</span>
              {:else}
                <span class="col-duration">
                  <span
                    role="button"
                    tabindex="0"
                    class="like-btn like-duration"
                    class:liked={player.isLiked(track.path)}
                    onclick={(e) => { e.stopPropagation(); player.toggleLike(track.path); }}
                    onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.stopPropagation(); e.preventDefault(); player.toggleLike(track.path); } }}
                    title={player.isLiked(track.path) ? 'Remove from Liked' : 'Add to Liked'}
                    aria-label={player.isLiked(track.path) ? 'Unlike track' : 'Like track'}
                  >
                    <svg width="13" height="13" viewBox="0 0 24 24" fill={player.isLiked(track.path) ? 'currentColor' : 'none'} stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                      <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
                    </svg>
                  </span>
                  <span class="duration-text">{formatDuration(track.duration_secs)}</span>
                </span>
              {/if}
            {/each}
          </button>
        {/each}
        </div>
      </div>
    {/if}
  </div>
</section>

<ContextMenu
  open={contextMenu !== null}
  x={contextMenu?.x ?? 0}
  y={contextMenu?.y ?? 0}
  items={trackMenuItems}
  onclose={closeContextMenu}
/>

<style>
  @import './TrackList.css';
</style>