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
      });
    }, MEMORY_SAVE_MS);
  }

  /** Screen X of playhead inside the wave column (clamped to the view). */
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
      if (mem) applyMemoryViews(mem);
      else applyDefaultViews();
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
    // One sample per CSS pixel; max several peaks inside the column.
    const cols = Math.max(1, Math.ceil(cssW));
    const top: number[] = new Array(cols);
    const bot: number[] = new Array(cols);

    for (let col = 0; col < cols; col++) {
      const t0 = viewStart + (col / cols) * viewDur;
      const t1 = viewStart + ((col + 1) / cols) * viewDur;
      let amp = 0;
      const steps = 4;
      for (let s = 0; s < steps; s++) {
        const t = t0 + ((s + 0.5) / steps) * (t1 - t0);
        amp = Math.max(amp, sampleLaneAmp(lane, t, viewDur));
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
    // Reactive deps for redraw.
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
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      const direction = e.deltaY > 0 ? 1 / 1.12 : 1.12;
      zoomShared(direction, { clientX: e.clientX, anchorId: id });
      return;
    }
    e.preventDefault();
    // Prefer horizontal delta when present (trackpads).
    const dx =
      Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
    panBoth(-dx);
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
    // Tap (no pan) → place playhead under cursor.
    if (!wasPan) {
      setPlayheadFromClientX(e.clientX);
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
    const x = Math.max(0, Math.min(rect.width, clientX - rect.left));
    const t = fromLane.viewStartSecs + x / pxPerSec;
    const maxT =
      fromLane.durationSecs > 0 ? fromLane.durationSecs : Number.MAX_VALUE;
    playheadFromSecs = Math.max(0, Math.min(maxT, t));
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

    // Free playhead (can sit past end / before start when scrolling).
    const tFrom = playheadFromSecs;
    const tTo = mapPrevToNext(tFrom);

    let fromStartRel: number | null = null;
    let fromDurationSecs = 0;
    if (tFrom < fromLane.durationSecs) {
      fromStartRel = Math.max(0, tFrom);
      fromDurationSecs = Math.max(0.05, fromLane.durationSecs - fromStartRel);
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

    const toEndAbs =
      typeof to.cue_end_secs === "number" && Number.isFinite(to.cue_end_secs)
        ? to.cue_end_secs
        : null;

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
    return `${prev} · ${next}`;
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

  // Smooth playhead while previewing (RAF) — follows mix time from start.
  $effect(() => {
    if (!previewing || !previewSession) return;
    const session = previewSession;
    const startOrigin = session.fromStartRel;
    let raf = 0;
    const tick = () => {
      if (!previewing || !previewSession) return;
      const elapsed = (performance.now() - session.wallStartMs) / 1000;
      playheadFromSecs = startOrigin + elapsed;
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
    if (e.key === "Escape") {
      if (previewing) {
        e.preventDefault();
        void stopPreview();
        return;
      }
      void appWindow.close();
      return;
    }
    // Space — preview (don't steal when typing in future fields).
    if (e.key === " " || e.code === "Space") {
      const t = e.target;
      if (
        t instanceof HTMLInputElement ||
        t instanceof HTMLTextAreaElement ||
        (t instanceof HTMLElement && t.isContentEditable)
      ) {
        return;
      }
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
            {/if}
          {/if}
        </div>
      </section>

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
          aria-valuemax={fromLane.durationSecs || 0}
          aria-valuenow={playheadFromSecs}
          title={`Cut · prev ${formatTime(playheadFromSecs)} · next ${formatTime(mapPrevToNext(playheadFromSecs))}`}
        >
          <div class="mix-playhead-line" aria-hidden="true"></div>
          <div class="mix-playhead-knob" aria-hidden="true"></div>
        </div>
      {/if}
    </div>

    <p class="mix-help">
      <kbd>Space</kbd> play as on the graph (from playhead) · drag playhead ·
      scroll both / LMB pan one · saved per transition
    </p>
  </div>
</div>
