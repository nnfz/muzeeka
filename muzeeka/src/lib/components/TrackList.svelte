<script lang="ts">
  import ContextMenu from './ContextMenu.svelte';
  import {
    getPlayerStore,
    isEditablePlaylist,
    supportsPlaylistReorder,
    trackDisplayArtist,
    trackDisplayTitle,
    VIRTUAL_ALL_ID,
    VIRTUAL_LIKED_ID,
    type MusicFile,
  } from '$lib/stores/player.svelte';
  import {
    beginExportTrackDragUi,
    resetTrackDrag,
    setTrackDragActive,
    setTrackDragCopyTarget,
    trackDrag as trackDragUi,
  } from '$lib/stores/trackDrag.svelte';
  import { externalDrop } from '$lib/stores/externalDrop.svelte';
  import { openContextMenuFromEvent, type ContextMenuItem } from '$lib/contextMenu';
  import { open } from '@tauri-apps/plugin-dialog';
  import { revealItemInDir } from '@tauri-apps/plugin-opener';
  import { invoke } from '@tauri-apps/api/core';
  import { audioPathsForDrag, startFileDrag } from '$lib/fileDrag';
  import { exportAudioPathForTrack } from '$lib/trackPaths';
  import TrackCover from './TrackCover.svelte';

  type ColumnId = 'index' | 'title' | 'album' | 'duration';
  type SortDirection = 'asc' | 'desc';

  interface ListedTrack {
    track: MusicFile;
    playlistId: string;
    playlistName: string;
  }

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

  const DRAG_THRESHOLD = 4;
  const FILE_EXPORT_EDGE_MARGIN = 14;

  let gridEl = $state<HTMLDivElement | null>(null);
  let rowsEl = $state<HTMLDivElement | null>(null);
  let gridWidth = $state(0);
  let rowsViewportHeight = $state(0);
  let rowsScrollTop = $state(0);
  let isNarrow = $state(false);
  let resizingPair = $state<{ left: ColumnId; right: ColumnId } | null>(null);

  const ROW_HEIGHT = 52;
  const VIRTUAL_OVERSCAN = 10;

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
  let playlistSubmenu = $state<{
    x: number;
    y: number;
    paths: string[];
    sourcePlaylistId: string;
    targetPlaylists: { id: string; name: string }[];
  } | null>(null);
  let dragToast = $state<string | null>(null);
  const MENU_WIDTH = 176;
  const SUBMENU_WIDTH = 176;
  const MENU_GAP = 6;
  const MENU_ITEM_HEIGHT = 34;
  const MENU_PADDING = 4;
  let dragToastTimer: ReturnType<typeof setTimeout> | null = null;

  interface TrackDragState {
    paths: string[];
    sourcePlaylistId: string;
    isCopy: boolean;
    startX: number;
    startY: number;
    active: boolean;
    dropIndex: number | null;
    dropPlaylistId: string | null;
    fileExportStarted: boolean;
  }

  let trackDrag = $state<TrackDragState | null>(null);
  let dragCaptureEl = $state<HTMLElement | null>(null);
  let dragPointerId = $state<number | null>(null);
  let suppressTrackClick = false;

  let canReorder = $derived(supportsPlaylistReorder(player.activePlaylistId));

  let trackMenuItems = $derived.by((): ContextMenuItem[] => {
    const target = contextMenu?.item;
    if (!target) return [];

    const items: ContextMenuItem[] = [];

    // Multi-selection: determine which paths the menu applies to.
    const affectedPaths = selectedPaths.size > 0 && selectedPaths.has(target.track.path)
      ? [...selectedPaths]
      : [target.track.path];
    const affectedTracks = affectedPaths
      .map((path) => displayedTracks.find((item) => item.track.path === path)?.track)
      .filter((track): track is MusicFile => !!track);
    const multi = affectedPaths.length > 1;

    items.push({
      id: 'find-on-disk',
      label: multi ? `Найти ${affectedTracks.length} на диске` : 'Найти на диске',
      icon: 'folder',
      disabled: affectedTracks.length === 0,
      onSelect: () => revealTracksOnDisk(affectedTracks),
    });

    // Lyrics actions — single track only (lyrics cache key is per-title/artist).
    if (!multi) {
      items.push({
        id: 'import-ttml',
        label: 'Импорт TTML',
        onSelect: () => void importTtmlForTrack(target.track),
      });
      items.push({
        id: 'refetch-lyrics',
        label: 'Найти текст',
        onSelect: () => void refetchLyricsForTrack(target.track),
      });
      items.push({
        id: 'clear-lyrics',
        label: 'Убрать текст',
        onSelect: () => void clearLyricsForTrack(target.track),
      });
    }

    const availableTargetPlaylists = player.playlists.filter((playlist) => playlist.id !== target.playlistId);
    items.push({
      id: 'add-to-playlist',
      label: multi ? `Добавить ${affectedPaths.length} в плейлист ›` : 'Добавить в плейлист ›',
      icon: 'playlist',
      disabled: availableTargetPlaylists.length === 0,
      closeOnSelect: false,
      onSelect: () => addTracksToPlaylist(affectedPaths, target.playlistId),
    });

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

  let listedTracks = $derived.by((): ListedTrack[] => {
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

  let listedIndexByPath = $derived.by(() => {
    const map = new Map<string, number>();
    listedTracks.forEach((item, index) => map.set(item.track.path, index));
    return map;
  });

  let displayedTracks = $derived.by(() => {
    const items = [...listedTracks];
    if (!sortColumn) return items;

    const dir = sortDirection === 'asc' ? 1 : -1;
    return items.sort((a, b) => compareTracks(a, b, sortColumn!) * dir);
  });

  let visibleRange = $derived.by(() => {
    const total = displayedTracks.length;
    if (total === 0) return { start: 0, end: 0, top: 0, bottom: 0 };

    const start = Math.max(0, Math.floor(rowsScrollTop / ROW_HEIGHT) - VIRTUAL_OVERSCAN);
    const visibleCount = Math.ceil(rowsViewportHeight / ROW_HEIGHT) + VIRTUAL_OVERSCAN * 2;
    const end = Math.min(total, start + Math.max(visibleCount, VIRTUAL_OVERSCAN * 2));

    return {
      start,
      end,
      top: start * ROW_HEIGHT,
      bottom: Math.max(0, (total - end) * ROW_HEIGHT),
    };
  });

  let visibleTracks = $derived(
    displayedTracks.slice(visibleRange.start, visibleRange.end)
  );

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

  $effect(() => {
    const el = rowsEl;
    if (!el) return;

    const updateViewport = () => {
      rowsViewportHeight = el.clientHeight;
      rowsScrollTop = el.scrollTop;
    };

    updateViewport();
    const observer = new ResizeObserver(updateViewport);
    observer.observe(el);
    return () => observer.disconnect();
  });

  function handleRowsScroll(e: Event) {
    rowsScrollTop = (e.currentTarget as HTMLDivElement).scrollTop;
  }

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
        const ai = listedIndexByPath.get(a.track.path) ?? -1;
        const bi = listedIndexByPath.get(b.track.path) ?? -1;
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

  function showDragToast(message: string, ms = 2400) {
    dragToast = message;
    if (dragToastTimer) clearTimeout(dragToastTimer);
    dragToastTimer = setTimeout(() => {
      dragToast = null;
      dragToastTimer = null;
    }, ms);
  }

  async function importTtmlForTrack(track: MusicFile) {
    const selected = await open({
      multiple: false,
      filters: [
        { name: 'TTML lyrics', extensions: ['ttml', 'xml'] },
        { name: 'All files', extensions: ['*'] },
      ],
    });
    const path = typeof selected === 'string' ? selected : null;
    if (!path) return;

    try {
      await invoke('lyrics_import_ttml', {
        title: trackDisplayTitle(track),
        artist: trackDisplayArtist(track),
        album: track.album ?? null,
        durationSecs:
          track.duration_secs != null && track.duration_secs > 0
            ? Math.round(track.duration_secs)
            : null,
        path,
        trackPath: track.path,
      });
      showDragToast('TTML импортирован');
    } catch (e) {
      console.error('Failed to import TTML:', e);
      showDragToast(e instanceof Error ? e.message : 'Не удалось импортировать TTML', 3200);
    }
  }

  async function clearLyricsForTrack(track: MusicFile) {
    try {
      await invoke('lyrics_clear', {
        title: trackDisplayTitle(track),
        artist: trackDisplayArtist(track),
        album: track.album ?? null,
        durationSecs:
          track.duration_secs != null && track.duration_secs > 0
            ? Math.round(track.duration_secs)
            : null,
        trackPath: track.path,
      });
      showDragToast('Текст убран');
    } catch (e) {
      console.error('Failed to clear lyrics:', e);
      showDragToast(e instanceof Error ? e.message : 'Не удалось убрать текст', 3200);
    }
  }

  async function refetchLyricsForTrack(track: MusicFile) {
    showDragToast('Ищем текст…', 8000);
    try {
      const found = await invoke<boolean>('lyrics_refetch', {
        title: trackDisplayTitle(track),
        artist: trackDisplayArtist(track),
        album: track.album ?? null,
        durationSecs:
          track.duration_secs != null && track.duration_secs > 0
            ? Math.round(track.duration_secs)
            : null,
        trackPath: track.path,
      });
      showDragToast(found ? 'Текст найден' : 'Текст не найден', found ? 2400 : 3200);
    } catch (e) {
      console.error('Failed to refetch lyrics:', e);
      showDragToast(e instanceof Error ? e.message : 'Не удалось найти текст', 3200);
    }
  }

  function closeContextMenu() {
    contextMenu = null;
    playlistSubmenu = null;
  }

  function closePlaylistSubmenu() {
    playlistSubmenu = null;
  }

  function getPlaylistSubmenuPosition(targetPlaylistsCount: number) {
    if (!contextMenu) return { x: 8, y: 8 };

    const estimatedHeight = Math.min(
      window.innerHeight - 16,
      MENU_PADDING * 2 + targetPlaylistsCount * MENU_ITEM_HEIGHT
    );
    const preferredX = contextMenu.x + MENU_WIDTH + MENU_GAP;
    const x = preferredX + SUBMENU_WIDTH <= window.innerWidth - 8
      ? preferredX
      : Math.max(8, contextMenu.x - SUBMENU_WIDTH - MENU_GAP);
    const y = Math.min(
      Math.max(8, contextMenu.y + MENU_PADDING + MENU_ITEM_HEIGHT),
      window.innerHeight - estimatedHeight - 8
    );

    return { x, y };
  }

  function openPlaylistSubmenuForAdd(paths: string[], sourcePlaylistId: string) {
    const targetPlaylists = player.playlists.filter((playlist) => playlist.id !== sourcePlaylistId);
    if (targetPlaylists.length === 0) {
      closePlaylistSubmenu();
      return;
    }

    if (targetPlaylists.length === 1) {
      confirmAddTracksToPlaylist(targetPlaylists[0].id, paths, sourcePlaylistId);
      return;
    }

    playlistSubmenu = {
      ...getPlaylistSubmenuPosition(targetPlaylists.length),
      paths,
      sourcePlaylistId,
      targetPlaylists: targetPlaylists.map(({ id, name }) => ({ id, name })),
    };
  }

  function submenuItems(targetPlaylists: { id: string; name: string }[], paths: string[], sourcePlaylistId: string): ContextMenuItem[] {
    return targetPlaylists.map((playlist) => ({
      id: `playlist-${playlist.id}`,
      label: playlist.name,
      icon: 'playlist' as const,
      onSelect: () => confirmAddTracksToPlaylist(playlist.id, paths, sourcePlaylistId),
    }));
  }

  function stopWindowClickForTrackMenus(e: MouseEvent) {
    const target = e.target;
    if (target instanceof HTMLElement && target.closest('.context-menu')) return;
    closeContextMenu();
  }

  function stopWindowContextMenuForTrackMenus(e: MouseEvent) {
    const target = e.target;
    if (target instanceof HTMLElement && target.closest('.context-menu')) return;
    closeContextMenu();
  }

  function handleTrackMenuWindowKeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape') return;
    if (!contextMenu && !playlistSubmenu) return;
    e.preventDefault();
    if (playlistSubmenu) {
      closePlaylistSubmenu();
      return;
    }
    closeContextMenu();
  }

  function handleTrackMenuWindowKeydownProxy(e: KeyboardEvent) {
    handleTrackMenuWindowKeydown(e);
  }

  function playlistSubmenuItems(): ContextMenuItem[] {
    if (!playlistSubmenu) return [];
    return submenuItems(
      playlistSubmenu.targetPlaylists,
      playlistSubmenu.paths,
      playlistSubmenu.sourcePlaylistId
    );
  }

  function onPlaylistSubmenuClose() {
    closePlaylistSubmenu();
  }

  function onContextMenuClose() {
    closeContextMenu();
  }

  function stopMenuEventPropagation(e: Event) {
    e.stopPropagation();
  }

  function onTrackMenuPointerEnter() {
    playlistSubmenu = null;
  }

  function onTrackMenuPointerLeave() {
  }

  function onSubmenuPointerEnter() {
  }

  function onSubmenuPointerLeave() {
  }

  function openTrackContextMenu(e: MouseEvent, item: ListedTrack, index: number) {
    e.preventDefault();
    // If right-clicking an unselected track, focus only that track.
    if (!selectedPaths.has(item.track.path)) {
      selectedPaths = new Set();
      selectionAnchor = index;
    }
    const position = openContextMenuFromEvent(e, { width: 220, height: 264 });
    contextMenu = { item, ...position };
  }

  function revealTracksOnDisk(tracks: MusicFile[]) {
    const paths = [
      ...new Set(
        tracks
          .map((track) => exportAudioPathForTrack(track, track.path))
          .filter((path): path is string => !!path)
      ),
    ];
    if (paths.length === 0) return;

    void revealItemInDir(paths.length === 1 ? paths[0] : paths).catch((err) => {
      console.error('Failed to reveal track on disk:', err);
      showDragToast('Could not find track on disk');
    });
  }

  function addTracksToPlaylist(paths: string[], sourcePlaylistId: string) {
    openPlaylistSubmenuForAdd(paths, sourcePlaylistId);
  }

  function confirmAddTracksToPlaylist(targetId: string, paths: string[], sourcePlaylistId: string) {
    const added = player.copyTracksToPlaylist(paths, targetId, sourcePlaylistId);
    if (added > 0) {
      const targetName = player.playlists.find((playlist) => playlist.id === targetId)?.name ?? 'playlist';
      showDragToast(`Added ${added} track${added !== 1 ? 's' : ''} to ${targetName}`);
    }
    closeContextMenu();
  }

  function handleAddToPlaylistDialogKeydown(e: KeyboardEvent) {
    handleTrackMenuWindowKeydown(e);
  }

  function pathsForDrag(item: ListedTrack): string[] {
    if (selectedPaths.size > 0 && selectedPaths.has(item.track.path)) {
      return [...selectedPaths];
    }
    return [item.track.path];
  }

  function onTrackPointerDown(e: PointerEvent, item: ListedTrack) {
    if (!supportsPlaylistReorder(player.activePlaylistId)) return;
    if (e.button !== 0) return;

    const target = e.target as HTMLElement;
    if (target.closest('.like-btn') || target.closest('.sort-btn') || target.closest('.col-resizer')) {
      return;
    }

    trackDrag = {
      paths: pathsForDrag(item),
      sourcePlaylistId: player.activePlaylistId!,
      isCopy: e.ctrlKey || e.metaKey,
      startX: e.clientX,
      startY: e.clientY,
      active: false,
      dropIndex: null,
      dropPlaylistId: null,
      fileExportStarted: false,
    };

    dragCaptureEl = e.currentTarget as HTMLElement;
    dragPointerId = e.pointerId;
    dragCaptureEl.setPointerCapture(e.pointerId);
    window.addEventListener('pointermove', onTrackPointerMove);
    window.addEventListener('pointerup', onTrackPointerUp);
    window.addEventListener('pointercancel', onTrackPointerUp);
    window.addEventListener('blur', onTrackPointerCancel);
    document.addEventListener('visibilitychange', onTrackDragVisibility);
  }

  function cleanupTrackPointerDrag(resetUi = true) {
    window.removeEventListener('pointermove', onTrackPointerMove);
    window.removeEventListener('pointerup', onTrackPointerUp);
    window.removeEventListener('pointercancel', onTrackPointerUp);
    window.removeEventListener('blur', onTrackPointerCancel);
    document.removeEventListener('visibilitychange', onTrackDragVisibility);

    if (dragCaptureEl && dragPointerId !== null) {
      try {
        if (dragCaptureEl.hasPointerCapture(dragPointerId)) {
          dragCaptureEl.releasePointerCapture(dragPointerId);
        }
      } catch {
        /* pointer may already be released */
      }
    }

    dragCaptureEl = null;
    dragPointerId = null;
    trackDrag = null;
    if (resetUi) resetTrackDrag();
  }

  function onTrackPointerCancel() {
    cleanupTrackPointerDrag();
  }

  function onTrackDragVisibility() {
    if (document.visibilityState === 'hidden') {
      onTrackPointerCancel();
    }
  }

  function trackByPath(path: string): MusicFile | undefined {
    return displayedTracks.find((item) => item.track.path === path)?.track;
  }

  function isNearViewportEdge(clientX: number, clientY: number): boolean {
    return (
      clientX <= FILE_EXPORT_EDGE_MARGIN ||
      clientY <= FILE_EXPORT_EDGE_MARGIN ||
      clientX >= window.innerWidth - FILE_EXPORT_EDGE_MARGIN ||
      clientY >= window.innerHeight - FILE_EXPORT_EDGE_MARGIN
    );
  }

  function isOutsideViewport(clientX: number, clientY: number): boolean {
    return (
      clientX <= 0 ||
      clientY <= 0 ||
      clientX >= window.innerWidth ||
      clientY >= window.innerHeight
    );
  }

  /** Native file drag only when leaving the window (or its outer edge), not when moving inside. */
  function shouldStartNativeFileDrag(el: Element | null, clientX: number, clientY: number): boolean {
    if (el?.closest('[data-playlist-id]')) return false;
    if (isOutsideViewport(clientX, clientY)) return true;
    if (!isNearViewportEdge(clientX, clientY)) return false;
    return !el?.closest('.track-row');
  }

  function onTrackPointerMove(e: PointerEvent) {
    if (!trackDrag) return;

    const dx = e.clientX - trackDrag.startX;
    const dy = e.clientY - trackDrag.startY;
    if (!trackDrag.active && Math.hypot(dx, dy) < DRAG_THRESHOLD) return;

    if (!trackDrag.active) {
      setTrackDragActive(true, e.ctrlKey || e.metaKey);
    }

    trackDrag.active = true;
    trackDrag.isCopy = e.ctrlKey || e.metaKey;
    setTrackDragActive(true, trackDrag.isCopy);

    const el = document.elementFromPoint(e.clientX, e.clientY);

    if (!trackDrag.fileExportStarted && shouldStartNativeFileDrag(el, e.clientX, e.clientY)) {
      trackDrag.fileExportStarted = true;
      const { paths, sourcePlaylistId, isCopy } = trackDrag;
      const audioPaths = audioPathsForDrag(paths, trackByPath);
      if (audioPaths.length > 0) {
        const iconPath = trackByPath(paths[0])?.cover_path ?? null;
        beginExportTrackDragUi(paths, isCopy);
        cleanupTrackPointerDrag(false);
        void startFileDrag(audioPaths, {
          iconPath,
          trackSession: { paths, sourcePlaylistId, isCopy },
        }).catch((err) => {
          console.error('Failed to start file drag:', err);
        });
      } else {
        cleanupTrackPointerDrag();
      }
      return;
    }
    const playlistId = el?.closest('[data-playlist-id]')?.getAttribute('data-playlist-id') ?? null;
    const validPlaylistTarget =
      playlistId &&
      playlistId !== trackDrag.sourcePlaylistId &&
      isEditablePlaylist(playlistId);

    if (validPlaylistTarget) {
      trackDrag.dropPlaylistId = playlistId;
      trackDrag.dropIndex = null;
      setTrackDragCopyTarget(playlistId);
    } else if (canReorder) {
      const row = el?.closest('[data-track-index]');
      if (row) {
        const idx = Number(row.getAttribute('data-track-index'));
        const rect = row.getBoundingClientRect();
        const before = e.clientY < rect.top + rect.height / 2;
        trackDrag.dropIndex = before ? idx : idx + 1;
      } else {
        trackDrag.dropIndex = displayedTracks.length;
      }
      trackDrag.dropPlaylistId = null;
      setTrackDragCopyTarget(null);
    } else {
      trackDrag.dropPlaylistId = null;
      trackDrag.dropIndex = null;
      setTrackDragCopyTarget(null);
    }

    trackDrag = { ...trackDrag };
  }

  function applyDisplayedReorder(paths: string[], insertIndex: number, playlistId: string) {
    const items = [...displayedTracks];
    const movingSet = new Set(paths);
    const moving = items.filter((item) => movingSet.has(item.track.path));
    if (moving.length === 0) return;

    const remaining = items.filter((item) => !movingSet.has(item.track.path));
    const insertAt = Math.max(0, Math.min(insertIndex, remaining.length));
    const newOrder = [
      ...remaining.slice(0, insertAt),
      ...moving,
      ...remaining.slice(insertAt),
    ];

    if (playlistId === VIRTUAL_LIKED_ID || playlistId === VIRTUAL_ALL_ID) {
      player.reorderTracksInView(playlistId, paths, insertAt);
    } else {
      player.setPlaylistTrackOrder(playlistId, newOrder.map((item) => item.track));
    }

    if (sortColumn !== null) {
      sortColumn = null;
      sortDirection = 'asc';
      persistSort();
    }
  }

  function onTrackPointerUp() {
    const snapshot = trackDrag;
    const wasActive = snapshot?.active ?? false;

    cleanupTrackPointerDrag();

    if (!wasActive || !snapshot) return;

    suppressTrackClick = true;
    const { paths, sourcePlaylistId, isCopy, dropIndex, dropPlaylistId } = snapshot;

    if (
      dropPlaylistId &&
      isEditablePlaylist(dropPlaylistId) &&
      dropPlaylistId !== sourcePlaylistId
    ) {
      const target = player.playlists.find((p) => p.id === dropPlaylistId)?.name ?? 'playlist';
      if (isCopy) {
        const added = player.copyTracksToPlaylist(paths, dropPlaylistId, sourcePlaylistId);
        if (added > 0) {
          showDragToast(`Copied ${added} track${added !== 1 ? 's' : ''} to ${target}`);
        }
      } else {
        const moved = player.moveTracksToPlaylist(paths, dropPlaylistId, sourcePlaylistId);
        if (moved > 0) {
          showDragToast(`Moved ${moved} track${moved !== 1 ? 's' : ''} to ${target}`);
        }
      }
    } else if (!isCopy && canReorder && dropIndex !== null) {
      applyDisplayedReorder(paths, dropIndex, sourcePlaylistId);
    }
  }

  function handleTrackClick(item: ListedTrack, index: number, e: MouseEvent) {
    if (suppressTrackClick) {
      suppressTrackClick = false;
      return;
    }

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

<section
  class="track-panel"
  class:external-drop-target={
    externalDrop.active && externalDrop.zone === 'tracks'
  }
  data-track-drop-zone
>
  <div class="track-list">
    {#if !player.activePlaylistId}
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

        <div
          class="track-rows"
          class:track-drag-active={trackDrag?.active || trackDragUi.isExportSession}
          bind:this={rowsEl}
          onscroll={handleRowsScroll}
        >
        <div style="height: {visibleRange.top}px" aria-hidden="true"></div>
        {#each visibleTracks as item, localIndex (item.track.path)}
          {@const i = visibleRange.start + localIndex}
          {@const track = item.track}
          {@const isActive = track.path === player.currentFile}
          {@const isSelected = selectedPaths.has(track.path)}
          {@const isDraggingRow =
            (trackDrag?.active && trackDrag.paths.includes(track.path)) ||
            (trackDragUi.isExportSession && trackDragUi.draggingPaths.includes(track.path))}
          <button
            class="track-row"
            class:active={isActive}
            class:playing={isActive && player.isPlaying}
            class:paused={isActive && player.isPaused && !player.isPlaying}
            class:selected={isSelected}
            class:dragging={isDraggingRow}
            class:drop-before={
              (trackDrag?.active && !trackDrag.isCopy && trackDrag.dropIndex === i) ||
              (trackDragUi.isExportSession && !trackDragUi.isCopyDrag && trackDragUi.dropIndex === i)
            }
            class:drop-after={
              (trackDrag?.active && !trackDrag.isCopy && trackDrag.dropIndex === i + 1) ||
              (trackDragUi.isExportSession && !trackDragUi.isCopyDrag && trackDragUi.dropIndex === i + 1)
            }
            data-track-index={i}
            style="grid-template-columns: {gridTemplate}"
            onclick={(e) => handleTrackClick(item, i, e)}
            onpointerdown={(e) => onTrackPointerDown(e, item)}
            oncontextmenu={(e) => openTrackContextMenu(e, item, i)}
            title={`${trackDisplayTitle(track)} — ${trackDisplayArtist(track)}`}
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
                    <span class="track-artist">{trackDisplayArtist(track)}</span>
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
        <div style="height: {visibleRange.bottom}px" aria-hidden="true"></div>
        </div>
      </div>
    {/if}
  </div>
</section>

<svelte:window
  onclick={stopWindowClickForTrackMenus}
  oncontextmenu={stopWindowContextMenuForTrackMenus}
  onkeydown={handleTrackMenuWindowKeydownProxy}
/>

<div class="track-menu-layer">
  <div role="presentation" onmouseenter={onTrackMenuPointerEnter} onmouseleave={onTrackMenuPointerLeave} onclick={stopMenuEventPropagation} onkeydown={handleAddToPlaylistDialogKeydown} oncontextmenu={stopMenuEventPropagation}>
    <ContextMenu
      open={contextMenu !== null}
      x={contextMenu?.x ?? 0}
      y={contextMenu?.y ?? 0}
      items={trackMenuItems}
      onclose={onContextMenuClose}
    />
  </div>

  <div role="presentation" onmouseenter={onSubmenuPointerEnter} onmouseleave={onSubmenuPointerLeave} onclick={stopMenuEventPropagation} onkeydown={handleAddToPlaylistDialogKeydown} oncontextmenu={stopMenuEventPropagation}>
    <ContextMenu
      open={playlistSubmenu !== null}
      x={playlistSubmenu?.x ?? 0}
      y={playlistSubmenu?.y ?? 0}
      items={playlistSubmenuItems()}
      onclose={onPlaylistSubmenuClose}
    />
  </div>
</div>

{#if dragToast}
  <div class="track-drag-toast" role="status">{dragToast}</div>
{/if}

<style>
  @import './TrackList.css';
</style>