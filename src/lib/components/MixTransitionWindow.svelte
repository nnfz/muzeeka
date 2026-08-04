<script lang="ts">
  import "../../app.css";
  import "../../routes/+page.css";
  import "./MixTransitionWindow.css";
  import WindowControls from "./WindowControls.svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import {
    sameTrackPath,
    type MusicFile,
  } from "$lib/stores/player.svelte";
  import {
    loadMixTransitionMemory,
    saveMixTransitionMemory,
    takePendingMixTransition,
    trackLabel,
    type MixTransitionOpenPayload,
  } from "$lib/stores/mixTransition.svelte";
  import { hydrateAccentFromStorage } from "$lib/coverAccent";
  import MixEnvelopeGraph from "./MixEnvelopeGraph.svelte";
  import {
    MIX_PALETTE,
    MIN_BLOCK_SECS,
    blockEnd,
    childrenOf,
    childrenOnLane,
    clampBlock,
    createBlock,
    envelopeParamLabel,
    envelopeVToRate,
    isContainerKind,
    normalizeCurve,
    normalizeEnvelope,
    paletteItem,
    primaryTransition,
    sampleEnvelope,
    serializeBlocks,
    transitionAt,
    transitionBlocks,
    type EnvelopeCurve,
    type EnvelopePoint,
    type MixBlock,
    type MixBlockKind,
    type MixBlockTarget,
  } from "$lib/mix/blocks";

  interface WaveformPeaks {
    peaks: number[];
    durationSecs: number;
    bins: number;
    rangeStartSecs?: number;
    rangeEndSecs?: number;
  }

  type LaneId = "from" | "to";

  /** Peak samples for a track-relative time range. */
  type PeakBuf = {
    peaks: number[];
    start: number;
    end: number;
  };

  type LaneState = {
    /** Whole-track peaks — always kept, used when zoomed out. */
    overview: PeakBuf | null;
    /** Optional high-res window for zoomed-in views only. */
    detail: PeakBuf | null;
    /** Full playable length (segment for CUE, file for normal). */
    durationSecs: number;
    bpm: number | null;
    /** Seconds at the left edge of the viewport. */
    viewStartSecs: number;
    /** Beat-grid phase: seconds from track start to first downbeat. */
    gridOffsetSecs: number;
    loading: boolean;
    /** Quiet background re-fetch for zoom detail (no full-panel spinner). */
    refining: boolean;
    error: string | null;
  };

  const appWindow = getCurrentWindow();

  const MIN_PX_PER_SEC = 12;
  const MAX_PX_PER_SEC = 900;
  const DEFAULT_PX_PER_SEC = 90;
  /** How much of the previous-track end we keep in view on open (0..1 of viewport). */
  const PREV_END_FRACTION = 0.92;
  const BEATS_PER_BAR = 4;
  /**
   * How close (in screen px) the stick under the cursor must be to the other
   * track's stick before the magnet engages.
   */
  const SNAP_PX = 16;
  /** Detail LOD only when this many seconds (or less) are on screen. */
  const MAX_DETAIL_VIEW_SECS = 48;
  /** Never decode more than this many seconds for a detail window. */
  const MAX_DETAIL_RANGE_SECS = 72;
  const OVERVIEW_BINS_CAP = 2048;
  const DETAIL_BINS_CAP = 4096;
  /** Extra time around the viewport when fetching detail (sides each). */
  const LOD_PAD_RATIO = 1.15;
  /** Prefetch when remaining margin drops below this fraction of the view. */
  const LOD_MARGIN_RATIO = 0.45;
  const LOD_DEBOUNCE_MS = 120;
  const MEMORY_SAVE_MS = 200;


  let from = $state<MusicFile | null>(null);
  let to = $state<MusicFile | null>(null);
  let playlistId = $state<string | null>(null);
  let fromIndex = $state(0);

  let fromLane = $state<LaneState>(emptyLane());
  let toLane = $state<LaneState>(emptyLane());

  /** Shared horizontal scale (pixels per second of audio). */
  let pxPerSec = $state(DEFAULT_PX_PER_SEC);

  /**
   * Shared playhead / cut marker as time on the **previous** track.
   * Mapped to next via alignment offset. Drag freely inside the view.
   */
  let playheadFromSecs = $state(0);

  let fromCanvas = $state<HTMLCanvasElement | null>(null);
  let toCanvas = $state<HTMLCanvasElement | null>(null);
  let fromWrap = $state<HTMLDivElement | null>(null);
  let toWrap = $state<HTMLDivElement | null>(null);
  let stageEl = $state<HTMLDivElement | null>(null);

  let loadGen = 0;
  /** Per-lane request ids so stale LOD responses are dropped. */
  let waveReqFrom = 0;
  let waveReqTo = 0;
  let memoryTimer: ReturnType<typeof setTimeout> | null = null;
  /** Skip save while restoring memory / opening a pair. */
  let suppressMemorySave = false;

  let previewing = $state(false);
  let previewBusy = $state(false);
  let previewError = $state<string | null>(null);
  let previewGen = 0;
  /** Active preview: wall-clock clock for smooth playhead (not only IPC ticks). */
  let previewSession = $state<{
    fromStartRel: number;
    toStartRel: number;
    crossfadeSecs: number;
    wallStartMs: number;
  } | null>(null);

  let playheadDrag = $state<{
    pointerId: number;
    grabX: number;
  } | null>(null);
  let lodTimer: ReturnType<typeof setTimeout> | null = null;

  let drag = $state<{
    lane: LaneId;
    pointerId: number;
    startX: number;
    startView: number;
    /** True once pointer moved past threshold (real pan). */
    active: boolean;
    /** True while the magnet is holding a snap target. */
    snapped: boolean;
  } | null>(null);

  /** Timeline blocks: transition containers + effect inserts. */
  let blocks = $state<MixBlock[]>([]);
  let selectedBlockId = $state<string | null>(null);
  /** Expanded effect editor (automation graph). */
  let expandedBlockId = $state<string | null>(null);
  let blockError = $state<string | null>(null);

  /** Undo / redo for editor mutations (blocks, playhead, views). */
  type EditorSnapshot = {
    blocks: MixBlock[];
    selectedBlockId: string | null;
    expandedBlockId: string | null;
    playheadFromSecs: number;
    fromViewStart: number;
    toViewStart: number;
    fromGridOffset: number;
    toGridOffset: number;
    pxPerSec: number;
  };
  const UNDO_MAX = 80;
  let undoStack = $state<EditorSnapshot[]>([]);
  let redoStack = $state<EditorSnapshot[]>([]);
  /** True while restoring a snapshot — don't record that as a new undo step. */
  let undoSuspended = false;

  /** Drag a palette chip onto the stage. */
  let paletteDrag = $state<{
    kind: MixBlockKind;
    pointerId: number;
    clientX: number;
    clientY: number;
  } | null>(null);

  /** Move / resize a placed block. */
  let blockInteract = $state<{
    id: string;
    mode: "move" | "resize-l" | "resize-r";
    pointerId: number;
    originStart: number;
    originDur: number;
    originClientX: number;
  } | null>(null);

  const LANE_PAN_THRESHOLD = 4;

  if (typeof document !== "undefined") {
    document.documentElement.style.setProperty(
      "background-color",
      "#0a0a0f",
      "important",
    );
    if (document.body) {
      document.body.style.setProperty(
        "background-color",
        "#0a0a0f",
        "important",
      );
    }
    hydrateAccentFromStorage();
  }

  let titleText = $derived.by(() => {
    if (!from || !to) return "Mix transition";
    return `${trackLabel(from).split(" — ")[0]} → ${trackLabel(to).split(" — ")[0]}`;
  });

  let zoomLabel = $derived(
    pxPerSec >= 100
      ? `${Math.round(pxPerSec)} px/s`
      : `${pxPerSec.toFixed(0)} px/s`,
  );

  function emptyLane(): LaneState {
    return {
      overview: null,
      detail: null,
      durationSecs: 0,
      bpm: null,
      viewStartSecs: 0,
      gridOffsetSecs: 0,
      loading: false,
      refining: false,
      error: null,
    };
  }

  function trackOf(id: LaneId): MusicFile | null {
    return id === "from" ? from : to;
  }

  function bumpWaveReq(id: LaneId): number {
    if (id === "from") {
      waveReqFrom += 1;
      return waveReqFrom;
    }
    waveReqTo += 1;
    return waveReqTo;
  }

  function currentWaveReq(id: LaneId): number {
    return id === "from" ? waveReqFrom : waveReqTo;
  }

  function cueBase(track: MusicFile): number {
    const s = track.cue_start_secs;
    return typeof s === "number" && Number.isFinite(s) && s >= 0 ? s : 0;
  }

  function trackDurationHint(track: MusicFile): number {
    if (
      typeof track.cue_start_secs === "number" &&
      typeof track.cue_end_secs === "number" &&
      track.cue_end_secs > track.cue_start_secs
    ) {
      return track.cue_end_secs - track.cue_start_secs;
    }
    if (typeof track.duration_secs === "number" && track.duration_secs > 0) {
      return track.duration_secs;
    }
    return 0;
  }

  function covers(buf: PeakBuf, viewStart: number, viewEnd: number): boolean {
    const pad = Math.max(0.02, (viewEnd - viewStart) * 0.04);
    return buf.start <= viewStart + pad && buf.end >= viewEnd - pad;
  }

  /**
   * True when the view still has enough padding inside the detail window.
   * Near track edges, missing margin on that side is fine (nowhere to pad).
   */
  function hasHealthyMargin(
    buf: PeakBuf,
    viewStart: number,
    viewEnd: number,
    trackDur: number,
  ): boolean {
    const viewSpan = Math.max(0.05, viewEnd - viewStart);
    const need = viewSpan * LOD_MARGIN_RATIO;
    const leftOk =
      viewStart - buf.start >= need || buf.start <= 0.02 || viewStart <= 0.02;
    const rightOk =
      buf.end - viewEnd >= need ||
      buf.end >= trackDur - 0.02 ||
      viewEnd >= trackDur - 0.02;
    return leftOk && rightOk;
  }

  function densityOk(buf: PeakBuf, viewSpan: number, widthPx: number): boolean {
    const span = buf.end - buf.start;
    if (span <= 0 || buf.peaks.length < 8) return false;
    const binsInView = buf.peaks.length * (Math.max(0.05, viewSpan) / span);
    // ~1 sample/px is enough to look sharp; below ~0.55 we re-fetch.
    return binsInView >= widthPx * 0.55;
  }

  /**
   * Sample amplitude at track time `t`.
   * When zoomed in: use detail where available, fall back to overview only for
   * holes — never swap the whole strip to overview mid-scroll (that caused the
   * low-poly flash).
   */
  function sampleLaneAmp(
    lane: LaneState,
    t: number,
    viewSpan: number,
  ): number {
    const zoomedIn = viewSpan <= MAX_DETAIL_VIEW_SECS;
    if (zoomedIn && lane.detail) {
      if (t >= lane.detail.start && t <= lane.detail.end) {
        return peakAt(lane.detail, t);
      }
    }
    if (lane.overview) return peakAt(lane.overview, t);
    if (lane.detail) return peakAt(lane.detail, t);
    return 0;
  }

  function laneOf(id: LaneId): LaneState {
    return id === "from" ? fromLane : toLane;
  }

  function setLane(id: LaneId, patch: Partial<LaneState>) {
    if (id === "from") fromLane = { ...fromLane, ...patch };
    else toLane = { ...toLane, ...patch };
  }

  function wrapOf(id: LaneId): HTMLDivElement | null {
    return id === "from" ? fromWrap : toWrap;
  }

  function canvasOf(id: LaneId): HTMLCanvasElement | null {
    return id === "from" ? fromCanvas : toCanvas;
  }

  function viewportSecsFor(wrap: HTMLElement | null): number {
    const w = wrap?.clientWidth ?? 800;
    return Math.max(0.05, w / pxPerSec);
  }

  /**
   * Default placement only (open / reset) — free pan after that, no hard clamps.
   * Previous → end in view; next → start in view. Playhead at previous end (cut).
   */
  function applyDefaultViews() {
    const fromVp = viewportSecsFor(fromWrap);

    if (fromLane.durationSecs > 0) {
      fromLane = {
        ...fromLane,
        viewStartSecs: fromLane.durationSecs - fromVp * PREV_END_FRACTION,
      };
      playheadFromSecs = fromLane.durationSecs;
    }
    if (toLane.durationSecs > 0) {
      toLane = {
        ...toLane,
        viewStartSecs: 0,
      };
    }
  }

  function applyMemoryViews(
    mem: NonNullable<ReturnType<typeof loadMixTransitionMemory>>,
  ) {
    pxPerSec = Math.min(
      MAX_PX_PER_SEC,
      Math.max(MIN_PX_PER_SEC, mem.pxPerSec),
    );
    fromLane = {
      ...fromLane,
      viewStartSecs: mem.fromViewStart,
      gridOffsetSecs: mem.fromGridOffset,
    };
    toLane = {
      ...toLane,
      viewStartSecs: mem.toViewStart,
      gridOffsetSecs: mem.toGridOffset,
    };
    playheadFromSecs = mem.playheadFromSecs;
    blocks = mem.blocks ?? [];
    selectedBlockId = null;
    expandedBlockId = null;
  }

  function scheduleMemorySave() {
    if (suppressMemorySave || previewing) return;
    if (!playlistId || !from?.path || !to?.path) return;
    if (memoryTimer) clearTimeout(memoryTimer);
    memoryTimer = setTimeout(() => {
      memoryTimer = null;
      if (suppressMemorySave || !playlistId || !from?.path || !to?.path) return;
      saveMixTransitionMemory(playlistId, from.path, to.path, {
        fromViewStart: fromLane.viewStartSecs,
        toViewStart: toLane.viewStartSecs,
        pxPerSec,
        playheadFromSecs,
        fromGridOffset: fromLane.gridOffsetSecs,
        toGridOffset: toLane.gridOffsetSecs,
        blocks,
      });
    }, MEMORY_SAVE_MS);
  }

  function cloneBlocks(list: MixBlock[]): MixBlock[] {
    return serializeBlocks(list);
  }

  function captureSnapshot(): EditorSnapshot {
    return {
      blocks: cloneBlocks(blocks),
      selectedBlockId,
      expandedBlockId,
      playheadFromSecs,
      fromViewStart: fromLane.viewStartSecs,
      toViewStart: toLane.viewStartSecs,
      fromGridOffset: fromLane.gridOffsetSecs,
      toGridOffset: toLane.gridOffsetSecs,
      pxPerSec,
    };
  }

  /** Record current state before a user mutation (once per gesture). */
  function pushUndo() {
    if (undoSuspended || suppressMemorySave) return;
    const snap = captureSnapshot();
    undoStack = [...undoStack.slice(-(UNDO_MAX - 1)), snap];
    redoStack = [];
  }

  function clearUndoHistory() {
    undoStack = [];
    redoStack = [];
  }

  function applySnapshot(snap: EditorSnapshot) {
    undoSuspended = true;
    blocks = cloneBlocks(snap.blocks);
    selectedBlockId = snap.selectedBlockId;
    expandedBlockId = snap.expandedBlockId;
    playheadFromSecs = snap.playheadFromSecs;
    pxPerSec = Math.min(
      MAX_PX_PER_SEC,
      Math.max(MIN_PX_PER_SEC, snap.pxPerSec),
    );
    fromLane = {
      ...fromLane,
      viewStartSecs: snap.fromViewStart,
      gridOffsetSecs: snap.fromGridOffset,
    };
    toLane = {
      ...toLane,
      viewStartSecs: snap.toViewStart,
      gridOffsetSecs: snap.toGridOffset,
    };
    undoSuspended = false;
    scheduleMemorySave();
  }

  function undo() {
    if (undoStack.length === 0) return;
    const prev = undoStack[undoStack.length - 1]!;
    undoStack = undoStack.slice(0, -1);
    redoStack = [...redoStack.slice(-(UNDO_MAX - 1)), captureSnapshot()];
    applySnapshot(prev);
  }

  function redo() {
    if (redoStack.length === 0) return;
    const next = redoStack[redoStack.length - 1]!;
    redoStack = redoStack.slice(0, -1);
    undoStack = [...undoStack.slice(-(UNDO_MAX - 1)), captureSnapshot()];
    applySnapshot(next);
  }

  function updateBlock(id: string, patch: Partial<MixBlock>) {
    blocks = blocks.map((b) => (b.id === id ? { ...b, ...patch } : b));
    scheduleMemorySave();
  }

  function removeBlock(id: string) {
    const victim = blocks.find((b) => b.id === id);
    if (!victim) return;
    pushUndo();
    // Removing a transition also drops its children.
    if (victim.kind === "transition") {
      blocks = blocks.filter((b) => b.id !== id && b.parentId !== id);
    } else {
      blocks = blocks.filter((b) => b.id !== id);
    }
    if (selectedBlockId === id) selectedBlockId = null;
    if (expandedBlockId === id) expandedBlockId = null;
    scheduleMemorySave();
  }

  function togglePin(id: string) {
    const b = blocks.find((x) => x.id === id);
    if (!b) return;
    pushUndo();
    updateBlock(id, { pinned: !b.pinned });
  }

  function toggleExpand(id: string) {
    const b = blocks.find((x) => x.id === id);
    if (!b || isContainerKind(b.kind)) return;
    // Expand is UI chrome only — no undo entry.
    expandedBlockId = expandedBlockId === id ? null : id;
    selectedBlockId = id;
  }

  function toggleTargetLane(id: string) {
    const b = blocks.find((x) => x.id === id);
    if (!b || isContainerKind(b.kind)) return;
    pushUndo();
    const next: MixBlockTarget = b.targetLane === "to" ? "from" : "to";
    updateBlock(id, { targetLane: next });
  }

  function setBlockEnvelope(id: string, envelope: EnvelopePoint[]) {
    const b = blocks.find((x) => x.id === id);
    if (!b) return;
    updateBlock(id, {
      params: {
        ...b.params,
        envelope: normalizeEnvelope(envelope),
      },
    });
  }

  function setBlockCurve(id: string, curve: EnvelopeCurve) {
    const b = blocks.find((x) => x.id === id);
    if (!b) return;
    if (normalizeCurve(b.params.curve) === curve) return;
    updateBlock(id, {
      params: {
        ...b.params,
        curve,
      },
    });
  }

  /** Called once at the start of an envelope edit gesture. */
  function beginEnvelopeEdit() {
    pushUndo();
  }

  /** Which deck a pointer Y is over (upper = prev, lower = next). */
  function targetLaneAtClientY(clientY: number): MixBlockTarget {
    if (!fromWrap || !toWrap) return "from";
    const fromRect = fromWrap.getBoundingClientRect();
    const toRect = toWrap.getBoundingClientRect();
    if (clientY < fromRect.bottom - 2) return "from";
    if (clientY > toRect.top + 2) return "to";
    // Gap between lanes — nearer deck.
    const mid = (fromRect.bottom + toRect.top) / 2;
    return clientY < mid ? "from" : "to";
  }

  /** Prev-track time under a client X (wave column). */
  function timeAtClientX(clientX: number): number | null {
    if (!fromWrap || pxPerSec <= 0) return null;
    const rect = fromWrap.getBoundingClientRect();
    const x = Math.max(0, Math.min(rect.width, clientX - rect.left));
    return fromLane.viewStartSecs + x / pxPerSec;
  }

  function stageWaveFrame(): {
    left: number;
    top: number;
    width: number;
    height: number;
    fromTop: number;
    fromHeight: number;
    toTop: number;
    toHeight: number;
  } | null {
    if (!stageEl || !fromWrap || !toWrap) return null;
    const stageRect = stageEl.getBoundingClientRect();
    const fromRect = fromWrap.getBoundingClientRect();
    const toRect = toWrap.getBoundingClientRect();
    const w = fromWrap.clientWidth;
    if (w <= 0) return null;
    return {
      left: fromRect.left - stageRect.left,
      top: fromRect.top - stageRect.top,
      width: w,
      height: Math.max(0, toRect.bottom - fromRect.top),
      fromTop: fromRect.top - stageRect.top,
      fromHeight: fromRect.height,
      toTop: toRect.top - stageRect.top,
      toHeight: toRect.height,
    };
  }

  type BlockLayout = {
    id: string;
    kind: MixBlockKind;
    left: number;
    top: number;
    width: number;
    height: number;
    label: string;
    short: string;
    accent: string;
    pinned: boolean;
    selected: boolean;
    expanded: boolean;
    targetLane: MixBlockTarget | null;
    parentId: string | null;
    startFromSecs: number;
    durationSecs: number;
    envelope: EnvelopePoint[];
    curve: EnvelopeCurve;
  };

  const EFFECT_STRIP_H = 26;
  const EFFECT_EXPANDED_H = 132;

  function layoutBlocks(): BlockLayout[] {
    const frame = stageWaveFrame();
    if (!frame || pxPerSec <= 0) return [];
    const out: BlockLayout[] = [];
    const view0 = fromLane.viewStartSecs;
    const view1 = view0 + frame.width / pxPerSec;

    const place = (b: MixBlock, top: number, height: number) => {
      const end = blockEnd(b);
      // Cull only when fully off-screen (keep a small pad for pan).
      if (end < view0 - 0.05 || b.startFromSecs > view1 + 0.05) return;
      // Full timeline geometry — never clip width to the camera.
      // Envelope graphs must map 0..1 of *block duration* to this full width;
      // the layer's overflow clips what sticks out of the wave column.
      const rawL = (b.startFromSecs - view0) * pxPerSec;
      const rawR = (end - view0) * pxPerSec;
      const fullW = Math.max(4, rawR - rawL);
      // Skip microscopic strips only (extreme zoom-out of tiny blocks).
      if (fullW < 2 && b.durationSecs < 0.05) return;
      const meta = paletteItem(b.kind);
      const expanded = expandedBlockId === b.id;
      out.push({
        id: b.id,
        kind: b.kind,
        left: frame.left + rawL,
        top,
        width: fullW,
        height,
        label: meta?.label ?? b.kind,
        short: meta?.short ?? b.kind.slice(0, 2).toUpperCase(),
        accent: meta?.accent ?? "#7c5cff",
        pinned: b.pinned,
        selected: selectedBlockId === b.id,
        expanded,
        targetLane: b.targetLane,
        parentId: b.parentId,
        startFromSecs: b.startFromSecs,
        durationSecs: b.durationSecs,
        envelope: normalizeEnvelope(
          b.params.envelope ??
            (isContainerKind(b.kind) ? [{ t: 0, v: 0.5 }, { t: 1, v: 0.5 }] : undefined),
        ),
        curve: normalizeCurve(b.params.curve),
      });
    };

    const stackLane = (
      kids: MixBlock[],
      laneTop: number,
      laneH: number,
    ) => {
      if (kids.length === 0 || laneH <= 0) return;
      // Stack from bottom of the lane so waveforms stay readable on top.
      let y = laneTop + laneH - 4;
      for (let i = kids.length - 1; i >= 0; i--) {
        const child = kids[i]!;
        const h =
          expandedBlockId === child.id ? EFFECT_EXPANDED_H : EFFECT_STRIP_H;
        y -= h + 3;
        const top = Math.max(laneTop + 2, y);
        place(child, top, Math.min(h, laneTop + laneH - top - 2));
      }
    };

    // Transition containers span both waveforms.
    for (const t of transitionBlocks(blocks)) {
      place(t, frame.top, frame.height);
      stackLane(
        childrenOnLane(blocks, t.id, "from"),
        frame.fromTop,
        frame.fromHeight,
      );
      stackLane(
        childrenOnLane(blocks, t.id, "to"),
        frame.toTop,
        frame.toHeight,
      );
    }

    // Orphan effects — place on their target lane.
    for (const b of blocks) {
      if (b.parentId != null) continue;
      if (isContainerKind(b.kind)) continue;
      const lane = b.targetLane === "to" ? "to" : "from";
      const top = lane === "to" ? frame.toTop : frame.fromTop;
      const h = lane === "to" ? frame.toHeight : frame.fromHeight;
      const eh =
        expandedBlockId === b.id ? EFFECT_EXPANDED_H : EFFECT_STRIP_H;
      place(b, top + h - eh - 6, eh);
    }

    return out;
  }

  let blockLayouts = $derived.by(() => {
    void blocks;
    void selectedBlockId;
    void expandedBlockId;
    void fromLane.viewStartSecs;
    void toLane.viewStartSecs;
    void pxPerSec;
    void fromWrap;
    void toWrap;
    void stageEl;
    void fromLane.durationSecs;
    void toLane.durationSecs;
    return layoutBlocks();
  });

  function onPalettePointerDown(e: PointerEvent, kind: MixBlockKind) {
    const item = paletteItem(kind);
    if (!item?.enabled) return;
    if (e.button !== 0) return;
    e.preventDefault();
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    paletteDrag = {
      kind,
      pointerId: e.pointerId,
      clientX: e.clientX,
      clientY: e.clientY,
    };
    blockError = null;
  }

  function onPalettePointerMove(e: PointerEvent) {
    if (!paletteDrag || paletteDrag.pointerId !== e.pointerId) return;
    paletteDrag = {
      ...paletteDrag,
      clientX: e.clientX,
      clientY: e.clientY,
    };
  }

  function onPalettePointerUp(e: PointerEvent) {
    if (!paletteDrag || paletteDrag.pointerId !== e.pointerId) return;
    const kind = paletteDrag.kind;
    const cx = e.clientX;
    const cy = e.clientY;
    paletteDrag = null;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }

    // Must drop over the wave stage.
    if (!stageEl || !fromWrap || !toWrap) return;
    const fromRect = fromWrap.getBoundingClientRect();
    const toRect = toWrap.getBoundingClientRect();
    const over =
      cx >= fromRect.left &&
      cx <= fromRect.right &&
      cy >= fromRect.top &&
      cy <= toRect.bottom;
    if (!over) {
      blockError = "Drop on the waveforms to place a block";
      return;
    }

    const t = timeAtClientX(cx);
    if (t == null) return;

    if (isContainerKind(kind)) {
      // One transition is enough for a pair in v1 — replace if user re-drops.
      pushUndo();
      const existing = transitionBlocks(blocks);
      const dur = createBlock(kind, t, { bpm: fromLane.bpm }).durationSecs;
      let start = t - dur * 0.35;
      let nb = clampBlock(
        createBlock(kind, start, { durationSecs: dur, bpm: fromLane.bpm }),
        {
          minStart: 0,
          maxEnd:
            fromLane.durationSecs > 0
              ? Math.max(fromLane.durationSecs, t + dur)
              : t + dur * 2,
        },
      );
      // Keep children of previous transition if replacing.
      if (existing.length > 0) {
        const oldId = existing[0]!.id;
        const kids = childrenOf(blocks, oldId).map((c) => ({
          ...c,
          parentId: nb.id,
          // Re-clamp into new container.
          ...(() => {
            const clamped = clampBlock(c, {
              minStart: nb.startFromSecs,
              maxEnd: blockEnd(nb),
            });
            return {
              startFromSecs: clamped.startFromSecs,
              durationSecs: clamped.durationSecs,
            };
          })(),
        }));
        blocks = [
          nb,
          ...blocks.filter((b) => b.id !== oldId && b.parentId !== oldId),
          ...kids,
        ];
      } else {
        blocks = [...blocks, nb];
      }
      selectedBlockId = nb.id;
      // Nudge playhead into the block for preview context.
      if (
        playheadFromSecs < nb.startFromSecs ||
        playheadFromSecs > blockEnd(nb)
      ) {
        playheadFromSecs = nb.startFromSecs;
      }
      blockError = null;
      scheduleMemorySave();
      return;
    }

    // Effect — must land inside a transition; deck from drop Y (top/bottom).
    const host = transitionAt(blocks, t);
    if (!host) {
      blockError = "Place a Transition block first, then drop effects into it";
      return;
    }
    pushUndo();
    const targetLane = targetLaneAtClientY(cy);
    const dur = Math.min(
      createBlock(kind, t, { bpm: fromLane.bpm }).durationSecs,
      host.durationSecs,
    );
    let start = Math.max(host.startFromSecs, t - dur * 0.25);
    let nb = clampBlock(
      createBlock(kind, start, {
        durationSecs: dur,
        parentId: host.id,
        targetLane,
        bpm: fromLane.bpm,
      }),
      { minStart: host.startFromSecs, maxEnd: blockEnd(host) },
    );
    blocks = [...blocks, nb];
    selectedBlockId = nb.id;
    expandedBlockId = nb.id;
    blockError = null;
    scheduleMemorySave();
  }

  function onBlockPointerDown(
    e: PointerEvent,
    id: string,
    mode: "move" | "resize-l" | "resize-r",
  ) {
    const b = blocks.find((x) => x.id === id);
    if (!b) return;
    // Pinned blocks use pointer-events:none (except action buttons) so
    // waveform pan reaches the tracks underneath — never capture here.
    if (b.pinned) return;

    e.preventDefault();
    e.stopPropagation();
    // One undo step for the whole move/resize drag.
    pushUndo();
    selectedBlockId = id;
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    blockInteract = {
      id,
      mode,
      pointerId: e.pointerId,
      originStart: b.startFromSecs,
      originDur: b.durationSecs,
      originClientX: e.clientX,
    };
  }

  function onBlockPointerMove(e: PointerEvent) {
    if (!blockInteract || blockInteract.pointerId !== e.pointerId) return;
    if (pxPerSec <= 0) return;
    const b = blocks.find((x) => x.id === blockInteract!.id);
    if (!b || b.pinned) return;

    const dx = e.clientX - blockInteract.originClientX;
    const dt = dx / pxPerSec;

    let bounds: { minStart?: number; maxEnd?: number } = {
      minStart: 0,
      maxEnd:
        fromLane.durationSecs > 0
          ? fromLane.durationSecs + 60
          : Number.POSITIVE_INFINITY,
    };
    if (b.parentId) {
      const parent = blocks.find((x) => x.id === b.parentId);
      if (parent) {
        bounds = {
          minStart: parent.startFromSecs,
          maxEnd: blockEnd(parent),
        };
      }
    }

    let next = { ...b };
    if (blockInteract.mode === "move") {
      next.startFromSecs = blockInteract.originStart + dt;
    } else if (blockInteract.mode === "resize-l") {
      const newStart = blockInteract.originStart + dt;
      const end = blockInteract.originStart + blockInteract.originDur;
      const start = Math.min(newStart, end - MIN_BLOCK_SECS);
      next.startFromSecs = start;
      next.durationSecs = end - start;
    } else {
      next.durationSecs = Math.max(
        MIN_BLOCK_SECS,
        blockInteract.originDur + dt,
      );
    }
    next = clampBlock(next, bounds);

    // Keep children inside transition when the container moves/resizes.
    if (b.kind === "transition") {
      const kids = childrenOf(blocks, b.id).map((c) =>
        clampBlock(c, {
          minStart: next.startFromSecs,
          maxEnd: blockEnd(next),
        }),
      );
      blocks = blocks.map((x) => {
        if (x.id === b.id) return next;
        const kid = kids.find((k) => k.id === x.id);
        return kid ?? x;
      });
    } else {
      blocks = blocks.map((x) => (x.id === b.id ? next : x));
    }
  }

  function onBlockPointerUp(e: PointerEvent) {
    if (!blockInteract || blockInteract.pointerId !== e.pointerId) return;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    blockInteract = null;
    scheduleMemorySave();
  }

  /**
   * Keep the playhead on-screen while preview runs — otherwise it sticks to the
   * right edge of the wave strip ("divider") and looks frozen even though time advances.
   * Both lanes pan together so alignment is preserved.
   */
  function followPlayheadInView() {
    if (!fromWrap || pxPerSec <= 0) return;
    const w = fromWrap.clientWidth;
    if (w <= 0) return;
    // Prefer playhead in the right-center of the viewport (DAW-style transport).
    const softLeft = w * 0.12;
    const softRight = w * 0.72;
    const x = (playheadFromSecs - fromLane.viewStartSecs) * pxPerSec;
    let deltaPx = 0;
    if (x > softRight) deltaPx = x - softRight;
    else if (x < softLeft) deltaPx = x - softLeft;
    if (Math.abs(deltaPx) < 0.5) return;
    const dt = deltaPx / pxPerSec;
    fromLane = {
      ...fromLane,
      viewStartSecs: fromLane.viewStartSecs + dt,
    };
    toLane = {
      ...toLane,
      viewStartSecs: toLane.viewStartSecs + dt,
    };
  }

  /** Screen X of playhead inside the wave column. */
  function playheadLayout(): {
    left: number;
    top: number;
    height: number;
    inView: boolean;
    xInWave: number;
  } | null {
    if (!stageEl || !fromWrap || !toWrap) return null;
    const stageRect = stageEl.getBoundingClientRect();
    const fromRect = fromWrap.getBoundingClientRect();
    const toRect = toWrap.getBoundingClientRect();
    const waveW = fromWrap.clientWidth;
    if (waveW <= 0 || pxPerSec <= 0) return null;

    const rawX = (playheadFromSecs - fromLane.viewStartSecs) * pxPerSec;
    const inView = rawX >= -0.5 && rawX <= waveW + 0.5;
    // While previewing we auto-scroll — still clamp a bit so the knob isn't lost.
    // When editing (not previewing), clamp to the strip so drag stays usable.
    const xInWave = Math.max(0, Math.min(waveW, rawX));
    return {
      left: fromRect.left - stageRect.left + xInWave,
      top: fromRect.top - stageRect.top,
      height: Math.max(0, toRect.bottom - fromRect.top),
      inView,
      xInWave,
    };
  }

  let playheadUi = $derived.by(() => {
    // Depend on layout drivers.
    void fromLane.viewStartSecs;
    void toLane.viewStartSecs;
    void playheadFromSecs;
    void pxPerSec;
    void fromWrap;
    void toWrap;
    void stageEl;
    void fromLane.durationSecs;
    void toLane.durationSecs;
    return playheadLayout();
  });

  function formatBpm(n: number | null | undefined): string {
    if (n == null || !Number.isFinite(n) || n <= 0) return "—";
    const r = Math.round(n * 10) / 10;
    return Number.isInteger(r) ? String(r) : r.toFixed(1);
  }

  function formatTime(secs: number): string {
    if (!Number.isFinite(secs) || secs < 0) secs = 0;
    const m = Math.floor(secs / 60);
    const s = Math.floor(secs % 60);
    const cs = Math.floor((secs % 1) * 10);
    return `${m}:${s.toString().padStart(2, "0")}.${cs}`;
  }

  async function loadBpm(track: MusicFile, gen: number, id: LaneId) {
    try {
      const bpm = await invoke<number | null>("library_get_bpm", {
        path: track.path,
        audioPath: track.audio_path ?? null,
      });
      if (gen !== loadGen) return;
      const t = id === "from" ? from : to;
      if (!t || !sameTrackPath(t.path, track.path)) return;
      const value =
        typeof bpm === "number" && Number.isFinite(bpm) && bpm > 0
          ? bpm
          : null;
      setLane(id, { bpm: value });
    } catch {
      if (gen !== loadGen) return;
      setLane(id, { bpm: null });
    }
  }

  /** Fold offset into [0, beat). */
  function foldGridOffset(offset: number, bpm: number): number {
    const beat = 60 / bpm;
    if (!(beat > 1e-6)) return 0;
    let o = offset % beat;
    if (o < 0) o += beat;
    return o;
  }

  /** Place a grid stick under track-relative time `t` (click-on-kick). */
  function alignGridToTime(id: LaneId, t: number) {
    const lane = laneOf(id);
    if (lane.bpm == null || lane.bpm <= 0) return;
    pushUndo();
    const off = foldGridOffset(t, lane.bpm);
    setLane(id, { gridOffsetSecs: off });
    scheduleMemorySave();
  }

  /** Nudge grid phase by a fraction of a beat (negative = earlier sticks). */
  function nudgeGrid(id: LaneId, beatFraction: number) {
    const lane = laneOf(id);
    if (lane.bpm == null || lane.bpm <= 0) return;
    pushUndo();
    const beat = 60 / lane.bpm;
    const off = foldGridOffset(
      lane.gridOffsetSecs + beat * beatFraction,
      lane.bpm,
    );
    setLane(id, { gridOffsetSecs: off });
    scheduleMemorySave();
  }

  let gridAlignBusy = $state<"from" | "to" | null>(null);

  /**
   * Analyze onsets and snap grid sticks to kicks.
   * `silent` — no undo entry (used on first open).
   */
  async function alignGridToKick(id: LaneId, silent = false) {
    const track = trackOf(id);
    const lane = laneOf(id);
    if (!track || lane.bpm == null || lane.bpm <= 0) return;
    if (gridAlignBusy) return;
    gridAlignBusy = id;
    try {
      const off = await invoke<number>("library_detect_beat_offset", {
        path: track.path,
        audioPath: track.audio_path ?? null,
        bpm: lane.bpm,
      });
      if (typeof off !== "number" || !Number.isFinite(off)) return;
      if (!silent) pushUndo();
      setLane(id, {
        gridOffsetSecs: foldGridOffset(off, lane.bpm),
      });
      scheduleMemorySave();
      redrawAll();
    } catch (e) {
      console.warn("beat align failed", e);
    } finally {
      gridAlignBusy = null;
    }
  }

  /**
   * Fetch peaks for absolute file times [absStart, absEnd].
   * Pass `absEnd = null` for “until end of file”.
   * `mode: overview` keeps a permanent full-track buffer; `detail` is zoom-only.
   */
  async function fetchPeaksAbs(
    track: MusicFile,
    gen: number,
    id: LaneId,
    absStart: number,
    absEnd: number | null,
    bins: number,
    mode: "overview" | "detail",
  ) {
    const req = bumpWaveReq(id);
    if (mode === "overview") {
      setLane(id, { loading: true, error: null, refining: false });
    } else {
      setLane(id, { refining: true, error: null });
    }

    const base = cueBase(track);

    try {
      const data = await invoke<WaveformPeaks>("library_get_waveform", {
        path: track.path,
        audioPath: track.audio_path ?? null,
        bins: Math.max(64, Math.min(DETAIL_BINS_CAP, Math.round(bins))),
        cueStartSecs: absStart,
        cueEndSecs: absEnd,
      });
      if (gen !== loadGen || req !== currentWaveReq(id)) return;
      const t = trackOf(id);
      if (!t || !sameTrackPath(t.path, track.path)) return;

      const rs =
        typeof data.rangeStartSecs === "number" ? data.rangeStartSecs : absStart;
      const re =
        typeof data.rangeEndSecs === "number"
          ? data.rangeEndSecs
          : absEnd ?? rs + (data.durationSecs || 0);
      const buf: PeakBuf = {
        peaks: data.peaks ?? [],
        start: Math.max(0, rs - base),
        end: Math.max(0.05, re - base),
      };
      if (buf.end <= buf.start) buf.end = buf.start + 0.05;

      if (mode === "overview") {
        const duration =
          trackDurationHint(track) ||
          buf.end ||
          (typeof data.durationSecs === "number" ? data.durationSecs : 0);
        setLane(id, {
          overview: buf,
          // Drop any stale zoom window from a previous track.
          detail: null,
          durationSecs: duration,
          loading: false,
          refining: false,
          error: null,
        });
      } else {
        setLane(id, {
          detail: buf,
          refining: false,
          error: null,
        });
        // View may have moved while decoding — top up if still short on margin.
        scheduleLodRefresh();
      }
    } catch (e) {
      if (gen !== loadGen || req !== currentWaveReq(id)) return;
      const msg = typeof e === "string" ? e : String(e);
      if (mode === "overview") {
        setLane(id, {
          overview: null,
          loading: false,
          refining: false,
          error: msg.replace(/^Error:\s*/i, ""),
        });
      } else {
        setLane(id, { refining: false });
        scheduleLodRefresh();
      }
    }
  }

  async function loadWaveformOverview(
    track: MusicFile,
    gen: number,
    id: LaneId,
  ) {
    const wrap = wrapOf(id);
    const width = wrap?.clientWidth ?? 900;
    // Fixed moderate budget — never proportional to track length.
    const bins = Math.max(
      400,
      Math.min(OVERVIEW_BINS_CAP, Math.round(width * 1.4)),
    );
    const base = cueBase(track);
    const absEnd =
      typeof track.cue_end_secs === "number" &&
      Number.isFinite(track.cue_end_secs)
        ? track.cue_end_secs
        : null;
    await fetchPeaksAbs(track, gen, id, base, absEnd, bins, "overview");
  }

  /**
   * Only when zoomed in tightly: fetch a high-res window with generous pad so
   * scrolling does not immediately run out of detail (and flash overview).
   */
  function ensureDetail(id: LaneId) {
    const track = trackOf(id);
    const lane = laneOf(id);
    const wrap = wrapOf(id);
    if (!track || !wrap || lane.durationSecs <= 0) return;
    if (lane.loading || lane.refining) return;
    if (!lane.overview) return;

    const width = wrap.clientWidth;
    if (width <= 0) return;
    const viewStart = lane.viewStartSecs;
    const viewDur = width / pxPerSec;
    const viewEnd = viewStart + viewDur;

    // Zoomed out → overview is enough. Do not start heavy decodes.
    if (viewDur > MAX_DETAIL_VIEW_SECS) return;

    // Still healthy (covers + margin + density) → nothing to do.
    if (
      lane.detail &&
      covers(lane.detail, viewStart, viewEnd) &&
      hasHealthyMargin(lane.detail, viewStart, viewEnd, lane.durationSecs) &&
      densityOk(lane.detail, viewDur, width)
    ) {
      return;
    }

    const pad = Math.max(viewDur * LOD_PAD_RATIO, 4);
    let r0 = Math.max(0, viewStart - pad);
    let r1 = Math.min(lane.durationSecs, viewEnd + pad);
    // Hard cap decode length so we never walk multi-minute ranges at high res.
    if (r1 - r0 > MAX_DETAIL_RANGE_SECS) {
      const mid = (viewStart + viewEnd) / 2;
      r0 = Math.max(0, mid - MAX_DETAIL_RANGE_SECS / 2);
      r1 = Math.min(lane.durationSecs, r0 + MAX_DETAIL_RANGE_SECS);
    }
    if (r1 - r0 < 0.08) return;

    // Skip if we'd re-fetch almost the same window that's already ready.
    if (lane.detail && densityOk(lane.detail, viewDur, width)) {
      const overlap =
        Math.min(lane.detail.end, r1) - Math.max(lane.detail.start, r0);
      if (
        overlap > (r1 - r0) * 0.85 &&
        hasHealthyMargin(lane.detail, viewStart, viewEnd, lane.durationSecs)
      ) {
        return;
      }
    }

    const bins = Math.max(
      400,
      Math.min(DETAIL_BINS_CAP, Math.round(width * 2.2)),
    );
    const base = cueBase(track);
    void fetchPeaksAbs(
      track,
      loadGen,
      id,
      base + r0,
      base + r1,
      bins,
      "detail",
    );
  }

  function scheduleLodRefresh() {
    if (lodTimer) clearTimeout(lodTimer);
    lodTimer = setTimeout(() => {
      lodTimer = null;
      // LMB track-drag: wait until release. Scroll pan is fine to prefetch during.
      if (drag) {
        scheduleLodRefresh();
        return;
      }
      ensureDetail("from");
      ensureDetail("to");
    }, LOD_DEBOUNCE_MS);
  }

  async function applyPayload(payload: MixTransitionOpenPayload) {
    if (!payload?.from?.path || !payload?.to?.path) return;
    loadGen += 1;
    const gen = loadGen;
    waveReqFrom += 1;
    waveReqTo += 1;
    suppressMemorySave = true;
    if (lodTimer) {
      clearTimeout(lodTimer);
      lodTimer = null;
    }
    if (memoryTimer) {
      clearTimeout(memoryTimer);
      memoryTimer = null;
    }
    from = payload.from;
    to = payload.to;
    playlistId = payload.playlistId;
    fromIndex = payload.fromIndex;
    pxPerSec = DEFAULT_PX_PER_SEC;
    playheadFromSecs = 0;
    fromLane = { ...emptyLane(), loading: true };
    toLane = { ...emptyLane(), loading: true };
    drag = null;
    playheadDrag = null;
    blockInteract = null;
    paletteDrag = null;
    blocks = [];
    selectedBlockId = null;
    expandedBlockId = null;
    blockError = null;
    clearUndoHistory();
    previewSession = null;
    previewing = false;
    previewError = null;

    const mem = loadMixTransitionMemory(
      payload.playlistId,
      payload.from.path,
      payload.to.path,
    );

    try {
      await invoke("player_init");
    } catch {
      /* main may already own BASS */
    }

    await Promise.all([
      loadWaveformOverview(payload.from, gen, "from"),
      loadWaveformOverview(payload.to, gen, "to"),
      loadBpm(payload.from, gen, "from"),
      loadBpm(payload.to, gen, "to"),
    ]);

    if (gen !== loadGen) return;
    // Wait a frame so wraps have real width before placing views.
    requestAnimationFrame(() => {
      if (gen !== loadGen) return;
      if (mem) {
        applyMemoryViews(mem);
      } else {
        applyDefaultViews();
        // First open for this edge — snap grids to kicks when BPM is known.
        if (fromLane.bpm) void alignGridToKick("from", true);
        if (toLane.bpm) void alignGridToKick("to", true);
      }
      suppressMemorySave = false;
      redrawAll();
      scheduleLodRefresh();
    });
  }

  function accentColor(): string {
    if (typeof document === "undefined") return "#7c5cff";
    const v = getComputedStyle(document.documentElement)
      .getPropertyValue("--accent")
      .trim();
    return v || "#7c5cff";
  }

  function peakAt(buf: PeakBuf, t: number): number {
    const peaks = buf.peaks;
    if (!peaks || peaks.length === 0) return 0;
    const span = buf.end - buf.start;
    if (span <= 0) return 0;
    if (t < buf.start || t > buf.end) return 0;
    const u = (t - buf.start) / span;
    const idx = u * (peaks.length - 1);
    const i0 = Math.floor(idx);
    const i1 = Math.min(peaks.length - 1, i0 + 1);
    const f = idx - i0;
    return peaks[i0]! * (1 - f) + peaks[i1]! * f;
  }

  function speedBlocksForLane(id: LaneId): MixBlock[] {
    return blocks
      .filter((b) => b.kind === "speed" && (b.targetLane ?? "from") === id)
      .sort((a, b) => a.startFromSecs - b.startFromSecs);
  }

  function buildSpeedWarpTable(b: MixBlock): number[] {
    const steps = 48;
    const envelope = b.params.envelope;
    const curve = normalizeCurve(b.params.curve);
    const table = new Array<number>(steps + 1);
    table[0] = 0;
    let acc = 0;
    for (let i = 1; i <= steps; i++) {
      const u0 = (i - 1) / steps;
      const u1 = i / steps;
      const r0 = envelopeVToRate(sampleEnvelope(envelope, u0, curve));
      const r1 = envelopeVToRate(sampleEnvelope(envelope, u1, curve));
      acc += ((r0 + r1) / 2) * (u1 - u0) * b.durationSecs;
      table[i] = acc;
    }
    return table;
  }

  function sourceConsumedInBlock(
    b: MixBlock,
    table: number[],
    localT: number,
  ): number {
    const steps = table.length - 1;
    const u = Math.max(0, Math.min(1, localT / b.durationSecs));
    const pos = u * steps;
    const i0 = Math.floor(pos);
    const i1 = Math.min(steps, i0 + 1);
    const frac = pos - i0;
    const v0 = table[i0]!;
    const v1 = table[i1]!;
    return v0 + (v1 - v0) * frac;
  }

  function makeLaneWarp(id: LaneId): (wallT: number) => number {
    const list = speedBlocksForLane(id);
    if (list.length === 0) return () => 0;
    const tables = list.map((b) => buildSpeedWarpTable(b));
    return (wallT: number) => {
      let delta = 0;
      for (let i = 0; i < list.length; i++) {
        const b = list[i]!;
        if (wallT <= b.startFromSecs) break;
        const end = blockEnd(b);
        if (wallT >= end) {
          delta += tables[i]![tables[i]!.length - 1]! - b.durationSecs;
        } else {
          const local = wallT - b.startFromSecs;
          delta += sourceConsumedInBlock(b, tables[i]!, local) - local;
        }
      }
      return delta;
    };
  }

  function drawLane(id: LaneId) {
    const canvas = canvasOf(id);
    const wrap = wrapOf(id);
    const lane = laneOf(id);
    if (!canvas || !wrap) return;

    const cssW = wrap.clientWidth;
    const cssH = wrap.clientHeight;
    if (cssW <= 0 || cssH <= 0) return;

    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.max(1, Math.floor(cssW * dpr));
    canvas.height = Math.max(1, Math.floor(cssH * dpr));
    canvas.style.width = `${cssW}px`;
    canvas.style.height = `${cssH}px`;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);

    const viewStart = lane.viewStartSecs;
    const viewDur = cssW / pxPerSec;
    const viewEnd = viewStart + viewDur;
    const muted = id === "to";
    const accent = accentColor();
    const mid = cssH / 2;

    // ── Beat grid ──────────────────────────────────────────────────────────
    const bpm = lane.bpm;
    if (bpm != null && bpm > 0) {
      const beatSec = 60 / bpm;
      // Don't draw denser than ~4px — collapse to every Nth beat.
      let stepBeats = 1;
      while (beatSec * stepBeats * pxPerSec < 4) stepBeats *= 2;

      const firstBeat = Math.floor(
        (viewStart - lane.gridOffsetSecs) / beatSec,
      );
      const lastBeat = Math.ceil((viewEnd - lane.gridOffsetSecs) / beatSec);

      for (let b = firstBeat; b <= lastBeat; b += stepBeats) {
        const t = lane.gridOffsetSecs + b * beatSec;
        if (t < viewStart - 0.001 || t > viewEnd + 0.001) continue;
        const x = (t - viewStart) * pxPerSec;
        const isBar = b % BEATS_PER_BAR === 0;
        const isDownbeat = isBar && b % (BEATS_PER_BAR * 4) === 0;

        if (isDownbeat) {
          ctx.strokeStyle = muted
            ? "rgba(200, 190, 255, 0.28)"
            : "rgba(255, 255, 255, 0.22)";
          ctx.lineWidth = 1.5;
        } else if (isBar) {
          ctx.strokeStyle = muted
            ? "rgba(180, 170, 230, 0.18)"
            : "rgba(255, 255, 255, 0.14)";
          ctx.lineWidth = 1;
        } else {
          ctx.strokeStyle = muted
            ? "rgba(160, 150, 210, 0.08)"
            : "rgba(255, 255, 255, 0.06)";
          ctx.lineWidth = 1;
        }
        ctx.beginPath();
        ctx.moveTo(x + 0.5, 0);
        ctx.lineTo(x + 0.5, cssH);
        ctx.stroke();
      }
    }

    // Center guide
    ctx.strokeStyle = "rgba(255,255,255,0.05)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(0, mid);
    ctx.lineTo(cssW, mid);
    ctx.stroke();

    // Track bounds (start / end markers)
    if (lane.durationSecs > 0) {
      for (const edge of [0, lane.durationSecs]) {
        if (edge < viewStart || edge > viewEnd) continue;
        const x = (edge - viewStart) * pxPerSec;
        ctx.strokeStyle = "rgba(255,255,255,0.2)";
        ctx.setLineDash([3, 3]);
        ctx.beginPath();
        ctx.moveTo(x + 0.5, 0);
        ctx.lineTo(x + 0.5, cssH);
        ctx.stroke();
        ctx.setLineDash([]);
      }
    }

    // ── Waveform ───────────────────────────────────────────────────────────
    if (
      lane.durationSecs <= 0 ||
      ((!lane.overview || lane.overview.peaks.length === 0) &&
        (!lane.detail || lane.detail.peaks.length === 0))
    ) {
      return;
    }

    const maxAmp = cssH * 0.42;
    const cols = Math.max(1, Math.ceil(cssW));
    const top: number[] = new Array(cols);
    const bot: number[] = new Array(cols);

    const wallOffset =
      id === "to" ? toLane.viewStartSecs - fromLane.viewStartSecs : 0;
    const warp = makeLaneWarp(id);

    for (let col = 0; col < cols; col++) {
      const t0 = viewStart + (col / cols) * viewDur;
      const t1 = viewStart + ((col + 1) / cols) * viewDur;
      let amp = 0;
      const steps = 4;
      for (let s = 0; s < steps; s++) {
        const t = t0 + ((s + 0.5) / steps) * (t1 - t0);
        const wallT = t - wallOffset;
        const tWarped = t + warp(wallT);
        amp = Math.max(amp, sampleLaneAmp(lane, tWarped, viewDur));
      }
      const a = Math.max(0.4, amp * maxAmp);
      top[col] = mid - a;
      bot[col] = mid + a;
    }

    ctx.fillStyle = muted ? "rgba(160, 150, 210, 0.7)" : accent;
    ctx.beginPath();
    ctx.moveTo(0, mid);
    for (let col = 0; col < cols; col++) {
      ctx.lineTo(col + 0.5, top[col]!);
    }
    for (let col = cols - 1; col >= 0; col--) {
      ctx.lineTo(col + 0.5, bot[col]!);
    }
    ctx.closePath();
    ctx.globalAlpha = 0.9;
    ctx.fill();
    ctx.globalAlpha = 1;

    ctx.strokeStyle = muted
      ? "rgba(220, 220, 240, 0.35)"
      : "rgba(255,255,255,0.3)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let col = 0; col < cols; col++) {
      if (col === 0) ctx.moveTo(col + 0.5, top[col]!);
      else ctx.lineTo(col + 0.5, top[col]!);
    }
    ctx.stroke();
  }

  function redrawAll() {
    drawLane("from");
    drawLane("to");
  }

  $effect(() => {
    void blocks;
    void fromLane.overview;
    void fromLane.detail;
    void fromLane.viewStartSecs;
    void fromLane.bpm;
    void fromLane.gridOffsetSecs;
    void fromLane.durationSecs;
    void fromLane.refining;
    void toLane.overview;
    void toLane.detail;
    void toLane.viewStartSecs;
    void toLane.bpm;
    void toLane.gridOffsetSecs;
    void toLane.durationSecs;
    void toLane.refining;
    void pxPerSec;
    void fromCanvas;
    void toCanvas;
    redrawAll();
  });

  // Zoomed-in only: schedule a short high-res window. Zoom-out is free (overview).
  $effect(() => {
    void fromLane.viewStartSecs;
    void toLane.viewStartSecs;
    void fromLane.durationSecs;
    void toLane.durationSecs;
    void pxPerSec;
    void fromWrap;
    void toWrap;
    if (!from && !to) return;
    scheduleLodRefresh();
  });

  $effect(() => {
    const a = fromWrap;
    const b = toWrap;
    if (!a && !b) return;
    const ro = new ResizeObserver(() => {
      redrawAll();
      scheduleLodRefresh();
    });
    if (a) ro.observe(a);
    if (b) ro.observe(b);
    return () => ro.disconnect();
  });

  // ── Zoom / pan interactions ──────────────────────────────────────────────

  /**
   * Shared zoom that keeps both lanes pinned on screen.
   * Same horizontal fraction (cursor / center) locks the track time under that
   * pixel on each strip — so the transition alignment doesn't drift.
   */
  function zoomShared(
    factor: number,
    opts?: { clientX?: number; anchorId?: LaneId },
  ) {
    const next = Math.min(
      MAX_PX_PER_SEC,
      Math.max(MIN_PX_PER_SEC, pxPerSec * factor),
    );
    if (Math.abs(next - pxPerSec) < 0.01) return;

    const oldPx = pxPerSec;
    const anchorId = opts?.anchorId ?? "from";
    const anchorWrap = wrapOf(anchorId) ?? fromWrap ?? toWrap;

    // Screen fraction of the zoom pivot (0 = left, 0.5 = center, 1 = right).
    let frac = 0.5;
    if (anchorWrap && opts?.clientX != null) {
      const rect = anchorWrap.getBoundingClientRect();
      if (rect.width > 0) {
        frac = Math.max(
          0,
          Math.min(1, (opts.clientX - rect.left) / rect.width),
        );
      }
    }

    // Capture the audio time currently under that fraction on each lane.
    function timeAtFrac(id: LaneId): number | null {
      const lane = laneOf(id);
      const wrap = wrapOf(id);
      if (!wrap || lane.durationSecs <= 0) return null;
      const w = wrap.clientWidth;
      if (w <= 0) return null;
      return lane.viewStartSecs + (frac * w) / oldPx;
    }

    const tFrom = timeAtFrac("from");
    const tTo = timeAtFrac("to");

    pxPerSec = next;

    function viewStartFor(id: LaneId, t: number | null): number | null {
      if (t == null) return null;
      const wrap = wrapOf(id);
      if (!wrap) return null;
      const w = wrap.clientWidth;
      if (w <= 0) return null;
      return t - (frac * w) / pxPerSec;
    }

    const fromStart = viewStartFor("from", tFrom);
    const toStart = viewStartFor("to", tTo);

    if (fromStart != null) {
      fromLane = { ...fromLane, viewStartSecs: fromStart };
    }
    if (toStart != null) {
      toLane = { ...toLane, viewStartSecs: toStart };
    }
    scheduleMemorySave();
  }

  /** Beat / bar / edge markers for snap (seconds into the track). */
  function collectMarkers(
    lane: LaneState,
    tMin: number,
    tMax: number,
  ): { t: number; bar: boolean }[] {
    const out: { t: number; bar: boolean }[] = [];
    const push = (t: number, bar: boolean) => {
      if (!Number.isFinite(t)) return;
      // Dedup near-identical times.
      const last = out[out.length - 1];
      if (last && Math.abs(last.t - t) < 1e-4) {
        last.bar = last.bar || bar;
        return;
      }
      out.push({ t, bar });
    };

    push(0, true);
    if (lane.durationSecs > 0) push(lane.durationSecs, true);

    if (lane.bpm != null && lane.bpm > 0) {
      const beat = 60 / lane.bpm;
      if (beat > 1e-4) {
        let n = Math.floor((tMin - lane.gridOffsetSecs) / beat) - 2;
        const nMax = Math.ceil((tMax - lane.gridOffsetSecs) / beat) + 2;
        const limit = n + 800;
        for (; n <= nMax && n < limit; n++) {
          const t = lane.gridOffsetSecs + n * beat;
          if (t < -1 || (lane.durationSecs > 0 && t > lane.durationSecs + 1))
            continue;
          const bar = ((n % BEATS_PER_BAR) + BEATS_PER_BAR) % BEATS_PER_BAR === 0;
          push(t, bar);
        }
      }
    }

    out.sort((a, b) => a.t - b.t);
    return out;
  }

  function nearestMarker(
    marks: { t: number; bar: boolean }[],
    target: number,
  ): { t: number; bar: boolean } | null {
    if (marks.length === 0) return null;
    let lo = 0;
    let hi = marks.length - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (marks[mid]!.t < target) lo = mid + 1;
      else hi = mid;
    }
    let best = marks[lo]!;
    if (lo > 0) {
      const prev = marks[lo - 1]!;
      if (Math.abs(prev.t - target) < Math.abs(best.t - target)) best = prev;
    }
    if (lo + 1 < marks.length) {
      const next = marks[lo + 1]!;
      if (Math.abs(next.t - target) < Math.abs(best.t - target)) best = next;
    }
    return best;
  }

  /**
   * Snap so a grid stick on the dragged track lands on a stick of the other
   * track (same screen X). Works mid-gap too: we don't require a stick under
   * the cursor — only that some nearby self stick is within SNAP_PX of some
   * other stick on screen.
   *
   * For each self stick S near the grab:
   *   O = other stick currently nearest to S's screen column
   *   if |x(S) − x(O)| ≤ SNAP_PX → candidate
   * Prefer bar↔bar, then smallest gap, then stick closest to cursor.
   */
  function snapViewStart(
    id: LaneId,
    raw: number,
    clientX: number,
    enabled: boolean,
  ): { viewStart: number; snapped: boolean } {
    if (!enabled || pxPerSec <= 0) return { viewStart: raw, snapped: false };

    const self = laneOf(id);
    const otherId: LaneId = id === "from" ? "to" : "from";
    const other = laneOf(otherId);
    const selfWrap = wrapOf(id);
    const otherWrap = wrapOf(otherId);
    if (
      self.durationSecs <= 0 ||
      other.durationSecs <= 0 ||
      !selfWrap ||
      !otherWrap
    ) {
      return { viewStart: raw, snapped: false };
    }

    const selfRect = selfWrap.getBoundingClientRect();
    const otherRect = otherWrap.getBoundingClientRect();
    if (selfRect.width <= 0 || otherRect.width <= 0) {
      return { viewStart: raw, snapped: false };
    }

    // Cursor X in each strip (lanes are stacked, same width — clamp to strip).
    const xSelf = Math.max(
      0,
      Math.min(selfRect.width, clientX - selfRect.left),
    );
    const xOther = Math.max(
      0,
      Math.min(otherRect.width, clientX - otherRect.left),
    );

    // With the raw (unsnapped) offset, what audio times sit under the cursor?
    const tSelfUnder = raw + xSelf / pxPerSec;
    const tOtherUnder = other.viewStartSecs + xOther / pxPerSec;

    // Search at least half a beat so a grab between sticks still sees both
    // neighbors; also cover a few SNAP_PX of screen so zoom-out still works.
    const selfBeat =
      self.bpm != null && self.bpm > 0 ? 60 / self.bpm : 0.5;
    const searchSecs = Math.max(
      selfBeat * 0.55,
      (SNAP_PX * 4) / pxPerSec,
      0.05,
    );
    const pad = Math.max(searchSecs * 2, 2);
    const selfMarks = collectMarkers(
      self,
      tSelfUnder - pad,
      tSelfUnder + pad,
    );
    const otherMarks = collectMarkers(
      other,
      tOtherUnder - pad * 2,
      tOtherUnder + pad * 2,
    );
    if (selfMarks.length === 0 || otherMarks.length === 0) {
      return { viewStart: raw, snapped: false };
    }

    type Cand = {
      s: { t: number; bar: boolean };
      o: { t: number; bar: boolean };
      gapPx: number;
      distCursorPx: number;
    };
    let best: Cand | null = null;

    for (const s of selfMarks) {
      // Only consider sticks near the grab point (mid-gap ≈ half beat away).
      const distCursorPx = Math.abs((s.t - tSelfUnder) * pxPerSec);
      if (Math.abs(s.t - tSelfUnder) > searchSecs) continue;

      // Other-track time that currently shares S's screen column.
      const tOtherAtS = other.viewStartSecs + (s.t - raw);
      const o = nearestMarker(otherMarks, tOtherAtS);
      if (!o) continue;

      const gapPx =
        Math.abs(s.t - raw - (o.t - other.viewStartSecs)) * pxPerSec;
      if (gapPx > SNAP_PX) continue;

      const cand: Cand = { s, o, gapPx, distCursorPx };
      if (!best) {
        best = cand;
        continue;
      }
      const bestBars = (best.s.bar ? 1 : 0) + (best.o.bar ? 1 : 0);
      const candBars = (cand.s.bar ? 1 : 0) + (cand.o.bar ? 1 : 0);
      if (candBars > bestBars) {
        best = cand;
        continue;
      }
      if (candBars < bestBars) continue;
      if (
        cand.gapPx < best.gapPx - 0.5 ||
        (Math.abs(cand.gapPx - best.gapPx) <= 0.5 &&
          cand.distCursorPx < best.distCursorPx)
      ) {
        best = cand;
      }
    }

    if (!best) return { viewStart: raw, snapped: false };

    // Put S exactly on O's screen column:
    // viewStart' = other.viewStart + s.t - o.t
    const snappedStart = other.viewStartSecs + best.s.t - best.o.t;
    return { viewStart: snappedStart, snapped: true };
  }

  /**
   * Scroll-to-pan for the transition view — both lanes move together so
   * alignment stays intact. Individual offsets only change via LMB drag.
   */
  function panBoth(deltaPx: number) {
    const dt = -deltaPx / pxPerSec;
    if (fromLane.durationSecs > 0) {
      fromLane = {
        ...fromLane,
        viewStartSecs: fromLane.viewStartSecs + dt,
      };
    }
    if (toLane.durationSecs > 0) {
      toLane = {
        ...toLane,
        viewStartSecs: toLane.viewStartSecs + dt,
      };
    }
    scheduleMemorySave();
  }

  function onWheel(e: WheelEvent, id: LaneId) {
    // Ctrl/Cmd + wheel → zoom (shared scale, both lanes stay put).
    // Plain wheel → pan the whole view (both lanes).
    // stopPropagation: stage/blocks-layer may also listen so we don't double-fire.
    e.preventDefault();
    e.stopPropagation();
    if (e.ctrlKey || e.metaKey) {
      const direction = e.deltaY > 0 ? 1 / 1.12 : 1.12;
      zoomShared(direction, { clientX: e.clientX, anchorId: id });
      return;
    }
    // Prefer horizontal delta when present (trackpads).
    const dx =
      Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
    panBoth(-dx);
  }

  /**
   * Wheel over unpinned blocks never hits the wave wraps (blocks sit on top).
   * Route pan/zoom the same way regardless of pin state.
   */
  function onBlocksLayerWheel(e: WheelEvent) {
    const lane: LaneId =
      targetLaneAtClientY(e.clientY) === "to" ? "to" : "from";
    onWheel(e, lane);
  }

  function onPointerDown(e: PointerEvent, id: LaneId) {
    if (e.button !== 0) return;
    // Don't steal playhead handle.
    if ((e.target as HTMLElement)?.closest?.(".mix-playhead")) return;
    const wrap = wrapOf(id);
    if (!wrap) return;
    wrap.setPointerCapture(e.pointerId);
    drag = {
      lane: id,
      pointerId: e.pointerId,
      startX: e.clientX,
      startView: laneOf(id).viewStartSecs,
      active: false,
      snapped: false,
    };
  }

  function onPointerMove(e: PointerEvent, id: LaneId) {
    if (!drag || drag.lane !== id || drag.pointerId !== e.pointerId) return;
    const dx = e.clientX - drag.startX;
    if (!drag.active) {
      if (Math.abs(dx) < LANE_PAN_THRESHOLD) return;
      drag = { ...drag, active: true };
    }
    const raw = drag.startView - dx / pxPerSec;
    // Alt = free drag without magnet.
    const { viewStart, snapped } = snapViewStart(
      id,
      raw,
      e.clientX,
      !e.altKey,
    );
    setLane(id, { viewStartSecs: viewStart });
    drag = { ...drag, snapped };
  }

  function onPointerUp(e: PointerEvent, id: LaneId) {
    if (!drag || drag.lane !== id || drag.pointerId !== e.pointerId) return;
    const wrap = wrapOf(id);
    const wasPan = drag.active;
    // Tap (no pan):
    //   Alt → put a beat stick under the cursor (align grid to that kick)
    //   plain → place playhead
    if (!wasPan) {
      if (e.altKey && laneOf(id).bpm != null) {
        const t = timeAtClientX(e.clientX);
        if (t != null) alignGridToTime(id, t);
      } else {
        setPlayheadFromClientX(e.clientX);
      }
    } else if (!e.altKey) {
      // Final snap on release using cursor position.
      const dx = e.clientX - drag.startX;
      const raw = drag.startView - dx / pxPerSec;
      const { viewStart } = snapViewStart(id, raw, e.clientX, true);
      setLane(id, { viewStartSecs: viewStart });
      scheduleMemorySave();
    } else {
      scheduleMemorySave();
    }
    try {
      wrap?.releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    drag = null;
  }

  function resetView(id: LaneId) {
    const wrap = wrapOf(id);
    const lane = laneOf(id);
    if (!wrap || lane.durationSecs <= 0) return;
    const vp = viewportSecsFor(wrap);
    if (id === "from") {
      setLane(id, {
        viewStartSecs: lane.durationSecs - vp * PREV_END_FRACTION,
      });
    } else {
      setLane(id, { viewStartSecs: 0 });
    }
  }

  // ── Playhead + preview ───────────────────────────────────────────────────
  //
  // Playhead = cut marker as time on previous track (draggable, view-clamped).
  // Alignment offset maps it onto next:
  //   t_next = t_prev + (to.viewStart - from.viewStart)
  // No fixed center guide — you place the playhead wherever you want.

  function audioPathFor(track: MusicFile): string {
    if (track.audio_path?.trim()) return track.audio_path.trim();
    const marker = track.path.lastIndexOf("#cue:");
    if (marker > 0) return track.path.slice(0, marker);
    return track.path;
  }

  /** Track-relative seconds → absolute file seconds (CUE segments offset). */
  function absTime(track: MusicFile, rel: number): number {
    const base =
      typeof track.cue_start_secs === "number" &&
      Number.isFinite(track.cue_start_secs)
        ? track.cue_start_secs
        : 0;
    return base + Math.max(0, rel);
  }

  function mapPrevToNext(tPrev: number): number {
    return tPrev + (toLane.viewStartSecs - fromLane.viewStartSecs);
  }

  function setPlayheadFromClientX(clientX: number) {
    if (!fromWrap || pxPerSec <= 0) return;
    const rect = fromWrap.getBoundingClientRect();
    // Clamp to the *visible strip only* — not to prev-track duration.
    // Time past prev end is the gap / silence region on the mix timeline.
    const x = Math.max(0, Math.min(rect.width, clientX - rect.left));
    const t = fromLane.viewStartSecs + x / pxPerSec;
    playheadFromSecs = Math.max(0, t);
    scheduleMemorySave();
  }

  function onPlayheadPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    playheadDrag = { pointerId: e.pointerId, grabX: e.clientX };
    setPlayheadFromClientX(e.clientX);
  }

  function onPlayheadPointerMove(e: PointerEvent) {
    if (!playheadDrag || playheadDrag.pointerId !== e.pointerId) return;
    setPlayheadFromClientX(e.clientX);
  }

  function onPlayheadPointerUp(e: PointerEvent) {
    if (!playheadDrag || playheadDrag.pointerId !== e.pointerId) return;
    const target = e.currentTarget as HTMLElement;
    try {
      target.releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    playheadDrag = null;
    scheduleMemorySave();
  }

  /**
   * Timeline plan from the graph (no clamp that ruins offset):
   * at playhead: t_prev = playhead, t_next = playhead + (to.viewStart - from.viewStart)
   * If t_next < 0 → next starts later (delay). If t_next past end → no next yet.
   *
   * Transition block (if any) only limits *where mix automation runs* (fromDuration /
   * cue end). Without a transition block the playhead is free and preview can run
   * to the natural end of the tracks — no artificial seekbar/zone clamp.
   */
  function buildPreviewPlan(): {
    fromStartRel: number | null;
    fromDurationSecs: number;
    toStartRel: number | null;
    toDelaySecs: number;
    toEndAbs: number | null;
  } | null {
    if (!from || !to) return null;
    if (fromLane.durationSecs <= 0.05 && toLane.durationSecs <= 0.05) return null;

    const zone = primaryTransition(blocks);
    // Start exactly where the playhead is (free). Only if a transition zone exists
    // *and* the playhead sits outside it, snap the *preview start* into the zone
    // so Preview still makes sense — the on-graph playhead itself is not clamped
    // when placing/scrubbing.
    let tFrom = playheadFromSecs;
    let zoneEnd: number | null = null;
    if (zone) {
      const zEnd = blockEnd(zone);
      zoneEnd = zEnd;
      if (tFrom < zone.startFromSecs) tFrom = zone.startFromSecs;
      if (tFrom >= zEnd - 0.02) tFrom = Math.max(zone.startFromSecs, zEnd - 0.05);
    }

    const tTo = mapPrevToNext(tFrom);

    let fromStartRel: number | null = null;
    let fromDurationSecs = 0;
    if (tFrom < fromLane.durationSecs) {
      fromStartRel = Math.max(0, tFrom);
      // No transition block → play to the end of prev (and next follows graph).
      // With a zone → stop prev at zone end so DSP only covers the transition.
      const naturalEnd =
        zoneEnd != null
          ? Math.min(fromLane.durationSecs, zoneEnd)
          : fromLane.durationSecs;
      fromDurationSecs = Math.max(0.05, naturalEnd - fromStartRel);
    }

    let toStartRel: number | null = null;
    let toDelaySecs = 0;
    if (tTo < toLane.durationSecs) {
      if (tTo < 0) {
        // Next is still to the right of the playhead on the graph.
        toDelaySecs = -tTo;
        toStartRel = 0;
      } else {
        toStartRel = tTo;
        toDelaySecs = 0;
      }
    }

    if (fromStartRel == null && toStartRel == null) return null;

    let toEndAbs: number | null =
      typeof to.cue_end_secs === "number" && Number.isFinite(to.cue_end_secs)
        ? to.cue_end_secs
        : null;

    // Only when a transition zone exists: cap next cue end to the mapped zone end.
    // Without a zone — leave next free to its natural/CUE end.
    if (zone && zoneEnd != null && toStartRel != null) {
      const toZoneEndRel = mapPrevToNext(zoneEnd);
      if (toZoneEndRel > 0) {
        const absEnd = absTime(to, Math.min(toLane.durationSecs, toZoneEndRel));
        toEndAbs =
          toEndAbs != null ? Math.min(toEndAbs, absEnd) : absEnd;
      }
    }

    return {
      fromStartRel,
      fromDurationSecs,
      toStartRel,
      toDelaySecs,
      toEndAbs,
    };
  }

  let previewHint = $derived.by(() => {
    const plan = buildPreviewPlan();
    if (!plan) return null;
    const zone = primaryTransition(blocks);
    const prev =
      plan.fromStartRel != null
        ? `prev ${formatTime(plan.fromStartRel)}`
        : "prev —";
    const next =
      plan.toStartRel != null
        ? plan.toDelaySecs > 0.05
          ? `next in ${plan.toDelaySecs.toFixed(1)}s @ ${formatTime(plan.toStartRel)}`
          : `next ${formatTime(plan.toStartRel)}`
        : "next —";
    const zoneLabel = zone
      ? ` · zone ${formatTime(zone.durationSecs)}`
      : "";
    return `${prev} · ${next}${zoneLabel}`;
  });

  async function stopPreview() {
    previewGen += 1;
    previewing = false;
    previewBusy = false;
    previewSession = null;
    try {
      await invoke("player_stop");
    } catch {
      /* ignore */
    }
    scheduleMemorySave();
  }

  async function startPreview() {
    const plan = buildPreviewPlan();
    if (!from || !to || !plan) {
      previewError = "Need both tracks loaded to preview";
      return;
    }

    previewError = null;
    previewBusy = true;
    const gen = ++previewGen;

    try {
      await invoke("player_stop");
    } catch {
      /* ignore */
    }

    try {
      try {
        await invoke("player_init");
      } catch {
        /* main may own BASS */
      }

      // Exact graph times — do NOT clamp next into 0 when it's still off to the right.
      const fromCueStart =
        plan.fromStartRel != null ? absTime(from, plan.fromStartRel) : null;
      const fromCueEnd =
        plan.fromStartRel != null
          ? absTime(from, plan.fromStartRel + plan.fromDurationSecs)
          : null;
      const toCueStart =
        plan.toStartRel != null ? absTime(to, plan.toStartRel) : null;

      // Automation blocks → mix clock (t=0 at preview start), per deck.
      const mixOrigin =
        plan.fromStartRel ??
        (plan.toStartRel != null
          ? plan.toStartRel - (toLane.viewStartSecs - fromLane.viewStartSecs)
          : playheadFromSecs);
      const collectEnv = (
        lane: "from" | "to",
        kind: "volume" | "lowpass" | "highpass" | "speed",
      ) =>
        blocks
          .filter(
            (b) =>
              b.kind === kind &&
              (b.targetLane ?? "from") === lane &&
              b.durationSecs > 0.02,
          )
          .map((b) => ({
            startSecs: b.startFromSecs - mixOrigin,
            durationSecs: b.durationSecs,
            curve: normalizeCurve(b.params.curve),
            points: normalizeEnvelope(b.params.envelope).map((p) => ({
              t: p.t,
              v: p.v,
            })),
          }));

      await invoke("player_mix_crossfade", {
        fromPath: from.path,
        fromAudioPath: audioPathFor(from),
        fromCueStart,
        fromCueEnd,
        toPath: to.path,
        toAudioPath: audioPathFor(to),
        toCueStart,
        toCueEnd: plan.toEndAbs ?? undefined,
        toDelaySecs: plan.toDelaySecs,
        fromDurationSecs: plan.fromDurationSecs,
        fromVol: collectEnv("from", "volume"),
        toVol: collectEnv("to", "volume"),
        fromLp: collectEnv("from", "lowpass"),
        fromHp: collectEnv("from", "highpass"),
        toLp: collectEnv("to", "lowpass"),
        toHp: collectEnv("to", "highpass"),
        fromSpeed: collectEnv("from", "speed"),
        toSpeed: collectEnv("to", "speed"),
      });
      if (gen !== previewGen) return;
      previewing = true;
      previewSession = {
        fromStartRel: plan.fromStartRel ?? playheadFromSecs,
        toStartRel: plan.toStartRel ?? 0,
        crossfadeSecs: plan.fromDurationSecs,
        wallStartMs: performance.now(),
      };
    } catch (e) {
      if (gen !== previewGen) return;
      previewing = false;
      previewSession = null;
      previewError = (typeof e === "string" ? e : String(e)).replace(
        /^Error:\s*/i,
        "",
      );
    } finally {
      if (gen === previewGen) previewBusy = false;
    }
  }

  // Playhead = mix timeline at 1× wall-clock (not content rate).
  // Speed blocks only retune *audio* so one deck can match the other;
  // the graph/playhead stay on the layout clock.
  $effect(() => {
    if (!previewing || !previewSession) return;
    const session = previewSession;
    const startOrigin = session.fromStartRel;
    let raf = 0;
    const tick = () => {
      if (!previewing || !previewSession) return;
      const elapsed = (performance.now() - session.wallStartMs) / 1000;
      playheadFromSecs = startOrigin + elapsed;
      followPlayheadInView();
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  async function togglePreview() {
    if (previewBusy) return;
    if (previewing) await stopPreview();
    else await startPreview();
  }

  function handleKeydown(e: KeyboardEvent) {
    const t = e.target;
    const typing =
      t instanceof HTMLInputElement ||
      t instanceof HTMLTextAreaElement ||
      (t instanceof HTMLElement && t.isContentEditable);

    // Ctrl+Z undo · Ctrl+Shift+Z / Ctrl+Y redo — use e.code (physical key),
    // not e.key, so RU/other layouts still work (KeyZ stays Z-row, not "я").
    if ((e.ctrlKey || e.metaKey) && !typing) {
      if (e.code === "KeyZ" && !e.shiftKey) {
        e.preventDefault();
        undo();
        return;
      }
      if ((e.code === "KeyZ" && e.shiftKey) || e.code === "KeyY") {
        e.preventDefault();
        redo();
        return;
      }
    }

    if (e.key === "Escape") {
      if (previewing) {
        e.preventDefault();
        void stopPreview();
        return;
      }
      if (selectedBlockId) {
        e.preventDefault();
        selectedBlockId = null;
        return;
      }
      void appWindow.close();
      return;
    }
    if (
      (e.key === "Delete" || e.key === "Backspace") &&
      selectedBlockId &&
      !previewing &&
      !typing
    ) {
      e.preventDefault();
      removeBlock(selectedBlockId);
      return;
    }
    // Space — preview (don't steal when typing in future fields).
    if ((e.key === " " || e.code === "Space") && !typing) {
      e.preventDefault();
      void togglePreview();
      return;
    }
    // +/- zoom — both lanes stay aligned.
    if (e.key === "=" || e.key === "+") {
      e.preventDefault();
      zoomShared(1.15);
    } else if (e.key === "-" || e.key === "_") {
      e.preventDefault();
      zoomShared(1 / 1.15);
    } else if (e.key === "0" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      pxPerSec = DEFAULT_PX_PER_SEC;
      applyDefaultViews();
    }
  }

  onMount(() => {
    let unlistenOpen: (() => void) | undefined;
    let unlistenResize: (() => void) | undefined;
    let unlistenEnded: (() => void) | undefined;
    let unlistenChanged: (() => void) | undefined;

    const pending = takePendingMixTransition();
    if (pending) void applyPayload(pending);

    void listen<MixTransitionOpenPayload>("mix-transition:open", (event) => {
      void stopPreview();
      void applyPayload(event.payload);
    }).then((fn) => {
      unlistenOpen = fn;
    });

    void appWindow
      .onResized(() => {
        redrawAll();
      })
      .then((fn) => {
        unlistenResize = fn;
      });

    void listen("player:track-ended", () => {
      if (!previewing) return;
      // Natural end of the last surviving deck — UI only; backend already
      // released the stream (and clears mix-preview ownership).
      previewing = false;
      previewBusy = false;
      previewSession = null;
      scheduleMemorySave();
    }).then((fn) => {
      unlistenEnded = fn;
    });

    // When previous deck ends mid-mix, backend hands off to next and emits
    // track-changed — keep previewing until that deck ends or user stops.
    void listen<{ path?: string }>("player:track-changed", () => {
      /* mix preview keeps running on the incoming deck */
    }).then((fn) => {
      unlistenChanged = fn;
    });

    return () => {
      unlistenOpen?.();
      unlistenResize?.();
      unlistenEnded?.();
      unlistenChanged?.();
      if (memoryTimer) clearTimeout(memoryTimer);
      if (previewing) void invoke("player_stop").catch(() => {});
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="mix-win">
  <header class="app-header mix-header">
    <div class="mix-win-title" data-tauri-drag-region title={titleText}>
      {titleText}
    </div>
    <div class="app-header-spacer" data-tauri-drag-region></div>
    {#if previewHint}
      <div class="mix-preview-hint" title="Cut from alignment (not a screen marker)">
        {previewHint}
      </div>
    {/if}
    <div class="mix-zoom-chip" title="Ctrl + scroll to zoom · drag to pan">
      {zoomLabel}
    </div>
    <button
      type="button"
      class="mix-preview-btn"
      class:is-playing={previewing}
      disabled={previewBusy || (!from && !to)}
      title={previewing
        ? "Stop preview (Space)"
        : "Preview as aligned on the graph from the playhead"}
      onclick={() => void togglePreview()}
    >
      {#if previewing}
        <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <rect x="6" y="6" width="4" height="12" rx="1" />
          <rect x="14" y="6" width="4" height="12" rx="1" />
        </svg>
        Stop
      {:else}
        <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <path d="M8 5v14l11-7z" />
        </svg>
        Preview
      {/if}
    </button>
    <WindowControls showMinimize={true} showMaximize={true} showClose={true} />
  </header>

  {#if previewError}
    <div class="mix-preview-error" role="alert">{previewError}</div>
  {/if}

  <div class="mix-body">
    <div class="mix-stage" bind:this={stageEl}>
      <section class="mix-track-panel" aria-label="Previous track">
        <div class="mix-track-meta">
          <div class="mix-track-meta-left">
            <span class="mix-track-role">Previous</span>
            <span class="mix-bpm-badge" title="BPM from file tags">
              {formatBpm(fromLane.bpm)}
              <span class="mix-bpm-unit">BPM</span>
            </span>
            {#if fromLane.bpm}
              <div class="mix-grid-tools" title="Beat grid phase">
                <button
                  type="button"
                  class="mix-grid-btn"
                  title="Nudge grid earlier (1/16 beat)"
                  disabled={gridAlignBusy === "from"}
                  onclick={() => nudgeGrid("from", -1 / 16)}
                >
                  ◀
                </button>
                <button
                  type="button"
                  class="mix-grid-btn mix-grid-align"
                  title="Auto-align sticks to kicks"
                  disabled={gridAlignBusy === "from"}
                  onclick={() => void alignGridToKick("from")}
                >
                  {gridAlignBusy === "from" ? "…" : "Kick"}
                </button>
                <button
                  type="button"
                  class="mix-grid-btn"
                  title="Nudge grid later (1/16 beat)"
                  disabled={gridAlignBusy === "from"}
                  onclick={() => nudgeGrid("from", 1 / 16)}
                >
                  ▶
                </button>
              </div>
            {/if}
            {#if fromLane.durationSecs > 0}
              <span class="mix-time-hint">
                {formatTime(fromLane.viewStartSecs)}
                →
                {formatTime(
                  fromLane.viewStartSecs + viewportSecsFor(fromWrap),
                )}
              </span>
            {/if}
          </div>
          <div class="mix-track-meta-right">
            <button
              type="button"
              class="mix-reset-btn"
              title="Jump to end"
              onclick={() => {
                resetView("from");
                scheduleMemorySave();
              }}
            >
              End
            </button>
            <span class="mix-track-name" title={from ? trackLabel(from) : ""}>
              {from ? trackLabel(from) : "—"}
            </span>
          </div>
        </div>
        <div
          class="mix-wave-wrap"
          class:is-dragging={drag?.lane === "from" && drag.active}
          class:is-snapped={drag?.lane === "from" && drag.snapped}
          bind:this={fromWrap}
          onwheel={(e) => onWheel(e, "from")}
          onpointerdown={(e) => onPointerDown(e, "from")}
          onpointermove={(e) => onPointerMove(e, "from")}
          onpointerup={(e) => onPointerUp(e, "from")}
          onpointercancel={(e) => onPointerUp(e, "from")}
          role="presentation"
        >
          <canvas class="mix-wave-canvas" bind:this={fromCanvas}></canvas>
          {#if fromLane.loading}
            <div class="mix-wave-status">
              <span class="mix-wave-spinner" aria-hidden="true"></span>
              Loading waveform…
            </div>
          {:else if fromLane.error}
            <div class="mix-wave-status error">{fromLane.error}</div>
          {:else}
            {#if fromLane.refining}
              <div class="mix-wave-refining" title="Loading detail">
                <span class="mix-wave-spinner" aria-hidden="true"></span>
              </div>
            {/if}
            {#if !fromLane.bpm}
              <div class="mix-wave-hint">No BPM tag — grid hidden</div>
            {:else}
              <div class="mix-wave-hint">
                Alt+click kick to pin grid · Kick auto-align
              </div>
            {/if}
          {/if}
        </div>
      </section>

      <section class="mix-track-panel" aria-label="Next track">
        <div class="mix-track-meta">
          <div class="mix-track-meta-left">
            <span class="mix-track-role">Next</span>
            <span class="mix-bpm-badge" title="BPM from file tags">
              {formatBpm(toLane.bpm)}
              <span class="mix-bpm-unit">BPM</span>
            </span>
            {#if toLane.bpm}
              <div class="mix-grid-tools" title="Beat grid phase">
                <button
                  type="button"
                  class="mix-grid-btn"
                  title="Nudge grid earlier (1/16 beat)"
                  disabled={gridAlignBusy === "to"}
                  onclick={() => nudgeGrid("to", -1 / 16)}
                >
                  ◀
                </button>
                <button
                  type="button"
                  class="mix-grid-btn mix-grid-align"
                  title="Auto-align sticks to kicks"
                  disabled={gridAlignBusy === "to"}
                  onclick={() => void alignGridToKick("to")}
                >
                  {gridAlignBusy === "to" ? "…" : "Kick"}
                </button>
                <button
                  type="button"
                  class="mix-grid-btn"
                  title="Nudge grid later (1/16 beat)"
                  disabled={gridAlignBusy === "to"}
                  onclick={() => nudgeGrid("to", 1 / 16)}
                >
                  ▶
                </button>
              </div>
            {/if}
            {#if toLane.durationSecs > 0}
              <span class="mix-time-hint">
                {formatTime(toLane.viewStartSecs)}
                →
                {formatTime(toLane.viewStartSecs + viewportSecsFor(toWrap))}
              </span>
            {/if}
          </div>
          <div class="mix-track-meta-right">
            <button
              type="button"
              class="mix-reset-btn"
              title="Jump to start"
              onclick={() => {
                resetView("to");
                scheduleMemorySave();
              }}
            >
              Start
            </button>
            <span class="mix-track-name" title={to ? trackLabel(to) : ""}>
              {to ? trackLabel(to) : "—"}
            </span>
          </div>
        </div>
        <div
          class="mix-wave-wrap"
          class:is-dragging={drag?.lane === "to" && drag.active}
          class:is-snapped={drag?.lane === "to" && drag.snapped}
          bind:this={toWrap}
          onwheel={(e) => onWheel(e, "to")}
          onpointerdown={(e) => onPointerDown(e, "to")}
          onpointermove={(e) => onPointerMove(e, "to")}
          onpointerup={(e) => onPointerUp(e, "to")}
          onpointercancel={(e) => onPointerUp(e, "to")}
          role="presentation"
        >
          <canvas class="mix-wave-canvas" bind:this={toCanvas}></canvas>
          {#if toLane.loading}
            <div class="mix-wave-status">
              <span class="mix-wave-spinner" aria-hidden="true"></span>
              Loading waveform…
            </div>
          {:else if toLane.error}
            <div class="mix-wave-status error">{toLane.error}</div>
          {:else}
            {#if toLane.refining}
              <div class="mix-wave-refining" title="Loading detail">
                <span class="mix-wave-spinner" aria-hidden="true"></span>
              </div>
            {/if}
            {#if !toLane.bpm}
              <div class="mix-wave-hint">No BPM tag — grid hidden</div>
            {:else}
              <div class="mix-wave-hint">
                Alt+click kick to pin grid · Kick auto-align
              </div>
            {/if}
          {/if}
        </div>
      </section>

      <!-- Block layer: transition zone + effect inserts over both lanes -->
      <div
        class="mix-blocks-layer"
        aria-label="Mix blocks"
        onwheel={onBlocksLayerWheel}
      >
        {#each blockLayouts as bl (bl.id)}
          <div
            class="mix-block"
            class:is-transition={bl.kind === "transition"}
            class:is-effect={bl.kind !== "transition"}
            class:is-selected={bl.selected}
            class:is-pinned={bl.pinned}
            class:is-expanded={bl.expanded}
            class:lane-from={bl.targetLane === "from"}
            class:lane-to={bl.targetLane === "to"}
            style:left="{bl.left}px"
            style:top="{bl.top}px"
            style:width="{bl.width}px"
            style:height="{bl.height}px"
            style:--block-accent={bl.accent}
            title="{bl.label}{bl.targetLane
              ? bl.targetLane === 'from'
                ? ' · Prev (top)'
                : ' · Next (bottom)'
              : ''} · {formatTime(bl.startFromSecs)}–{formatTime(
              bl.startFromSecs + bl.durationSecs,
            )}{bl.pinned ? ' · pinned' : ''}"
            onpointerdown={(e) => onBlockPointerDown(e, bl.id, "move")}
            onpointermove={onBlockPointerMove}
            onpointerup={onBlockPointerUp}
            onpointercancel={onBlockPointerUp}
            onwheel={onBlocksLayerWheel}
            role="group"
            aria-label={bl.label}
          >
            <div class="mix-block-chrome">
              <div
                class="mix-block-handle mix-block-handle-l"
                onpointerdown={(e) => onBlockPointerDown(e, bl.id, "resize-l")}
                onpointermove={onBlockPointerMove}
                onpointerup={onBlockPointerUp}
                onpointercancel={onBlockPointerUp}
                role="separator"
                aria-orientation="vertical"
                aria-label="Resize start"
              ></div>
              <div class="mix-block-body">
                <span class="mix-block-badge">{bl.short}</span>
                {#if bl.targetLane}
                  <span
                    class="mix-block-lane"
                    class:lane-from={bl.targetLane === "from"}
                    class:lane-to={bl.targetLane === "to"}
                    title={bl.targetLane === "from"
                      ? "Previous deck (top)"
                      : "Next deck (bottom)"}
                  >
                    {bl.targetLane === "from" ? "↑ Prev" : "↓ Next"}
                  </span>
                {/if}
                <span class="mix-block-name">{bl.label}</span>
                <span class="mix-block-time">
                  {formatTime(bl.durationSecs)}
                </span>
                <div
                  class="mix-block-actions"
                  role="toolbar"
                  aria-label="Block actions"
                  onpointerdown={(e) => {
                    e.stopPropagation();
                  }}
                >
                  {#if bl.kind !== "transition"}
                    <button
                      type="button"
                      class="mix-block-act"
                      class:is-on={bl.expanded}
                      title={bl.expanded
                        ? "Collapse graph"
                        : "Expand · edit envelope"}
                      onpointerdown={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        toggleExpand(bl.id);
                      }}
                    >
                      <svg
                        width="11"
                        height="11"
                        viewBox="0 0 24 24"
                        fill="currentColor"
                        aria-hidden="true"
                        style:transform={bl.expanded
                          ? "rotate(180deg)"
                          : "none"}
                      >
                        <path d="M7 10l5 5 5-5H7z" />
                      </svg>
                    </button>
                    <button
                      type="button"
                      class="mix-block-act"
                      title={bl.targetLane === "from"
                        ? "Move to next deck (bottom)"
                        : "Move to previous deck (top)"}
                      onpointerdown={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        selectedBlockId = bl.id;
                        toggleTargetLane(bl.id);
                      }}
                    >
                      <svg
                        width="11"
                        height="11"
                        viewBox="0 0 24 24"
                        fill="currentColor"
                        aria-hidden="true"
                      >
                        <path
                          d="M8 7h8v2H8V7zm0 4h8v2H8v-2zm0 4h5v2H8v-2zM18 5v14l-4-3.5L18 5z"
                          opacity="0.9"
                        />
                      </svg>
                    </button>
                  {/if}
                  <button
                    type="button"
                    class="mix-block-act"
                    class:is-on={bl.pinned}
                    title={bl.pinned ? "Unpin" : "Pin (lock size/position)"}
                    onpointerdown={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      selectedBlockId = bl.id;
                      togglePin(bl.id);
                    }}
                  >
                    {#if bl.pinned}
                      <svg width="11" height="11" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                        <path
                          d="M16 12V4h1V2H7v2h1v8l-2 2v2h5.2v6h1.6v-6H18v-2l-2-2z"
                        />
                      </svg>
                    {:else}
                      <svg width="11" height="11" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                        <path
                          d="M16 12V4h1V2H7v2h1v8l-2 2v2h5.2v6h1.6v-6H18v-2l-2-2z"
                          opacity="0.55"
                        />
                      </svg>
                    {/if}
                  </button>
                  <button
                    type="button"
                    class="mix-block-act danger"
                    title="Delete (Del)"
                    onpointerdown={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      removeBlock(bl.id);
                    }}
                  >
                    <svg width="11" height="11" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                      <path
                        d="M6 7h12v2H6V7zm2 3h8l-.8 10H8.8L8 10zm3-6h2l1 1h5v2H5V5h5l1-1z"
                      />
                    </svg>
                  </button>
                </div>
              </div>
              <div
                class="mix-block-handle mix-block-handle-r"
                onpointerdown={(e) => onBlockPointerDown(e, bl.id, "resize-r")}
                onpointermove={onBlockPointerMove}
                onpointerup={onBlockPointerUp}
                onpointercancel={onBlockPointerUp}
                role="separator"
                aria-orientation="vertical"
                aria-label="Resize end"
              ></div>
            </div>
            {#if bl.expanded && bl.kind !== "transition"}
              <div
                class="mix-block-editor"
                role="group"
                aria-label="Envelope editor"
                onpointerdown={(e) => e.stopPropagation()}
              >
                <MixEnvelopeGraph
                  kind={bl.kind}
                  points={bl.envelope}
                  curve={bl.curve}
                  accent={bl.accent}
                  label={envelopeParamLabel(bl.kind)}
                  onGestureStart={beginEnvelopeEdit}
                  onChange={(pts) => setBlockEnvelope(bl.id, pts)}
                  onCurveChange={(c) => setBlockCurve(bl.id, c)}
                />
              </div>
            {/if}
          </div>
        {/each}
      </div>

      {#if playheadUi && playheadUi.height > 0}
        <div
          class="mix-playhead"
          class:is-dragging={!!playheadDrag}
          class:is-previewing={previewing}
          class:off-view={!playheadUi.inView}
          style:left="{playheadUi.left}px"
          style:top="{playheadUi.top}px"
          style:height="{playheadUi.height}px"
          onpointerdown={onPlayheadPointerDown}
          onpointermove={onPlayheadPointerMove}
          onpointerup={onPlayheadPointerUp}
          onpointercancel={onPlayheadPointerUp}
          role="slider"
          tabindex="0"
          aria-label="Transition playhead"
          aria-valuemin={0}
          aria-valuemax={Math.max(
            playheadFromSecs + 1,
            fromLane.durationSecs || 0,
            // Prev-time of next-track end so scrubbing past the gap is allowed.
            toLane.durationSecs -
              (toLane.viewStartSecs - fromLane.viewStartSecs) +
              1,
          )}
          aria-valuenow={playheadFromSecs}
          title={`Cut · prev ${formatTime(playheadFromSecs)} · next ${formatTime(mapPrevToNext(playheadFromSecs))}`}
        >
          <div class="mix-playhead-line" aria-hidden="true"></div>
          <div class="mix-playhead-knob" aria-hidden="true"></div>
        </div>
      {/if}
    </div>

    {#if blockError}
      <div class="mix-block-error" role="status">{blockError}</div>
    {/if}

    <div class="mix-palette" aria-label="Block palette">
      <div class="mix-palette-label">Blocks</div>
      <div class="mix-palette-row">
        {#each MIX_PALETTE as item (item.kind)}
          <button
            type="button"
            class="mix-palette-chip"
            class:is-container={item.isContainer}
            class:is-disabled={!item.enabled}
            class:is-dragging={paletteDrag?.kind === item.kind}
            style:--chip-accent={item.accent}
            disabled={!item.enabled}
            title={item.description}
            onpointerdown={(e) => onPalettePointerDown(e, item.kind)}
            onpointermove={onPalettePointerMove}
            onpointerup={onPalettePointerUp}
            onpointercancel={onPalettePointerUp}
          >
            <span class="mix-palette-short">{item.short}</span>
            <span class="mix-palette-name">{item.label}</span>
          </button>
        {/each}
      </div>
      <p class="mix-palette-hint">
        Drag <strong>Transition</strong> · drop effects on <strong>top</strong>
        (prev) or <strong>bottom</strong> (next) · expand for graphs ·
        <kbd>Ctrl+Z</kbd> undo
      </p>
    </div>

    <p class="mix-help">
      <kbd>Space</kbd> preview · <kbd>Ctrl+Z</kbd> undo ·
      <kbd>Ctrl+Y</kbd> redo · playhead · pan · saved per edge
    </p>
  </div>
</div>

{#if paletteDrag}
  {@const ghost = paletteItem(paletteDrag.kind)}
  {#if ghost}
    <div
      class="mix-palette-ghost"
      style:left="{paletteDrag.clientX}px"
      style:top="{paletteDrag.clientY}px"
      style:--chip-accent={ghost.accent}
      aria-hidden="true"
    >
      <span class="mix-palette-short">{ghost.short}</span>
      <span class="mix-palette-name">{ghost.label}</span>
    </div>
  {/if}
{/if}