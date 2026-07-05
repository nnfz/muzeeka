<script lang="ts">
  import ContextMenu from './ContextMenu.svelte';
  import {
    getPlayerStore,
    trackDisplayArtist,
    trackDisplayTitle,
    trackSearchText,
    type MusicFile,
  } from '$lib/stores/player.svelte';
  import { openContextMenuFromEvent, type ContextMenuItem } from '$lib/contextMenu';
  import { open } from '@tauri-apps/plugin-dialog';
  import TrackCover from './TrackCover.svelte';


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
  const DEFAULT_LAYOUT: ColumnLayout = {
    index: 24,
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

  function normalizeIndexWidth(width: number): number {
    return Math.max(MIN_COLUMN_WIDTHS.index, Math.round(width));
  }

  function loadColumnLayout(): ColumnLayout {
    try {
      const raw = localStorage.getItem(STORAGE_COLUMN_LAYOUT_KEY);
      if (!raw) return { ...DEFAULT_LAYOUT };
      const parsed: unknown = JSON.parse(raw);

      if (parsed && typeof parsed === 'object') {
        const data = parsed as Record<string, unknown>;

        if (
          typeof data.index === 'number' &&
          typeof data.duration === 'number' &&
          typeof data.titleShare === 'number'
        ) {
          const index =
            data.index >= 28 ? DEFAULT_LAYOUT.index : normalizeIndexWidth(data.index);
          return {
            index,
            duration: Math.max(MIN_COLUMN_WIDTHS.duration, Math.round(data.duration)),
            titleShare: clamp(data.titleShare, 0.05, 0.95),
          };
        }

        if (typeof data.title === 'number' && typeof data.album === 'number') {
          const middle = data.title + data.album;
          const storedIndex =
            typeof data.index === 'number' ? Math.round(data.index) : DEFAULT_LAYOUT.index;
          return {
            index: storedIndex >= 28 ? DEFAULT_LAYOUT.index : normalizeIndexWidth(storedIndex),
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
    return { ...DEFAULT_LAYOUT };
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
  let contextMenu = $state<{ item: ListedTrack; x: number; y: number } | null>(null);

  let trackMenuItems = $derived.by((): ContextMenuItem[] => {
    const target = contextMenu?.item;
    if (!target) return [];

    return [
      {
        id: 'delete',
        label: 'Delete',
        icon: 'delete',
        danger: true,
        onSelect: () => player.removeTrack(target.track.path, target.playlistId),
      },
    ];
  });

  let isGlobalSearch = $derived(searchQuery.trim().length > 0);

  let listedTracks = $derived.by((): ListedTrack[] => {
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

    let index = layout.index;
    let duration = layout.duration;

    const maxIndex = available - duration - middleMin;
    const maxDuration = available - index - middleMin;
    index = clamp(index, MIN_COLUMN_WIDTHS.index, Math.max(MIN_COLUMN_WIDTHS.index, maxIndex));
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

      if (left === 'index' && right === 'title') {
        const maxIndex = available - startLayout.duration - middleMin;
        columnLayout = {
          ...startLayout,
          index: clamp(startLayout.index + delta, MIN_COLUMN_WIDTHS.index, maxIndex),
        };
        return;
      }

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
    if (left === 'index' && right === 'title') {
      columnLayout = { ...columnLayout, index: DEFAULT_LAYOUT.index };
    } else if (left === 'title' && right === 'album') {
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

  function openTrackContextMenu(e: MouseEvent, item: ListedTrack) {
    const position = openContextMenuFromEvent(e);
    contextMenu = { item, ...position };
  }

  function handleTrackClick(item: ListedTrack) {
    closeContextMenu();
    player.play(item.track.path);
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
    {#if !player.activePlaylist}
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

              {#if i < visibleColumns.length - 1}
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
          <button
            class="track-row"
            class:active={isActive}
            class:playing={isActive && player.isPlaying}
            class:paused={isActive && player.isPaused && !player.isPlaying}
            style="grid-template-columns: {gridTemplate}"
            onclick={() => handleTrackClick(item)}
            oncontextmenu={(e) => openTrackContextMenu(e, item)}
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
                <span class="col-duration">{formatDuration(track.duration_secs)}</span>
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