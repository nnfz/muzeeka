<script lang="ts">
  import { onMount } from "svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import {
    getPlayerStore,
    isEditablePlaylist,
    sameTrackPath,
    supportsPlaylistReorder,
    trackDisplayArtist,
    trackDisplayTitle,
    VIRTUAL_ALL_ID,
    VIRTUAL_LIKED_ID,
    type MusicFile,
  } from "$lib/stores/player.svelte";
  import {
    beginExportTrackDragUi,
    endExportTrackDragUi,
    resetTrackDrag,
    setTrackDragActive,
    setTrackDragCopyTarget,
    trackDrag as trackDragUi,
  } from "$lib/stores/trackDrag.svelte";
  import { externalDrop } from "$lib/stores/externalDrop.svelte";
  import {
    openContextMenuFromEvent,
    type ContextMenuItem,
  } from "$lib/contextMenu";
  import { open } from "@tauri-apps/plugin-dialog";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import { invoke } from "@tauri-apps/api/core";
  import { audioPathsForDrag, startFileDrag } from "$lib/fileDrag";
  import { exportAudioPathForTrack } from "$lib/trackPaths";
  import { reorderItemsAtBoundary } from "$lib/trackOrder";
  import { getCoverSrc } from "$lib/coverCache";
  import { COVER_PLACEHOLDER_SRC } from "$lib/coverPlaceholder";
  import { dragFloatHide } from "$lib/dragFloat";
  import TrackCover from "./TrackCover.svelte";

  type ColumnId = "index" | "title" | "album" | "duration";
  type SortDirection = "asc" | "desc";

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

  const COLUMN_ORDER: ColumnId[] = ["index", "title", "album", "duration"];
  const COL_GAP = 6;
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
  const STORAGE_COLUMN_LAYOUT_KEY = "muzeeka:track-table:column-layout";
  const STORAGE_SORT_KEY = "muzeeka:track-table:sort";

  const player = getPlayerStore();

  const DRAG_THRESHOLD = 4;
  /**
   * Hand off to OS file-drag after the cursor is fully outside this long.
   * Short enough to feel like a normal "pull out", long enough to not fire
   * on a 1px overshoot while reordering.
   */
  const FILE_EXPORT_OUTSIDE_MS = 180;

  let rowsEl = $state<HTMLDivElement | null>(null);
  let gridWidth = $state(0);
  let rowsViewportHeight = $state(0);
  let rowsScrollTop = $state(0);
  let isNarrow = $state(false);
  let resizingPair = $state<{ left: ColumnId; right: ColumnId } | null>(null);

  const ROW_HEIGHT = 52;
  /** Visual gap opened between rows while reordering (px). */
  const DROP_GAP_PX = 44;
  /** Matches `.track-list-rows` vertical padding — used for drop-index math. */
  const LIST_PAD_Y = 8;
  /**
   * Hysteresis (px) so the open gap doesn't flip dropIndex when the pointer
   * sits on a boundary — that feedback loop is the "jitter while wiggling".
   */
  const DROP_INDEX_HYSTERESIS = 14;
  const VIRTUAL_OVERSCAN = 10;

  function isColumnId(value: unknown): value is ColumnId {
    return (
      value === "index" ||
      value === "title" ||
      value === "album" ||
      value === "duration"
    );
  }

  function clamp(value: number, min: number, max: number): number {
    return Math.max(min, Math.min(max, value));
  }

  function loadColumnLayout(): ColumnLayout {
    try {
      const raw = localStorage.getItem(STORAGE_COLUMN_LAYOUT_KEY);
      if (!raw) return { ...DEFAULT_LAYOUT, index: FIXED_INDEX_WIDTH };
      const parsed: unknown = JSON.parse(raw);

      if (parsed && typeof parsed === "object") {
        const data = parsed as Record<string, unknown>;

        if (
          typeof data.duration === "number" &&
          typeof data.titleShare === "number"
        ) {
          return {
            index: FIXED_INDEX_WIDTH,
            duration: Math.max(
              MIN_COLUMN_WIDTHS.duration,
              Math.round(data.duration),
            ),
            titleShare: clamp(data.titleShare, 0.05, 0.95),
          };
        }

        if (typeof data.title === "number" && typeof data.album === "number") {
          const middle = data.title + data.album;
          return {
            index: FIXED_INDEX_WIDTH,
            duration:
              typeof data.duration === "number"
                ? Math.max(
                    MIN_COLUMN_WIDTHS.duration,
                    Math.round(data.duration),
                  )
                : DEFAULT_LAYOUT.duration,
            titleShare:
              middle > 0
                ? clamp(data.title / middle, 0.05, 0.95)
                : DEFAULT_LAYOUT.titleShare,
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
      if (!raw) return { column: null, direction: "asc" };
      const parsed: unknown = JSON.parse(raw);
      if (
        parsed &&
        typeof parsed === "object" &&
        "column" in parsed &&
        "direction" in parsed
      ) {
        const column = (parsed as { column: unknown }).column;
        const direction = (parsed as { direction: unknown }).direction;
        if (
          (column === null || isColumnId(column)) &&
          (direction === "asc" || direction === "desc")
        ) {
          return { column, direction };
        }
      }
    } catch {
      /* ignore */
    }
    return { column: null, direction: "asc" };
  }

  const initialSort = loadSort();

  let columnLayout = $state<ColumnLayout>(loadColumnLayout());
  let sortColumn = $state<ColumnId | null>(initialSort.column);
  let sortDirection = $state<SortDirection>(initialSort.direction);
  // --- Multi-selection state ---
  let selectedPaths = $state<Set<string>>(new Set());
  // Anchor index (in displayedTracks) for range selection.
  let selectionAnchor = $state<number | null>(null);

  let contextMenu = $state<{ item: ListedTrack; x: number; y: number } | null>(
    null,
  );
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
    /** Live pointer for floating cover preview. */
    pointerX: number;
    pointerY: number;
    active: boolean;
    dropIndex: number | null;
    dropPlaylistId: string | null;
    fileExportStarted: boolean;
    /** performance.now() when pointer first went outside the window; null if inside. */
    outsideSince: number | null;
    /** Alt held — user is forcing OS file export. */
    exportMode: boolean;
  }

  let trackDrag = $state<TrackDragState | null>(null);
  let dragPointerId = $state<number | null>(null);
  /**
   * After an active drag, the browser may still fire a synthetic `click`.
   * Swallow only that ghost click — auto-clear so the next real click plays.
   */
  let suppressTrackClick = false;
  let suppressTrackClickTimer: ReturnType<typeof setTimeout> | null = null;
  let dragPreviewCoverFailed = $state(false);
  /** Degrees — slight tilt that follows horizontal mouse velocity. */
  let dragFloatRotate = $state(-2.5);
  let dragFloatSample = { x: 0, y: 0, t: 0 };

  let canReorder = $derived(supportsPlaylistReorder(player.activePlaylistId));

  /** True while reordering inside the list (not copy-to-playlist / file export). */
  let reorderGapActive = $derived(
    !!trackDrag?.active &&
      !trackDrag.isCopy &&
      trackDrag.dropPlaylistId == null &&
      trackDrag.dropIndex != null &&
      canReorder,
  );

  let reorderDropIndex = $derived(
    reorderGapActive ? (trackDrag?.dropIndex ?? null) : null,
  );

  function isOutsideViewport(clientX: number, clientY: number): boolean {
    return (
      clientX <= 0 ||
      clientY <= 0 ||
      clientX >= window.innerWidth ||
      clientY >= window.innerHeight
    );
  }

  let dragPreview = $derived.by(() => {
    const drag = trackDrag;
    if (!drag?.active || drag.paths.length === 0) return null;
    const leadPath = drag.paths[0];
    const lead = displayedTracks.find(
      (item) => item.track.path === leadPath,
    )?.track;
    const coverPath = lead?.cover_path ?? null;
    const resolved = getCoverSrc(coverPath);
    const src =
      !dragPreviewCoverFailed && resolved ? resolved : COVER_PLACEHOLDER_SRC;
    return {
      x: drag.pointerX,
      y: drag.pointerY,
      rotate: dragFloatRotate,
      count: drag.paths.length,
      title: lead ? trackDisplayTitle(lead) : "Track",
      artist: lead ? trackDisplayArtist(lead) : "",
      coverSrc: src,
      isCopy: drag.isCopy,
      exportMode: drag.exportMode,
    };
  });

  /** Wipe any leftover drag state that can freeze clicks (run on mount + Escape). */
  function hardResetDragUi() {
    detachLiveDragListeners();
    document.body.classList.remove("track-reorder-dragging");
    trackDrag = null;
    dragFloatRotate = -2.5;
    suppressTrackClick = false;
    if (suppressTrackClickTimer) {
      clearTimeout(suppressTrackClickTimer);
      suppressTrackClickTimer = null;
    }
    resetTrackDrag();
    // Best-effort: hide/destroy any leftover outside-window float from earlier experiments.
    void dragFloatHide();
  }

  // On mount / unmount — clear any leftover drag float / body class from a
  // previous crash. Must NOT be a reactive $effect (that would reset mid-drag).
  onMount(() => {
    hardResetDragUi();
    // Kill any orphan always-on-top drag-float window from earlier builds.
    void dragFloatHide();
    return () => hardResetDragUi();
  });

  let trackMenuItems = $derived.by((): ContextMenuItem[] => {
    const target = contextMenu?.item;
    if (!target) return [];

    const items: ContextMenuItem[] = [];

    // Multi-selection: determine which paths the menu applies to.
    const affectedPaths =
      selectedPaths.size > 0 && selectedPaths.has(target.track.path)
        ? [...selectedPaths]
        : [target.track.path];
    const affectedTracks = affectedPaths
      .map(
        (path) =>
          displayedTracks.find((item) => item.track.path === path)?.track,
      )
      .filter((track): track is MusicFile => !!track);
    const multi = affectedPaths.length > 1;

    items.push({
      id: "find-on-disk",
      label: multi
        ? `Найти ${affectedTracks.length} на диске`
        : "Найти на диске",
      icon: "folder",
      disabled: affectedTracks.length === 0,
      onSelect: () => revealTracksOnDisk(affectedTracks),
    });

    // Lyrics actions — single track only (lyrics cache key is per-title/artist).
    if (!multi) {
      items.push({
        id: "import-ttml",
        label: "Импорт TTML",
        onSelect: () => void importTtmlForTrack(target.track),
      });
      items.push({
        id: "refetch-lyrics",
        label: "Найти текст",
        onSelect: () => void refetchLyricsForTrack(target.track),
      });
      items.push({
        id: "clear-lyrics",
        label: "Убрать текст",
        onSelect: () => void clearLyricsForTrack(target.track),
      });
    }

    const availableTargetPlaylists = player.playlists.filter(
      (playlist) => playlist.id !== target.playlistId,
    );
    items.push({
      id: "add-to-playlist",
      label: multi
        ? `Добавить ${affectedPaths.length} в плейлист ›`
        : "Добавить в плейлист ›",
      icon: "playlist",
      disabled: availableTargetPlaylists.length === 0,
      closeOnSelect: false,
      onSelect: () => addTracksToPlaylist(affectedPaths, target.playlistId),
    });

    // Like / Unlike
    const allLiked = affectedPaths.every((p) => player.isLiked(p));
    items.push({
      id: "like",
      label: allLiked
        ? multi
          ? `Remove ${affectedPaths.length} from Liked`
          : "Remove from Liked"
        : multi
          ? `Add ${affectedPaths.length} to Liked`
          : "Add to Liked",
      icon: "heart",
      onSelect: () =>
        affectedPaths.forEach((p) => {
          if (allLiked ? player.isLiked(p) : !player.isLiked(p))
            player.toggleLike(p);
        }),
    });

    // Delete — only for real playlists; only if all selected are from the same playlist
    const pid = target.playlistId;
    const isRealPlaylist =
      pid && pid !== VIRTUAL_ALL_ID && pid !== VIRTUAL_LIKED_ID;
    const allSamePlaylist =
      isRealPlaylist &&
      affectedPaths.every((p) => {
        const lt = listedTracks.find((l) => l.track.path === p);
        return lt && lt.playlistId === pid;
      });
    if (allSamePlaylist) {
      items.push({
        id: "delete",
        label: multi ? `Delete ${affectedPaths.length} tracks` : "Delete",
        icon: "delete",
        danger: true,
        onSelect: () =>
          affectedPaths.forEach((p) => player.removeTrack(p, pid)),
      });
    }
    return items;
  });

  let listedTracks = $derived.by((): ListedTrack[] => {
    if (!player.activePlaylistId) return [];

    return player.tracks.map((track) => ({
      track,
      playlistId: player.activePlaylistId!,
      playlistName: player.activePlaylist?.name ?? "",
    }));
  });

  let visibleColumns = $derived(
    isNarrow ? COLUMN_ORDER.filter((id) => id !== "album") : COLUMN_ORDER,
  );

  function availableWidth(columns: ColumnId[]): number {
    const gaps = (columns.length - 1) * COL_GAP;
    return Math.max(0, gridWidth - gaps);
  }

  function minMiddleWidth(columns: ColumnId[]): number {
    return columns
      .filter((id) => id !== "index" && id !== "duration")
      .reduce((sum, id) => sum + MIN_COLUMN_WIDTHS[id], 0);
  }

  function computeEffectiveWidths(
    columns: ColumnId[],
    layout: ColumnLayout,
  ): Record<ColumnId, number> {
    const available = availableWidth(columns);
    const middleMin = minMiddleWidth(columns);

    const index = FIXED_INDEX_WIDTH;
    let duration = layout.duration;

    const maxDuration = available - index - middleMin;
    duration = clamp(
      duration,
      MIN_COLUMN_WIDTHS.duration,
      Math.max(MIN_COLUMN_WIDTHS.duration, maxDuration),
    );

    const middle = available - index - duration;

    let title = middle;
    let album = 0;

    if (columns.includes("album")) {
      title = Math.max(
        MIN_COLUMN_WIDTHS.title,
        Math.round(middle * layout.titleShare),
      );
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

  let effectiveWidths = $derived(
    computeEffectiveWidths(visibleColumns, columnLayout),
  );

  let gridTemplate = $derived(
    visibleColumns.map((id) => `${effectiveWidths[id]}px`).join(" "),
  );

  let listedIndexByPath = $derived.by(() => {
    const map = new Map<string, number>();
    listedTracks.forEach((item, index) => map.set(item.track.path, index));
    return map;
  });

  let displayedTracks = $derived.by(() => {
    const items = [...listedTracks];
    if (!sortColumn) return items;

    const dir = sortDirection === "asc" ? 1 : -1;
    return items.sort((a, b) => compareTracks(a, b, sortColumn!) * dir);
  });

  // Keep next/prev + gapless in sync with the table's visible (sorted) order.
  $effect(() => {
    const playlistId = player.activePlaylistId;
    if (!playlistId) {
      player.setViewPlayOrder(null, null);
      return;
    }
    if (!sortColumn) {
      // Viewing this playlist unsorted — restore natural order if it is playing.
      player.setViewPlayOrder(playlistId, null);
      return;
    }
    const paths = displayedTracks.map((item) => item.track.path);
    player.setViewPlayOrder(playlistId, paths);
  });

  const HEADER_HEIGHT = 36;

  let visibleRange = $derived.by(() => {
    const total = displayedTracks.length;
    if (total === 0) return { start: 0, end: 0, top: 0, bottom: 0 };

    const scrollWithinRows = Math.max(0, rowsScrollTop - HEADER_HEIGHT);
    const start = Math.max(
      0,
      Math.floor(scrollWithinRows / ROW_HEIGHT) - VIRTUAL_OVERSCAN,
    );
    const visibleCount =
      Math.ceil(rowsViewportHeight / ROW_HEIGHT) + VIRTUAL_OVERSCAN * 2;
    const end = Math.min(
      total,
      start + Math.max(visibleCount, VIRTUAL_OVERSCAN * 2),
    );

    return {
      start,
      end,
      top: start * ROW_HEIGHT,
      bottom: Math.max(0, (total - end) * ROW_HEIGHT),
    };
  });

  let visibleTracks = $derived(
    displayedTracks.slice(visibleRange.start, visibleRange.end),
  );

  $effect(() => {
    const el = rowsEl;
    if (!el) return;

    const updateMetrics = () => {
      gridWidth = el.clientWidth - 28;
      isNarrow = el.clientWidth < 560;
      rowsViewportHeight = el.clientHeight;
      rowsScrollTop = el.scrollTop;
    };

    updateMetrics();
    const observer = new ResizeObserver(updateMetrics);
    observer.observe(el);
    return () => observer.disconnect();
  });

  function handleRowsScroll(e: Event) {
    rowsScrollTop = (e.currentTarget as HTMLDivElement).scrollTop;
  }

  function persistColumnLayout() {
    localStorage.setItem(
      STORAGE_COLUMN_LAYOUT_KEY,
      JSON.stringify(columnLayout),
    );
  }

  function persistSort() {
    localStorage.setItem(
      STORAGE_SORT_KEY,
      JSON.stringify({ column: sortColumn, direction: sortDirection }),
    );
  }

  function compareTracks(
    a: ListedTrack,
    b: ListedTrack,
    column: ColumnId,
  ): number {
    switch (column) {
      case "index": {
        const ai = listedIndexByPath.get(a.track.path) ?? -1;
        const bi = listedIndexByPath.get(b.track.path) ?? -1;
        return ai - bi;
      }
      case "title":
        return trackDisplayTitle(a.track).localeCompare(
          trackDisplayTitle(b.track),
          undefined,
          {
            sensitivity: "base",
          },
        );
      case "album":
        return (a.track.album ?? "").localeCompare(
          b.track.album ?? "",
          undefined,
          {
            sensitivity: "base",
          },
        );
      case "duration":
        return (a.track.duration_secs ?? -1) - (b.track.duration_secs ?? -1);
    }
  }

  function toggleSort(column: ColumnId) {
    if (sortColumn !== column) {
      sortColumn = column;
      sortDirection = "asc";
    } else if (sortDirection === "asc") {
      sortDirection = "desc";
    } else {
      sortColumn = null;
      sortDirection = "asc";
    }
    persistSort();
  }

  function startColumnResize(left: ColumnId, right: ColumnId, e: PointerEvent) {
    if (left === "index") return;
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

      if (left === "title" && right === "album") {
        const middle = available - startLayout.index - startLayout.duration;
        const startTitle = middle * startLayout.titleShare;
        const nextTitle = clamp(
          startTitle + delta,
          MIN_COLUMN_WIDTHS.title,
          middle - MIN_COLUMN_WIDTHS.album,
        );
        columnLayout = {
          ...startLayout,
          titleShare: nextTitle / middle,
        };
        return;
      }

      if (right === "duration") {
        const maxDuration = available - startLayout.index - middleMin;
        columnLayout = {
          ...startLayout,
          duration: clamp(
            startLayout.duration - delta,
            MIN_COLUMN_WIDTHS.duration,
            maxDuration,
          ),
        };
      }
    }

    function onUp() {
      resizingPair = null;
      persistColumnLayout();
      document.body.classList.remove("track-table-resizing");
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    }

    document.body.classList.add("track-table-resizing");
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  function resetColumnPair(left: ColumnId, right: ColumnId) {
    if (left === "index") return;
    if (left === "title" && right === "album") {
      columnLayout = { ...columnLayout, titleShare: DEFAULT_LAYOUT.titleShare };
    } else if (right === "duration") {
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
        { name: "TTML lyrics", extensions: ["ttml", "xml"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    const path = typeof selected === "string" ? selected : null;
    if (!path) return;

    try {
      await invoke("lyrics_import_ttml", {
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
      showDragToast("TTML импортирован");
    } catch (e) {
      console.error("Failed to import TTML:", e);
      showDragToast(
        e instanceof Error ? e.message : "Не удалось импортировать TTML",
        3200,
      );
    }
  }

  async function clearLyricsForTrack(track: MusicFile) {
    try {
      await invoke("lyrics_clear", {
        title: trackDisplayTitle(track),
        artist: trackDisplayArtist(track),
        album: track.album ?? null,
        durationSecs:
          track.duration_secs != null && track.duration_secs > 0
            ? Math.round(track.duration_secs)
            : null,
        trackPath: track.path,
      });
      showDragToast("Текст убран");
    } catch (e) {
      console.error("Failed to clear lyrics:", e);
      showDragToast(
        e instanceof Error ? e.message : "Не удалось убрать текст",
        3200,
      );
    }
  }

  async function refetchLyricsForTrack(track: MusicFile) {
    showDragToast("Ищем текст…", 8000);
    try {
      const found = await invoke<boolean>("lyrics_refetch", {
        title: trackDisplayTitle(track),
        artist: trackDisplayArtist(track),
        album: track.album ?? null,
        durationSecs:
          track.duration_secs != null && track.duration_secs > 0
            ? Math.round(track.duration_secs)
            : null,
        trackPath: track.path,
      });
      showDragToast(
        found ? "Текст найден" : "Текст не найден",
        found ? 2400 : 3200,
      );
    } catch (e) {
      console.error("Failed to refetch lyrics:", e);
      showDragToast(
        e instanceof Error ? e.message : "Не удалось найти текст",
        3200,
      );
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
      MENU_PADDING * 2 + targetPlaylistsCount * MENU_ITEM_HEIGHT,
    );
    const preferredX = contextMenu.x + MENU_WIDTH + MENU_GAP;
    const x =
      preferredX + SUBMENU_WIDTH <= window.innerWidth - 8
        ? preferredX
        : Math.max(8, contextMenu.x - SUBMENU_WIDTH - MENU_GAP);
    const y = Math.min(
      Math.max(8, contextMenu.y + MENU_PADDING + MENU_ITEM_HEIGHT),
      window.innerHeight - estimatedHeight - 8,
    );

    return { x, y };
  }

  function openPlaylistSubmenuForAdd(
    paths: string[],
    sourcePlaylistId: string,
  ) {
    const targetPlaylists = player.playlists.filter(
      (playlist) => playlist.id !== sourcePlaylistId,
    );
    if (targetPlaylists.length === 0) {
      closePlaylistSubmenu();
      return;
    }

    if (targetPlaylists.length === 1) {
      confirmAddTracksToPlaylist(
        targetPlaylists[0].id,
        paths,
        sourcePlaylistId,
      );
      return;
    }

    playlistSubmenu = {
      ...getPlaylistSubmenuPosition(targetPlaylists.length),
      paths,
      sourcePlaylistId,
      targetPlaylists: targetPlaylists.map(({ id, name }) => ({ id, name })),
    };
  }

  function submenuItems(
    targetPlaylists: { id: string; name: string }[],
    paths: string[],
    sourcePlaylistId: string,
  ): ContextMenuItem[] {
    return targetPlaylists.map((playlist) => ({
      id: `playlist-${playlist.id}`,
      label: playlist.name,
      icon: "playlist" as const,
      onSelect: () =>
        confirmAddTracksToPlaylist(playlist.id, paths, sourcePlaylistId),
    }));
  }

  function stopWindowClickForTrackMenus(e: MouseEvent) {
    const target = e.target;
    if (target instanceof HTMLElement && target.closest(".context-menu"))
      return;
    closeContextMenu();
  }

  function stopWindowContextMenuForTrackMenus(e: MouseEvent) {
    const target = e.target;
    if (target instanceof HTMLElement && target.closest(".context-menu"))
      return;
    closeContextMenu();
  }

  function isTypingTarget(target: EventTarget | null): boolean {
    return (
      target instanceof HTMLInputElement ||
      target instanceof HTMLTextAreaElement ||
      (target instanceof HTMLElement && target.isContentEditable)
    );
  }

  function selectAllDisplayedTracks() {
    if (displayedTracks.length === 0) {
      selectedPaths = new Set();
      selectionAnchor = null;
      return;
    }
    selectedPaths = new Set(displayedTracks.map((item) => item.track.path));
    selectionAnchor = 0;
  }

  function handleTrackMenuWindowKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      if (trackDrag) {
        e.preventDefault();
        hardResetDragUi();
        return;
      }
      if (contextMenu || playlistSubmenu) {
        e.preventDefault();
        if (playlistSubmenu) {
          closePlaylistSubmenu();
          return;
        }
        closeContextMenu();
        return;
      }
      // Escape clears multi-selection when no menu is open.
      if (selectedPaths.size > 0) {
        e.preventDefault();
        selectedPaths = new Set();
        selectionAnchor = null;
      }
      return;
    }

    // Ctrl/Cmd+A — select every track currently shown in the playlist table.
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.code === "KeyA") {
      if (isTypingTarget(e.target)) return;
      e.preventDefault();
      e.stopPropagation();
      selectAllDisplayedTracks();
    }
  }

  function handleTrackMenuWindowKeydownProxy(e: KeyboardEvent) {
    handleTrackMenuWindowKeydown(e);
  }

  // Drop multi-selection when switching playlists.
  $effect(() => {
    const _playlistId = player.activePlaylistId;
    selectedPaths = new Set();
    selectionAnchor = null;
    void _playlistId;
  });

  function playlistSubmenuItems(): ContextMenuItem[] {
    if (!playlistSubmenu) return [];
    return submenuItems(
      playlistSubmenu.targetPlaylists,
      playlistSubmenu.paths,
      playlistSubmenu.sourcePlaylistId,
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

  function onTrackMenuPointerLeave() {}

  function onSubmenuPointerEnter() {}

  function onSubmenuPointerLeave() {}

  function openTrackContextMenu(
    e: MouseEvent,
    item: ListedTrack,
    index: number,
  ) {
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
          .filter((path): path is string => !!path),
      ),
    ];
    if (paths.length === 0) return;

    void revealItemInDir(paths.length === 1 ? paths[0] : paths).catch((err) => {
      console.error("Failed to reveal track on disk:", err);
      showDragToast("Could not find track on disk");
    });
  }

  function addTracksToPlaylist(paths: string[], sourcePlaylistId: string) {
    openPlaylistSubmenuForAdd(paths, sourcePlaylistId);
  }

  function confirmAddTracksToPlaylist(
    targetId: string,
    paths: string[],
    sourcePlaylistId: string,
  ) {
    const added = player.copyTracksToPlaylist(
      paths,
      targetId,
      sourcePlaylistId,
    );
    if (added > 0) {
      const targetName =
        player.playlists.find((playlist) => playlist.id === targetId)?.name ??
        "playlist";
      showDragToast(
        `Added ${added} track${added !== 1 ? "s" : ""} to ${targetName}`,
      );
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

  function detachLiveDragListeners() {
    window.removeEventListener("pointermove", onTrackPointerMove);
    window.removeEventListener("pointerup", onTrackPointerUp);
    window.removeEventListener("pointercancel", onTrackPointerCancel);
    dragPointerId = null;
  }

  function attachLiveDragListeners(pointerId: number) {
    dragPointerId = pointerId;
    // No setPointerCapture — capture previously froze the UI after leave/cancel.
    window.addEventListener("pointermove", onTrackPointerMove);
    window.addEventListener("pointerup", onTrackPointerUp);
    window.addEventListener("pointercancel", onTrackPointerCancel);
  }

  function cleanupTrackPointerDrag(resetUi = true) {
    detachLiveDragListeners();
    document.body.classList.remove("track-reorder-dragging");
    trackDrag = null;
    dragFloatRotate = -2.5;
    if (resetUi) resetTrackDrag();
    void dragFloatHide();
  }

  function onTrackPointerDown(e: PointerEvent, item: ListedTrack) {
    // Drag is always allowed: reorder (when supported) and/or OS file export.
    if (e.button !== 0) return;
    if (!player.activePlaylistId) return;

    const target = e.target as HTMLElement;
    if (
      target.closest(".like-btn") ||
      target.closest(".sort-btn") ||
      target.closest(".col-resizer")
    ) {
      return;
    }

    if (trackDrag) cleanupTrackPointerDrag();

    dragPreviewCoverFailed = false;
    dragFloatRotate = -2.5;
    dragFloatSample = { x: e.clientX, y: e.clientY, t: performance.now() };
    trackDrag = {
      paths: pathsForDrag(item),
      sourcePlaylistId: player.activePlaylistId!,
      isCopy: e.ctrlKey || e.metaKey,
      startX: e.clientX,
      startY: e.clientY,
      pointerX: e.clientX,
      pointerY: e.clientY,
      active: false,
      dropIndex: null,
      dropPlaylistId: null,
      fileExportStarted: false,
      outsideSince: null,
      exportMode: e.altKey,
    };

    attachLiveDragListeners(e.pointerId);
  }

  function trackByPath(path: string): MusicFile | undefined {
    return displayedTracks.find((item) => item.track.path === path)?.track;
  }

  /**
   * Hand files to the OS (Explorer / Telegram / Discord…).
   * Returns true if export was started (or already in progress).
   */
  function beginOsFileExport(drag: TrackDragState): boolean {
    if (drag.fileExportStarted) return true;
    const audioPaths = audioPathsForDrag(drag.paths, trackByPath);
    if (audioPaths.length === 0) {
      showDragToast("No local file to export");
      return false;
    }
    drag.fileExportStarted = true;
    const iconPath = trackByPath(drag.paths[0])?.cover_path ?? null;
    const { paths, sourcePlaylistId, isCopy } = drag;
    beginExportTrackDragUi(paths, isCopy);
    cleanupTrackPointerDrag(false);
    void startFileDrag(audioPaths, {
      iconPath,
      trackSession: { paths, sourcePlaylistId, isCopy },
    }).catch((err) => {
      console.error("Failed to start file drag:", err);
      showDragToast("Could not start file drag");
      endExportTrackDragUi();
      resetTrackDrag();
    });
    return true;
  }

  /**
   * When to switch from in-app drag → OS file drag:
   * - Alt held (explicit export), once the drag is active
   * - Cursor fully outside the window for FILE_EXPORT_OUTSIDE_MS
   */
  function shouldStartNativeFileDrag(
    drag: TrackDragState,
    clientX: number,
    clientY: number,
    altHeld: boolean,
  ): boolean {
    if (altHeld) return true;

    const outside = isOutsideViewport(clientX, clientY);
    if (!outside) {
      drag.outsideSince = null;
      return false;
    }
    const now = performance.now();
    if (drag.outsideSince == null) {
      drag.outsideSince = now;
      return false;
    }
    return now - drag.outsideSince >= FILE_EXPORT_OUTSIDE_MS;
  }

  /**
   * Insert index from pointer Y using *untransformed* list geometry
   * (scrollTop + fixed row height). Never use getBoundingClientRect of rows —
   * those move when the gap opens and create a flip-flop loop.
   */
  function dropIndexFromPointerY(clientY: number): number | null {
    const el = rowsEl;
    if (!el || displayedTracks.length === 0) return null;

    const rect = el.getBoundingClientRect();
    // Outside the scroll viewport vertically → no reorder target.
    if (clientY < rect.top - 4 || clientY > rect.bottom + 4) return null;

    const yInContent =
      clientY - rect.top + el.scrollTop - LIST_PAD_Y;
    // Map Y onto insert slots [0 .. n] at row midpoints (stable coordinates).
    const raw = yInContent / ROW_HEIGHT;
    return Math.max(
      0,
      Math.min(displayedTracks.length, Math.round(raw)),
    );
  }

  /**
   * Only change dropIndex when the pointer has clearly crossed into a new
   * slot — kills boundary jitter while still feeling responsive.
   */
  function stabilizeDropIndex(
    next: number | null,
    prev: number | null,
    clientY: number,
  ): number | null {
    if (next == null) return null;
    if (prev == null || prev === next) return next;

    const el = rowsEl;
    if (!el) return next;

    const rect = el.getBoundingClientRect();
    const yInContent = clientY - rect.top + el.scrollTop - LIST_PAD_Y;
    // Boundary between prev and next slots (in content px).
    // Slot k is centered at k * ROW_HEIGHT; boundary between k and k+1 at (k+0.5)*H.
    const low = Math.min(prev, next);
    const boundary = (low + 0.5) * ROW_HEIGHT;
    const dist = yInContent - boundary;

    // Must push past boundary by hysteresis before flipping.
    if (next > prev && dist < DROP_INDEX_HYSTERESIS) return prev;
    if (next < prev && dist > -DROP_INDEX_HYSTERESIS) return prev;
    return next;
  }

  function onTrackPointerMove(e: PointerEvent) {
    if (!trackDrag) return;

    const dx = e.clientX - trackDrag.startX;
    const dy = e.clientY - trackDrag.startY;
    if (!trackDrag.active && Math.hypot(dx, dy) < DRAG_THRESHOLD) return;

    if (!trackDrag.active) {
      setTrackDragActive(true, e.ctrlKey || e.metaKey);
      document.body.classList.add("track-reorder-dragging");
    }

    trackDrag.active = true;
    trackDrag.isCopy = e.ctrlKey || e.metaKey;
    trackDrag.exportMode = e.altKey;
    trackDrag.pointerX = e.clientX;
    trackDrag.pointerY = e.clientY;
    setTrackDragActive(true, trackDrag.isCopy);

    // Tilt the float with horizontal velocity (smoothed, clamped).
    {
      const now = performance.now();
      const dt = Math.max(8, now - dragFloatSample.t);
      const vx = (e.clientX - dragFloatSample.x) / dt; // px/ms
      const vy = (e.clientY - dragFloatSample.y) / dt;
      const target = Math.max(-11, Math.min(11, -2.2 + vx * 55 + vy * 8));
      dragFloatRotate = dragFloatRotate * 0.72 + target * 0.28;
      dragFloatSample = { x: e.clientX, y: e.clientY, t: now };
    }

    // Alt+drag → immediate OS file export (explicit "pull song out").
    // Fully outside for ~180ms → same (natural pull to Explorer / messengers).
    if (
      !trackDrag.fileExportStarted &&
      shouldStartNativeFileDrag(
        trackDrag,
        e.clientX,
        e.clientY,
        e.altKey,
      )
    ) {
      beginOsFileExport(trackDrag);
      return;
    }

    const outside = isOutsideViewport(e.clientX, e.clientY);
    if (outside) {
      trackDrag.dropPlaylistId = null;
      trackDrag.dropIndex = null;
      setTrackDragCopyTarget(null);
      trackDrag = { ...trackDrag };
      return;
    }

    const el = document.elementFromPoint(e.clientX, e.clientY);
    const playlistId =
      el?.closest("[data-playlist-id]")?.getAttribute("data-playlist-id") ??
      null;
    const validPlaylistTarget =
      playlistId &&
      playlistId !== trackDrag.sourcePlaylistId &&
      isEditablePlaylist(playlistId);

    if (validPlaylistTarget) {
      trackDrag.dropPlaylistId = playlistId;
      trackDrag.dropIndex = null;
      setTrackDragCopyTarget(playlistId);
    } else if (canReorder) {
      const overList =
        !!el?.closest(".track-rows") ||
        !!el?.closest("[data-track-drop-zone]");
      if (overList) {
        const rawIndex = dropIndexFromPointerY(e.clientY);
        trackDrag.dropIndex = stabilizeDropIndex(
          rawIndex,
          trackDrag.dropIndex,
          e.clientY,
        );
      } else {
        trackDrag.dropIndex = null;
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

  function applyDisplayedReorder(
    paths: string[],
    insertIndex: number,
    playlistId: string,
  ) {
    const items = [...displayedTracks];
    const newOrder = reorderItemsAtBoundary(
      items,
      paths,
      insertIndex,
      (item) => item.track.path,
    );
    if (newOrder.every((item, index) => item === items[index])) return;

    if (playlistId === VIRTUAL_LIKED_ID || playlistId === VIRTUAL_ALL_ID) {
      player.reorderTracksInView(playlistId, paths, insertIndex);
    } else {
      player.setPlaylistTrackOrder(
        playlistId,
        newOrder.map((item) => item.track),
      );
    }

    if (sortColumn !== null) {
      sortColumn = null;
      sortDirection = "asc";
      persistSort();
    }
  }

  function armSuppressTrackClick() {
    // Swallow the ghost click from this pointer gesture only.
    // If the browser never fires that click (common after a real drag),
    // clear the flag quickly so the next intentional click still plays.
    suppressTrackClick = true;
    if (suppressTrackClickTimer) clearTimeout(suppressTrackClickTimer);
    suppressTrackClickTimer = setTimeout(() => {
      suppressTrackClick = false;
      suppressTrackClickTimer = null;
    }, 80);
  }

  function applyTrackDragDrop(snapshot: TrackDragState) {
    const { paths, sourcePlaylistId, isCopy, dropIndex, dropPlaylistId } =
      snapshot;

    if (
      dropPlaylistId &&
      isEditablePlaylist(dropPlaylistId) &&
      dropPlaylistId !== sourcePlaylistId
    ) {
      const target =
        player.playlists.find((p) => p.id === dropPlaylistId)?.name ??
        "playlist";
      if (isCopy) {
        const added = player.copyTracksToPlaylist(
          paths,
          dropPlaylistId,
          sourcePlaylistId,
        );
        if (added > 0) {
          showDragToast(
            `Copied ${added} track${added !== 1 ? "s" : ""} to ${target}`,
          );
        }
      } else {
        const moved = player.moveTracksToPlaylist(
          paths,
          dropPlaylistId,
          sourcePlaylistId,
        );
        if (moved > 0) {
          showDragToast(
            `Moved ${moved} track${moved !== 1 ? "s" : ""} to ${target}`,
          );
        }
      }
    } else if (!isCopy && canReorder && dropIndex !== null) {
      applyDisplayedReorder(paths, dropIndex, sourcePlaylistId);
    }
  }

  function onTrackPointerCancel(e: PointerEvent) {
    if (dragPointerId !== null && e.pointerId !== dragPointerId) return;
    const snapshot = trackDrag;
    // WebView often cancels the pointer when leaving the window — treat that
    // as "user is pulling the file out" and hand off to OS drag.
    if (snapshot?.active && !snapshot.fileExportStarted) {
      if (beginOsFileExport(snapshot)) return;
    }
    cleanupTrackPointerDrag();
  }

  function onTrackPointerUp(e?: PointerEvent) {
    if (
      e &&
      dragPointerId !== null &&
      e.pointerId !== dragPointerId &&
      e.type === "pointerup"
    ) {
      return;
    }

    const snapshot = trackDrag;
    const wasActive = snapshot?.active ?? false;

    // If released while clearly outside, export files instead of in-app drop.
    if (
      wasActive &&
      snapshot &&
      !snapshot.fileExportStarted &&
      isOutsideViewport(snapshot.pointerX, snapshot.pointerY)
    ) {
      if (beginOsFileExport(snapshot)) return;
    }

    cleanupTrackPointerDrag();

    if (!wasActive || !snapshot) return;

    armSuppressTrackClick();
    applyTrackDragDrop(snapshot);
  }

  function handleTrackClick(item: ListedTrack, index: number, e: MouseEvent) {
    if (suppressTrackClick) {
      suppressTrackClick = false;
      if (suppressTrackClickTimer) {
        clearTimeout(suppressTrackClickTimer);
        suppressTrackClickTimer = null;
      }
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
    if (seconds == null || !Number.isFinite(seconds) || seconds <= 0)
      return "—";
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, "0")}`;
  }

  function columnLabel(column: ColumnId): string {
    switch (column) {
      case "index":
        return "#";
      case "title":
        return "Title";
      case "album":
        return "Album";
      case "duration":
        return "Duration";
    }
  }
</script>

<section
  class="track-panel"
  class:external-drop-target={externalDrop.active &&
    externalDrop.zone === "tracks"}
  data-track-drop-zone
>
  <div class="track-list glass">
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
      <div class="track-table" class:is-resizing={resizingPair !== null}>
        <div
          class="track-table-header"
          style="grid-template-columns: {gridTemplate}"
        >
          {#each visibleColumns as column, i (column)}
            <div
              class="header-cell"
              class:col-index={column === "index"}
              class:col-title={column === "title"}
              class:col-album={column === "album"}
              class:col-duration={column === "duration"}
            >
              <button
                type="button"
                class="sort-btn"
                class:sorted={sortColumn === column}
                onclick={() => toggleSort(column)}
                aria-label={`Sort by ${columnLabel(column)}`}
              >
                {#if column === "duration"}
                  <svg
                    width="14"
                    height="14"
                    viewBox="0 0 24 24"
                    fill="currentColor"
                    aria-hidden="true"
                  >
                    <path
                      d="M12 2C6.477 2 2 6.477 2 12s4.477 10 10 10 10-4.477 10-10S17.523 2 12 2zm0 18c-4.411 0-8-3.589-8-8s3.589-8 8-8 8 3.589 8 8-3.589 8-8 8zm.5-13H11v6l5.25 3.15.75-1.23-4.5-2.67V7z"
                    />
                  </svg>
                {:else}
                  <span>{columnLabel(column)}</span>
                {/if}

                {#if sortColumn === column}
                  <span class="sort-indicator" aria-hidden="true">
                    {#if sortDirection === "asc"}
                      <svg
                        width="12"
                        height="12"
                        viewBox="0 0 24 24"
                        fill="currentColor"
                      >
                        <path d="M7 14l5-5 5 5H7z" />
                      </svg>
                    {:else}
                      <svg
                        width="12"
                        height="12"
                        viewBox="0 0 24 24"
                        fill="currentColor"
                      >
                        <path d="M7 10l5 5 5-5H7z" />
                      </svg>
                    {/if}
                  </span>
                {/if}
              </button>

              {#if i < visibleColumns.length - 1 && column !== "index" && column !== "album"}
                {@const rightColumn = visibleColumns[i + 1]}
                <button
                  type="button"
                  class="col-resizer"
                  class:active={resizingPair?.left === column}
                  aria-label={`Resize between ${columnLabel(column)} and ${columnLabel(rightColumn)}`}
                  onpointerdown={(e) =>
                    startColumnResize(column, rightColumn, e)}
                  ondblclick={() => resetColumnPair(column, rightColumn)}
                ></button>
              {/if}
            </div>
          {/each}
        </div>
        <div
          class="track-rows"
          class:track-drag-active={!!trackDrag?.active ||
            trackDragUi.isExportSession}
          class:has-reorder-gap={reorderGapActive}
          bind:this={rowsEl}
          onscroll={handleRowsScroll}
        >
          <div
            class="track-list-rows"
            class:drop-at-end={reorderDropIndex === displayedTracks.length &&
              displayedTracks.length > 0}
            style={reorderGapActive
              ? `padding-bottom: ${8 + DROP_GAP_PX}px`
              : undefined}
          >
            <div style="height: {visibleRange.top}px" aria-hidden="true"></div>
            {#each visibleTracks as item, localIndex (item.track.path)}
              {@const i = visibleRange.start + localIndex}
              {@const track = item.track}
              {@const isActive = sameTrackPath(track.path, player.currentFile)}
              {@const isSelected = selectedPaths.has(track.path)}
              {@const isDraggingRow =
                (!!trackDrag?.active &&
                  trackDrag.paths.includes(track.path)) ||
                (trackDragUi.isExportSession &&
                  trackDragUi.draggingPaths.includes(track.path))}
              {@const gapShift =
                reorderDropIndex != null && i >= reorderDropIndex
                  ? DROP_GAP_PX
                  : 0}
              {@const isGapEdge =
                reorderDropIndex != null && i === reorderDropIndex}
              <button
                class="track-row"
                class:active={isActive}
                class:playing={isActive && player.isPlaying}
                class:paused={isActive && player.isPaused && !player.isPlaying}
                class:selected={isSelected}
                class:dragging={isDraggingRow}
                class:gap-shift={isGapEdge}
                data-track-index={i}
                style="grid-template-columns: {gridTemplate}; transform: translate3d(0, {gapShift}px, 0)"
                onclick={(e) => handleTrackClick(item, i, e)}
                onpointerdown={(e) => onTrackPointerDown(e, item)}
                oncontextmenu={(e) => openTrackContextMenu(e, item, i)}
                title={`${trackDisplayTitle(track)} — ${trackDisplayArtist(track)}`}
              >
                {#each visibleColumns as column (column)}
                  {#if column === "index"}
                    <span class="col-index">
                      {#if isActive && player.isPlaying}
                        <span class="mini-eq" aria-label="Playing">
                          <span></span><span></span><span></span>
                        </span>
                      {:else if isActive && player.isPaused}
                        <span class="paused-icon" aria-label="Paused">
                          <svg
                            width="12"
                            height="12"
                            viewBox="0 0 24 24"
                            fill="currentColor"
                          >
                            <rect x="6" y="5" width="4" height="14" rx="1" />
                            <rect x="14" y="5" width="4" height="14" rx="1" />
                          </svg>
                        </span>
                      {:else}
                        <span class="track-num">{i + 1}</span>
                        <span class="play-icon" aria-hidden="true">
                          <svg
                            width="12"
                            height="12"
                            viewBox="0 0 24 24"
                            fill="currentColor"
                          >
                            <path d="M8 5v14l11-7z" />
                          </svg>
                        </span>
                      {/if}
                    </span>
                  {:else if column === "title"}
                    <span class="col-title">
                      <TrackCover {track} />
                      <span class="title-group">
                        <span class="track-name"
                          >{trackDisplayTitle(track)}</span
                        >
                        <span class="track-artist"
                          >{trackDisplayArtist(track)}</span
                        >
                      </span>
                    </span>
                  {:else if column === "album"}
                    <span class="col-album">{track.album ?? "—"}</span>
                  {:else}
                    <span class="col-duration">
                      <span
                        role="button"
                        tabindex="0"
                        class="like-btn like-duration"
                        class:liked={player.isLiked(track.path)}
                        onclick={(e) => {
                          e.stopPropagation();
                          player.toggleLike(track.path);
                        }}
                        onkeydown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.stopPropagation();
                            e.preventDefault();
                            player.toggleLike(track.path);
                          }
                        }}
                        title={player.isLiked(track.path)
                          ? "Remove from Liked"
                          : "Add to Liked"}
                        aria-label={player.isLiked(track.path)
                          ? "Unlike track"
                          : "Like track"}
                      >
                        <svg
                          width="13"
                          height="13"
                          viewBox="0 0 24 24"
                          fill={player.isLiked(track.path)
                            ? "currentColor"
                            : "none"}
                          stroke="currentColor"
                          stroke-width="2.2"
                          stroke-linecap="round"
                          stroke-linejoin="round"
                        >
                          <path
                            d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"
                          />
                        </svg>
                      </span>
                      <span class="duration-text"
                        >{formatDuration(track.duration_secs)}</span
                      >
                    </span>
                  {/if}
                {/each}
              </button>
            {/each}
            <div
              style="height: {visibleRange.bottom}px"
              aria-hidden="true"
            ></div>
          </div>
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
  <div
    role="presentation"
    onmouseenter={onTrackMenuPointerEnter}
    onmouseleave={onTrackMenuPointerLeave}
    onclick={stopMenuEventPropagation}
    onkeydown={handleAddToPlaylistDialogKeydown}
    oncontextmenu={stopMenuEventPropagation}
  >
    <ContextMenu
      open={contextMenu !== null}
      x={contextMenu?.x ?? 0}
      y={contextMenu?.y ?? 0}
      items={trackMenuItems}
      onclose={onContextMenuClose}
    />
  </div>

  <div
    role="presentation"
    onmouseenter={onSubmenuPointerEnter}
    onmouseleave={onSubmenuPointerLeave}
    onclick={stopMenuEventPropagation}
    onkeydown={handleAddToPlaylistDialogKeydown}
    oncontextmenu={stopMenuEventPropagation}
  >
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

{#if dragPreview}
  <div
    class="track-drag-float"
    class:is-copy={dragPreview.isCopy}
    class:is-export={dragPreview.exportMode}
    style="left: {dragPreview.x + 14}px; top: {dragPreview.y + 12}px; transform: rotate({dragPreview.rotate.toFixed(2)}deg)"
    aria-hidden="true"
  >
    <div class="track-drag-float-cover">
      <img
        src={dragPreview.coverSrc}
        alt=""
        draggable="false"
        onerror={() => {
          dragPreviewCoverFailed = true;
        }}
      />
      {#if dragPreview.count > 1}
        <span class="track-drag-float-badge">{dragPreview.count}</span>
      {/if}
    </div>
    <div class="track-drag-float-meta">
      <span class="track-drag-float-title">{dragPreview.title}</span>
      {#if dragPreview.artist}
        <span class="track-drag-float-artist">{dragPreview.artist}</span>
      {/if}
      {#if dragPreview.exportMode}
        <span class="track-drag-float-mode">Export file</span>
      {:else if dragPreview.isCopy}
        <span class="track-drag-float-mode">Copy</span>
      {/if}
    </div>
  </div>
{/if}

<style>
  @import "./TrackList.css";
</style>
