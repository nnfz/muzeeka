<script lang="ts">
  import {
    getPlayerStore,
    trackDisplayArtist,
    trackDisplayTitle,
    trackSearchText,
    type MusicFile,
  } from '$lib/stores/player.svelte';
  import { open } from '@tauri-apps/plugin-dialog';
  import TrackCover from './TrackCover.svelte';


  interface Props {
    searchQuery?: string;
  }

  type ColumnId = 'index' | 'title' | 'album' | 'duration';
  type SortDirection = 'asc' | 'desc';

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

  let filteredTracks = $derived(
    searchQuery.trim()
      ? player.tracks.filter((t) =>
          trackSearchText(t).includes(searchQuery.toLowerCase())
        )
      : player.tracks
  );

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
    const tracks = [...filteredTracks];
    if (!sortColumn) return tracks;

    const dir = sortDirection === 'asc' ? 1 : -1;
    return tracks.sort((a, b) => compareTracks(a, b, sortColumn!) * dir);
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

  function compareTracks(a: MusicFile, b: MusicFile, column: ColumnId): number {
    switch (column) {
      case 'index': {
        const ai = player.tracks.findIndex((t) => t.path === a.path);
        const bi = player.tracks.findIndex((t) => t.path === b.path);
        return ai - bi;
      }
      case 'title':
        return trackDisplayTitle(a).localeCompare(trackDisplayTitle(b), undefined, {
          sensitivity: 'base',
        });
      case 'album':
        return (a.album ?? '').localeCompare(b.album ?? '', undefined, {
          sensitivity: 'base',
        });
      case 'duration':
        return (a.duration_secs ?? -1) - (b.duration_secs ?? -1);
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

  function handleTrackClick(track: MusicFile) {
    player.play(track.path);
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
    {:else if !player.hasTracks}
      <div class="empty-state" data-tauri-drag-region>
        <p class="empty-title">Playlist is empty</p>
        <p class="empty-hint">Drop files or folders here</p>
        <button class="empty-btn" onclick={addTracksFromFolder}>
          Add Tracks
        </button>
      </div>
    {:else if filteredTracks.length === 0}
      <div class="empty-state" data-tauri-drag-region>
        <p class="empty-title">No matches</p>
        <p class="empty-hint">Try a different search term</p>
      </div>
    {:else}
      <div
        class="track-grid"
        class:is-resizing={resizingPair !== null}
        bind:this={gridEl}
        style="grid-template-columns: {gridTemplate}"
      >
        <div class="track-table-header">
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

        {#each displayedTracks as track, i (track.path)}
          {@const isActive = track.path === player.currentFile}
          <button
            class="track-row"
            class:active={isActive}
            class:playing={isActive && player.isPlaying}
            onclick={() => handleTrackClick(track)}
            title={`${trackDisplayTitle(track)} — ${trackDisplayArtist(track)}`}
          >
            {#each visibleColumns as column (column)}
              {#if column === 'index'}
                <span class="col-index">
                  {#if isActive && player.isPlaying}
                    <span class="mini-eq" aria-label="Playing">
                      <span></span><span></span><span></span>
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
                    <span class="track-artist">{trackDisplayArtist(track)}</span>
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
    {/if}
  </div>
</section>

<style>
  @import './TrackList.css';
</style>