<script lang="ts">
  import "../../app.css";
  import "../../routes/+page.css";
  // Side-effect import: unscoped, avoids WebView2 issues when <style>@import is scoped.
  import "./TrackPropertiesWindow.css";
  import WindowControls from "./WindowControls.svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { listen } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import {
    isStreamTrack,
    sameTrackPath,
    trackDisplayArtist,
    trackDisplayTitle,
    type MusicFile,
  } from "$lib/stores/player.svelte";
  import {
    takePendingTrackProperties,
    type TrackPropertiesOpenPayload,
    type TrackPropertiesRemovedPayload,
  } from "$lib/stores/trackProperties.svelte";
  import { hydrateAccentFromStorage } from "$lib/coverAccent";
  import {
    clearCoverSrcCache,
    getCoverSrc,
    preferFullCoverPath,
  } from "$lib/coverCache";
  import { COVER_PLACEHOLDER_SRC } from "$lib/coverPlaceholder";
  import {
    applyEffectivePlaybackRate,
    clampPlaybackRate,
    getCachedGlobalPlaybackRate,
    getTrackPlaybackRate,
    setTrackPlaybackRate,
  } from "$lib/trackPrefs";

  /** This webview is bound to one track window label for life. */
  const windowLabel = getCurrentWindow().label;

  type Section = "metadata" | "details" | "lyrics" | "muzeeka";

  interface TagTableRow {
    id: string;
    name: string;
    value: string;
    read_only?: boolean;
  }

  interface AudioTechInfo {
    bitrate_kbps?: number | null;
    sample_rate_hz?: number | null;
    channels?: number | null;
    bit_depth?: number | null;
    duration_secs?: number | null;
    total_samples?: number | null;
    codec?: string | null;
    encoding?: string | null;
    tool?: string | null;
    audio_md5?: string | null;
    embedded_cuesheet?: boolean | null;
    file_size?: number | null;
    modified_unix?: number | null;
    created_unix?: number | null;
    file_name?: string | null;
    folder_name?: string | null;
    file_path?: string | null;
    play_count?: number | null;
    first_played_unix?: number | null;
    last_played_unix?: number | null;
  }

  const sections: { id: Section; label: string }[] = [
    { id: "metadata", label: "Metadata" },
    { id: "details", label: "Details" },
    { id: "lyrics", label: "Lyrics" },
    { id: "muzeeka", label: "Muzeeka" },
  ];

  const RATE_PRESETS = [0.75, 0.85, 1.0, 1.25, 1.5] as const;

  let activeSection = $state<Section>("metadata");

  let track = $state<MusicFile | null>(null);
  let rows = $state<TagTableRow[]>([]);
  /** Path the current `rows` belong to — prevents lyrics lookup using previous track tags. */
  let rowsForPath = $state<string | null>(null);
  let tech = $state<AudioTechInfo | null>(null);

  // Muzeeka tab — app-only per-track prefs
  let trackRateOverride = $state<number | null>(null);
  let trackRateDraft = $state(1.0);
  let trackRateBusy = $state(false);
  let trackRateError = $state<string | null>(null);
  let trackRateSuccess = $state<string | null>(null);

  // BPM (written into file tags)
  let bpmValue = $state<number | null>(null);
  let bpmDraft = $state("");
  let bpmBusy = $state(false);
  let bpmDetecting = $state(false);
  let bpmError = $state<string | null>(null);
  let bpmSuccess = $state<string | null>(null);
  let bpmTapTimes = $state<number[]>([]);
  let bpmTapEstimate = $state<number | null>(null);
  let bpmTapFlash = $state(false);
  let bpmTapResetTimer: ReturnType<typeof setTimeout> | null = null;

  let coverBusy = $state(false);
  let coverFailed = $state(false);
  let tableLoading = $state(false);

  let saving = $state(false);
  let error = $state<string | null>(null);
  let success = $state<string | null>(null);
  let dirty = $state(false);

  let filter = $state("");

  // Lyrics tab state
  let lyricsText = $state("");
  let lyricsLoading = $state(false);
  let lyricsBusy = $state(false);
  let lyricsDirty = $state(false);
  let lyricsError = $state<string | null>(null);
  let lyricsSuccess = $state<string | null>(null);

  /** Bumps on every loadTrack so stale async tag/tech/lyrics results are dropped. */
  let loadGen = 0;

  let isCue = $derived(!!track?.path?.includes("#cue:"));
  let isStream = $derived(isStreamTrack(track));

  let coverSrc = $derived.by(() => {
    if (!track || coverFailed) return null;
    const path =
      preferFullCoverPath(track.cover_path, track.cover_path_full) ??
      track.cover_path_full ??
      track.cover_path;
    return getCoverSrc(path);
  });

  let visibleRows = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter(
      (r) =>
        r.name.toLowerCase().includes(q) ||
        r.id.toLowerCase().includes(q) ||
        r.value.toLowerCase().includes(q),
    );
  });

  let filledCount = $derived(rows.filter((r) => r.value.trim()).length);

  if (typeof document !== "undefined") {
    document.documentElement.style.setProperty(
      "background-color",
      "#0a0a0a",
      "important",
    );
    if (document.body) {
      document.body.style.setProperty(
        "background-color",
        "#0a0a0a",
        "important",
      );
    }
    hydrateAccentFromStorage();
  }

  function rowValue(id: string): string {
    return rows.find((r) => r.id === id)?.value?.trim() ?? "";
  }

  /**
   * Match FullscreenPlayer / disk cache keys exactly (library snapshot).
   * Tag-table edits must not change the cache key or we miss cached TTML.
   */
  function lyricsParams(t: MusicFile) {
    return {
      title: trackDisplayTitle(t),
      artist: trackDisplayArtist(t),
      album: t.album ?? null,
      durationSecs:
        t.duration_secs != null && t.duration_secs > 0
          ? Math.round(t.duration_secs)
          : null,
      trackPath: t.path,
    };
  }

  function stillCurrent(t: MusicFile, gen: number): boolean {
    return gen === loadGen && !!track && sameTrackPath(track.path, t.path);
  }

  async function loadTagTable(t: MusicFile, gen = loadGen) {
    tableLoading = true;
    try {
      const nextRows = await invoke<TagTableRow[]>("library_get_tag_table", {
        path: t.path,
        audioPath: t.audio_path ?? null,
        snapshot: t,
      });
      if (!stillCurrent(t, gen)) return;
      rows = nextRows;
      rowsForPath = t.path;
    } catch (e) {
      if (!stillCurrent(t, gen)) return;
      rows = [];
      rowsForPath = null;
      error = typeof e === "string" ? e : String(e);
    } finally {
      if (gen === loadGen) tableLoading = false;
    }
  }

  async function loadTechFor(t: MusicFile, gen = loadGen) {
    try {
      const next = await invoke<AudioTechInfo>("library_audio_tech_info", {
        path: t.path,
        audioPath: t.audio_path ?? null,
      });
      if (!stillCurrent(t, gen)) return;
      tech = next;
    } catch {
      if (!stillCurrent(t, gen)) return;
      tech = null;
    }
  }

  async function loadLyricsFor(t: MusicFile, gen = loadGen) {
    lyricsLoading = true;
    lyricsDirty = false;
    lyricsError = null;
    try {
      const params = lyricsParams(t);
      const ttml = await invoke<string | null>("lyrics_fetch", {
        title: params.title,
        artist: params.artist,
        album: params.album,
        durationSecs: params.durationSecs,
      });
      if (!stillCurrent(t, gen)) return;
      let text = ttml?.trim() ?? "";
      // Fallback: embedded file tags when cache/network has nothing.
      if (!text && rowsForPath && sameTrackPath(rowsForPath, t.path)) {
        text = rowValue("UnsyncLyrics") || rowValue("Lyrics") || "";
      }
      lyricsText = text;
    } catch {
      if (!stillCurrent(t, gen)) return;
      lyricsText = "";
    } finally {
      if (gen === loadGen) lyricsLoading = false;
    }
  }

  function rateFillPct(rate: number): number {
    return Math.max(0, Math.min(100, ((rate - 0.25) / (2 - 0.25)) * 100));
  }

  function formatBpm(n: number | null | undefined): string {
    if (n == null || !Number.isFinite(n) || n <= 0) return "—";
    return Number.isInteger(n) ? String(n) : n.toFixed(1);
  }

  function parseBpmInput(raw: string): number | null {
    const t = raw.trim().replace(",", ".");
    if (!t) return null;
    const n = Number(t);
    if (!Number.isFinite(n) || n < 1 || n >= 1000) return null;
    return Math.round(n * 10) / 10;
  }

  function clearBpmTapTimer() {
    if (bpmTapResetTimer) {
      clearTimeout(bpmTapResetTimer);
      bpmTapResetTimer = null;
    }
  }

  function scheduleBpmTapReset() {
    clearBpmTapTimer();
    // Idle for 2.5s → start a new tap series.
    bpmTapResetTimer = setTimeout(() => {
      bpmTapTimes = [];
      bpmTapEstimate = null;
      bpmTapResetTimer = null;
    }, 2500);
  }

  function recomputeTapBpm(times: number[]) {
    if (times.length < 2) {
      bpmTapEstimate = null;
      return;
    }
    const intervals: number[] = [];
    for (let i = 1; i < times.length; i++) {
      const d = times[i] - times[i - 1];
      // Ignore absurd intervals (slower than 30 BPM / faster than 300 BPM).
      if (d >= 200 && d <= 2000) intervals.push(d);
    }
    if (intervals.length === 0) {
      bpmTapEstimate = null;
      return;
    }
    // Median interval is robust to one late tap.
    const sorted = [...intervals].sort((a, b) => a - b);
    const mid = sorted[Math.floor(sorted.length / 2)];
    const bpm = 60000 / mid;
    bpmTapEstimate = Math.round(bpm * 10) / 10;
    bpmDraft = formatBpm(bpmTapEstimate).replace("—", "");
  }

  function handleBpmTap() {
    const now = performance.now();
    const times = [...bpmTapTimes, now].slice(-12);
    bpmTapTimes = times;
    recomputeTapBpm(times);
    scheduleBpmTapReset();
    bpmTapFlash = true;
    setTimeout(() => {
      bpmTapFlash = false;
    }, 80);
    bpmError = null;
    bpmSuccess = null;
  }

  async function loadBpmFromFile(t: MusicFile, gen = loadGen) {
    try {
      const bpm = await invoke<number | null>("library_get_bpm", {
        path: t.path,
        audioPath: t.audio_path ?? null,
      });
      if (!stillCurrent(t, gen)) return;
      bpmValue = bpm;
      bpmDraft = bpm != null ? formatBpm(bpm).replace("—", "") : "";
    } catch {
      if (!stillCurrent(t, gen)) return;
      // Fall back to tag table if present.
      const fromRows =
        parseBpmInput(rowValue("Bpm")) ?? parseBpmInput(rowValue("IntegerBpm"));
      bpmValue = fromRows;
      bpmDraft = fromRows != null ? formatBpm(fromRows).replace("—", "") : "";
    }
  }

  async function loadTrackPrefs(t: MusicFile, gen = loadGen) {
    trackRateError = null;
    trackRateSuccess = null;
    bpmError = null;
    bpmSuccess = null;
    bpmTapTimes = [];
    bpmTapEstimate = null;
    clearBpmTapTimer();
    try {
      const override = await getTrackPlaybackRate(t.path);
      if (!stillCurrent(t, gen)) return;
      trackRateOverride = override;
      trackRateDraft = override ?? getCachedGlobalPlaybackRate();
    } catch (e) {
      if (!stillCurrent(t, gen)) return;
      trackRateOverride = null;
      trackRateDraft = getCachedGlobalPlaybackRate();
      trackRateError = typeof e === "string" ? e : String(e);
    }
    await loadBpmFromFile(t, gen);
  }

  async function handleDetectBpm() {
    if (!track || bpmDetecting || bpmBusy) return;
    const t = track;
    const gen = loadGen;
    bpmDetecting = true;
    bpmError = null;
    bpmSuccess = null;
    try {
      // Ensure shared player/BASS is up (main already does this; cheap no-op).
      try {
        await invoke("player_init");
      } catch {
        /* ignore — detect will error with a clearer message if BASS is dead */
      }
      const bpm = await invoke<number>("library_detect_bpm", {
        path: t.path,
        audioPath: t.audio_path ?? null,
      });
      if (!stillCurrent(t, gen)) return;
      const rounded = Math.round(bpm * 10) / 10;
      bpmDraft = formatBpm(rounded).replace("—", "");
      bpmTapEstimate = rounded;
      bpmSuccess = `Detected ${formatBpm(rounded)} BPM — press Write to save tags`;
    } catch (e) {
      if (!stillCurrent(t, gen)) return;
      const msg = typeof e === "string" ? e : String(e);
      bpmError = msg.replace(/^Error:\s*/i, "");
      console.error("[bpm] detect failed", e);
    } finally {
      if (gen === loadGen) bpmDetecting = false;
    }
  }

  async function handleWriteBpm() {
    if (!track || bpmBusy) return;
    const bpm = parseBpmInput(bpmDraft);
    if (bpm == null) {
      bpmError = "Enter a BPM between 1 and 999";
      return;
    }
    bpmBusy = true;
    bpmError = null;
    bpmSuccess = null;
    try {
      const updated = await invoke<MusicFile>("library_set_track_bpm", {
        path: track.path,
        audioPath: track.audio_path ?? null,
        bpm,
        snapshot: track,
      });
      if (!stillCurrent(track, loadGen)) return;
      track = {
        ...updated,
        cover_path: updated.cover_path ?? track.cover_path,
        cover_path_full: updated.cover_path_full ?? track.cover_path_full,
      };
      bpmValue = bpm;
      bpmDraft = formatBpm(bpm).replace("—", "");
      bpmSuccess = isCue
        ? `BPM ${formatBpm(bpm)} written to audio image tags`
        : `BPM ${formatBpm(bpm)} written to file tags`;
      // Refresh metadata table so BPM rows appear.
      await loadTagTable(track);
    } catch (e) {
      bpmError = typeof e === "string" ? e : String(e);
    } finally {
      bpmBusy = false;
    }
  }

  function handleBpmDraftInput(e: Event) {
    bpmDraft = (e.currentTarget as HTMLInputElement).value;
    bpmError = null;
    bpmSuccess = null;
  }

  async function commitTrackRate(rate: number | null) {
    if (!track || trackRateBusy) return;
    trackRateBusy = true;
    trackRateError = null;
    trackRateSuccess = null;
    try {
      await setTrackPlaybackRate(track.path, rate);
      trackRateOverride = rate;
      trackRateDraft = rate ?? getCachedGlobalPlaybackRate();
      trackRateSuccess =
        rate == null
          ? "Using global speed from Settings"
          : `Track speed set to ${rate.toFixed(2)}×`;
      // Live-apply if this track is what the player would use next/now.
      void applyEffectivePlaybackRate(track.path);
    } catch (e) {
      trackRateError = typeof e === "string" ? e : String(e);
    } finally {
      trackRateBusy = false;
    }
  }

  function onTrackRateInput(e: Event) {
    const v = clampPlaybackRate(
      Number((e.currentTarget as HTMLInputElement).value),
    );
    trackRateDraft = v;
    trackRateSuccess = null;
  }

  function onTrackRateCommit() {
    void commitTrackRate(clampPlaybackRate(trackRateDraft));
  }

  function onTrackRatePreset(r: number) {
    trackRateDraft = r;
    void commitTrackRate(r);
  }

  function onTrackRateReset() {
    void commitTrackRate(null);
  }

  function loadTrack(next: MusicFile) {
    // Each window is one track — ignore foreign payloads.
    if (track && !sameTrackPath(track.path, next.path)) {
      return;
    }

    const gen = ++loadGen;
    track = next;
    rows = [];
    rowsForPath = null;
    tech = null;
    dirty = false;
    error = null;
    success = null;
    coverFailed = false;
    filter = "";
    lyricsText = "";
    lyricsDirty = false;
    lyricsError = null;
    lyricsSuccess = null;
    trackRateOverride = null;
    trackRateDraft = getCachedGlobalPlaybackRate();
    trackRateError = null;
    trackRateSuccess = null;
    bpmValue = null;
    bpmDraft = "";
    bpmError = null;
    bpmSuccess = null;
    bpmTapTimes = [];
    bpmTapEstimate = null;
    clearBpmTapTimer();

    void loadTechFor(next, gen);
    void loadTrackPrefs(next, gen);
    // Library snapshot lyrics key (parallel) + tags for metadata table.
    void loadLyricsFor(next, gen);
    void (async () => {
      await loadTagTable(next, gen);
      if (!stillCurrent(next, gen)) return;
      // If network/cache was empty, fill from embedded tags once rows exist.
      if (!lyricsText.trim() && !lyricsDirty) {
        const embedded =
          rowValue("UnsyncLyrics") || rowValue("Lyrics") || "";
        if (embedded) lyricsText = embedded;
      }
    })();
  }

  function markDirty() {
    dirty = true;
    success = null;
  }

  function markLyricsDirty() {
    lyricsDirty = true;
    lyricsSuccess = null;
  }

  function updateRow(id: string, value: string) {
    rows = rows.map((r) => (r.id === id ? { ...r, value } : r));
    markDirty();
  }

  function formatDuration(secs: number | null | undefined): string {
    if (secs == null || !Number.isFinite(secs) || secs < 0) return "—";
    const m = Math.floor(secs / 60);
    const s = Math.floor(secs % 60);
    return `${m}:${s.toString().padStart(2, "0")}`;
  }

  /** Foobar-style `3:06.441 (8 222 057 samples)`. */
  function formatDurationDetailed(
    secs: number | null | undefined,
    samples: number | null | undefined,
  ): string {
    if (secs == null || !Number.isFinite(secs) || secs < 0) return "—";
    const m = Math.floor(secs / 60);
    const rem = secs - m * 60;
    const whole = Math.floor(rem);
    const ms = Math.round((rem - whole) * 1000);
    let out = `${m}:${whole.toString().padStart(2, "0")}.${ms.toString().padStart(3, "0")}`;
    if (samples != null && Number.isFinite(samples) && samples > 0) {
      out += ` (${formatGroupedInt(Math.round(samples))} samples)`;
    }
    return out;
  }

  function formatGroupedInt(n: number): string {
    return Math.round(n)
      .toString()
      .replace(/\B(?=(\d{3})+(?!\d))/g, " ");
  }

  function formatSize(bytes: number | null | undefined): string {
    if (bytes == null || !Number.isFinite(bytes) || bytes <= 0) return "—";
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  /** Foobar-style `25.1 MB (26 414 793 bytes)`. */
  function formatSizeDetailed(bytes: number | null | undefined): string {
    if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return "—";
    if (bytes === 0) return "0 B";
    return `${formatSize(bytes)} (${formatGroupedInt(bytes)} bytes)`;
  }

  function formatBitrate(kbps: number | null | undefined): string {
    if (kbps == null || !Number.isFinite(kbps) || kbps <= 0) return "—";
    return `${Math.round(kbps)} kbps`;
  }

  function formatSampleRate(hz: number | null | undefined): string {
    if (hz == null || !Number.isFinite(hz) || hz <= 0) return "—";
    if (hz >= 1000) {
      const k = hz / 1000;
      return `${Number.isInteger(k) ? k : k.toFixed(1)} kHz`;
    }
    return `${hz} Hz`;
  }

  function formatSampleRateHz(hz: number | null | undefined): string {
    if (hz == null || !Number.isFinite(hz) || hz <= 0) return "—";
    return `${formatGroupedInt(hz)} Hz`;
  }

  function formatChannels(ch: number | null | undefined): string {
    if (ch == null || ch <= 0) return "—";
    if (ch === 1) return "Mono";
    if (ch === 2) return "Stereo";
    return `${ch} ch`;
  }

  function formatChannelsRaw(ch: number | null | undefined): string {
    if (ch == null || ch <= 0) return "—";
    return String(ch);
  }

  function formatBitDepth(bits: number | null | undefined): string {
    if (bits == null || bits <= 0) return "—";
    return String(bits);
  }

  function formatUnixLocal(unix: number | null | undefined): string {
    if (unix == null || !Number.isFinite(unix)) return "—";
    const d = new Date(unix * 1000);
    if (Number.isNaN(d.getTime())) return "—";
    const y = d.getFullYear();
    const mo = (d.getMonth() + 1).toString().padStart(2, "0");
    const day = d.getDate().toString().padStart(2, "0");
    const h = d.getHours().toString().padStart(2, "0");
    const mi = d.getMinutes().toString().padStart(2, "0");
    const s = d.getSeconds().toString().padStart(2, "0");
    return `${y}-${mo}-${day} ${h}:${mi}:${s}`;
  }

  function formatYesNo(v: boolean | null | undefined): string {
    if (v == null) return "—";
    return v ? "yes" : "no";
  }

  /** Foobar-style `1 time` / `N times`. */
  function formatPlayCount(n: number | null | undefined): string {
    const count = n == null || !Number.isFinite(n) || n < 0 ? 0 : Math.floor(n);
    return count === 1 ? "1 time" : `${count} times`;
  }

  function dash(v: string | null | undefined): string {
    const t = v?.trim();
    return t ? t : "—";
  }

  /** Subsong index: 0 for plain files; 0-based for `#cue:N`. */
  function subsongIndex(t: MusicFile): number {
    const m = t.path.match(/#cue:(\d+)/i);
    if (!m) return 0;
    const n = Number.parseInt(m[1], 10);
    if (!Number.isFinite(n) || n <= 0) return 0;
    return n - 1;
  }

  /** Strip CUE virtual suffix and return parent directory. */
  function folderFromTrackPath(path: string): string {
    const cleaned = path.replace(/#cue:\d+$/i, "");
    const i = Math.max(cleaned.lastIndexOf("\\"), cleaned.lastIndexOf("/"));
    return i > 0 ? cleaned.slice(0, i) : "";
  }

  type DetailRow = { label: string; value: string; mono?: boolean };

  let detailsGroups = $derived.by(() => {
    if (!track) return [] as { title: string; rows: DetailRow[] }[];
    const size = tech?.file_size ?? track.size;
    const duration = tech?.duration_secs ?? track.duration_secs;
    const diskPath = tech?.file_path ?? track.audio_path ?? track.path.replace(/#cue:\d+$/i, "");
    return [
      {
        title: "Location",
        rows: [
          {
            label: "File name",
            value: dash(tech?.file_name ?? track.file_name),
            mono: true,
          },
          {
            label: "Folder name",
            value: dash(
              tech?.folder_name ||
                folderFromTrackPath(track.audio_path ?? track.path),
            ),
            mono: true,
          },
          {
            label: "File path",
            value: dash(diskPath),
            mono: true,
          },
          { label: "Subsong index", value: String(subsongIndex(track)) },
          { label: "File size", value: formatSizeDetailed(size) },
          {
            label: "Last modified",
            value: formatUnixLocal(tech?.modified_unix),
          },
          { label: "Created", value: formatUnixLocal(tech?.created_unix) },
        ],
      },
      {
        title: "General",
        rows: [
          {
            label: "Duration",
            value: formatDurationDetailed(duration, tech?.total_samples),
          },
          {
            label: "Sample rate",
            value: formatSampleRateHz(tech?.sample_rate_hz),
          },
          {
            label: "Channels",
            value: formatChannelsRaw(tech?.channels),
          },
          {
            label: "Bits per sample",
            value: formatBitDepth(tech?.bit_depth),
          },
          {
            label: "Bitrate",
            value: formatBitrate(tech?.bitrate_kbps),
          },
          { label: "Codec", value: dash(tech?.codec) },
          { label: "Encoding", value: dash(tech?.encoding) },
          { label: "Tool", value: dash(tech?.tool) },
          {
            label: "Embedded cuesheet",
            value: formatYesNo(tech?.embedded_cuesheet),
          },
          {
            label: "Audio MD5",
            value: dash(tech?.audio_md5),
            mono: true,
          },
        ],
      },
      {
        title: "Playback Statistics",
        rows: [
          {
            label: "Played",
            value: formatPlayCount(tech?.play_count),
          },
          {
            label: "First played",
            value: formatUnixLocal(tech?.first_played_unix),
          },
          {
            label: "Last played",
            value: formatUnixLocal(tech?.last_played_unix),
          },
        ],
      },
    ];
  });

  function isMultilineField(id: string): boolean {
    return (
      id === "Comment" ||
      id === "Description" ||
      id === "Lyrics" ||
      id === "UnsyncLyrics"
    );
  }

  async function closeWindow() {
    // Multi-window: destroy this instance (do not hide/reuse a shared shell).
    try {
      await getCurrentWindow().close();
    } catch {
      /* ignore */
    }
  }

  async function handleSave() {
    if (!track || saving) return;
    saving = true;
    error = null;
    success = null;
    try {
      const updated = await invoke<MusicFile>("library_set_tag_table", {
        path: track.path,
        audioPath: track.audio_path ?? null,
        rows,
        snapshot: track,
      });

      track = {
        ...updated,
        cover_path: updated.cover_path ?? track.cover_path,
        cover_path_full: updated.cover_path_full ?? track.cover_path_full,
      };
      dirty = false;
      success = isStream
        ? "Saved to library"
        : isCue
          ? "Tags written to audio image + library"
          : "Tags written to file + library";
      await loadTagTable(track);
    } catch (e) {
      error = typeof e === "string" ? e : String(e);
    } finally {
      saving = false;
    }
  }

  async function handleChangeCover() {
    if (!track || coverBusy) return;
    const selected = await open({
      multiple: false,
      filters: [
        {
          name: "Images",
          extensions: ["jpg", "jpeg", "png", "webp", "gif", "bmp"],
        },
        { name: "All files", extensions: ["*"] },
      ],
    });
    const imagePath = typeof selected === "string" ? selected : null;
    if (!imagePath) return;

    coverBusy = true;
    error = null;
    success = null;
    try {
      const snapshot: MusicFile = {
        ...track,
        title: rowValue("TrackTitle") || track.title,
        artist: rowValue("TrackArtist") || track.artist,
        album: rowValue("AlbumTitle") || track.album,
        genre: rowValue("Genre") || track.genre,
      };
      const updated = await invoke<MusicFile>("library_set_track_cover", {
        path: track.path,
        imagePath,
        snapshot,
      });
      clearCoverSrcCache();
      coverFailed = false;
      track = updated;
      success = isCue
        ? "Cover embedded into audio image"
        : "Cover embedded into file";
    } catch (e) {
      error = typeof e === "string" ? e : String(e);
    } finally {
      coverBusy = false;
    }
  }

  async function handleFindLyrics() {
    if (!track || lyricsBusy) return;
    lyricsBusy = true;
    lyricsError = null;
    lyricsSuccess = null;
    try {
      const params = lyricsParams(track);
      const found = await invoke<boolean>("lyrics_refetch", {
        title: params.title,
        artist: params.artist,
        album: params.album,
        durationSecs: params.durationSecs,
        trackPath: params.trackPath,
      });
      if (found) {
        await loadLyricsFor(track);
        lyricsSuccess = "Lyrics found";
      } else {
        lyricsError = "No lyrics found";
      }
    } catch (e) {
      lyricsError = typeof e === "string" ? e : String(e);
    } finally {
      lyricsBusy = false;
    }
  }

  async function handleImportLyrics() {
    if (!track || lyricsBusy) return;
    const selected = await open({
      multiple: false,
      filters: [
        { name: "Lyrics", extensions: ["ttml", "xml", "lrc", "txt"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    const path = typeof selected === "string" ? selected : null;
    if (!path) return;

    lyricsBusy = true;
    lyricsError = null;
    lyricsSuccess = null;
    try {
      const params = lyricsParams(track);
      await invoke("lyrics_import_ttml", {
        title: params.title,
        artist: params.artist,
        album: params.album,
        durationSecs: params.durationSecs,
        path,
        trackPath: params.trackPath,
      });
      await loadLyricsFor(track);
      lyricsSuccess = "Lyrics imported";
    } catch (e) {
      lyricsError = typeof e === "string" ? e : String(e);
    } finally {
      lyricsBusy = false;
    }
  }

  async function handleSaveLyrics() {
    if (!track || lyricsBusy) return;
    if (!lyricsText.trim()) {
      lyricsError = "Text is empty — use Clear to remove lyrics";
      return;
    }
    lyricsBusy = true;
    lyricsError = null;
    lyricsSuccess = null;
    try {
      const params = lyricsParams(track);
      await invoke("lyrics_save_text", {
        title: params.title,
        artist: params.artist,
        album: params.album,
        durationSecs: params.durationSecs,
        content: lyricsText,
        trackPath: params.trackPath,
      });
      lyricsDirty = false;
      lyricsSuccess = "Lyrics saved";
    } catch (e) {
      lyricsError = typeof e === "string" ? e : String(e);
    } finally {
      lyricsBusy = false;
    }
  }

  async function handleClearLyrics() {
    if (!track || lyricsBusy) return;
    lyricsBusy = true;
    lyricsError = null;
    lyricsSuccess = null;
    try {
      const params = lyricsParams(track);
      await invoke("lyrics_clear", {
        title: params.title,
        artist: params.artist,
        album: params.album,
        durationSecs: params.durationSecs,
        trackPath: params.trackPath,
      });
      lyricsText = "";
      lyricsDirty = false;
      lyricsSuccess = "Lyrics cleared";
    } catch (e) {
      lyricsError = typeof e === "string" ? e : String(e);
    } finally {
      lyricsBusy = false;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      void closeWindow();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key === "s") {
      e.preventDefault();
      if (activeSection === "lyrics") {
        void handleSaveLyrics();
      } else {
        void handleSave();
      }
    }
  }

  function clearAndClose() {
    loadGen += 1;
    track = null;
    rows = [];
    rowsForPath = null;
    tech = null;
    dirty = false;
    error = null;
    success = null;
    filter = "";
    lyricsText = "";
    lyricsLoading = false;
    lyricsDirty = false;
    lyricsError = null;
    lyricsSuccess = null;
    void closeWindow();
  }

  function acceptOpenPayload(
    payload: TrackPropertiesOpenPayload | null | undefined,
  ): boolean {
    if (!payload?.track) return false;
    // emitTo targets this label; global emit still filtered by windowLabel.
    if (payload.windowLabel && payload.windowLabel !== windowLabel) {
      return false;
    }
    // Window is single-track: allow first load or refresh of the same path.
    if (track && !sameTrackPath(track.path, payload.track.path)) {
      return false;
    }
    return true;
  }

  onMount(() => {
    let unlistenOpen: (() => void) | undefined;
    let unlistenRemoved: (() => void) | undefined;
    let unlistenCloseAll: (() => void) | undefined;
    let cancelled = false;

    void (async () => {
      unlistenOpen = await listen<TrackPropertiesOpenPayload>(
        "track-properties:open",
        (event) => {
          if (!acceptOpenPayload(event.payload)) return;
          loadTrack(event.payload.track);
        },
      );
      if (cancelled) {
        unlistenOpen();
        return;
      }

      unlistenRemoved = await listen<TrackPropertiesRemovedPayload>(
        "track-properties:tracks-removed",
        (event) => {
          if (!track) return;
          const paths = event.payload?.paths ?? [];
          if (paths.some((p) => sameTrackPath(p, track!.path))) {
            clearAndClose();
          }
        },
      );
      if (cancelled) {
        unlistenOpen?.();
        unlistenRemoved?.();
        return;
      }

      unlistenCloseAll = await listen("track-properties:close-all", () => {
        clearAndClose();
      });
      if (cancelled) {
        unlistenOpen?.();
        unlistenRemoved?.();
        unlistenCloseAll?.();
        return;
      }

      // Per-label handoff written before this webview finished booting.
      const pending = takePendingTrackProperties(windowLabel);
      if (pending && acceptOpenPayload(pending)) {
        loadTrack(pending.track);
      }
    })();

    return () => {
      cancelled = true;
      unlistenOpen?.();
      unlistenRemoved?.();
      unlistenCloseAll?.();
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="props-window">
  <header class="app-header props-header">
    <div class="props-win-title" data-tauri-drag-region>
      {#if track}
        {trackDisplayTitle(track)} — properties
      {:else}
        Track properties
      {/if}
    </div>
    <div class="app-header-spacer" data-tauri-drag-region></div>
    <WindowControls showMinimize={false} showMaximize={false} />
  </header>

  <div class="props-layout">
    <aside class="props-sidebar glass">
      <div class="props-nav">
        {#each sections as section (section.id)}
          <button
            type="button"
            class="nav-item"
            class:active={activeSection === section.id}
            onclick={() => {
              activeSection = section.id;
              // Refresh probe + play stats when opening Details.
              if (section.id === "details" && track) {
                void loadTechFor(track);
              }
              if (section.id === "muzeeka" && track) {
                void loadTrackPrefs(track);
              }
            }}
          >
            <span class="nav-icon" aria-hidden="true">
              {#if section.id === "metadata"}
                <svg
                  width="16"
                  height="16"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.75"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                >
                  <path
                    d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z"
                  />
                  <polyline points="14 2 14 8 20 8" />
                  <line x1="8" y1="13" x2="16" y2="13" />
                  <line x1="8" y1="17" x2="14" y2="17" />
                </svg>
              {:else if section.id === "details"}
                <svg
                  width="16"
                  height="16"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.75"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                >
                  <circle cx="12" cy="12" r="10" />
                  <line x1="12" y1="16" x2="12" y2="12" />
                  <line x1="12" y1="8" x2="12.01" y2="8" />
                </svg>
              {:else if section.id === "lyrics"}
                <svg
                  width="16"
                  height="16"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.75"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                >
                  <path
                    d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z"
                  />
                  <polyline points="14 2 14 8 20 8" />
                  <line x1="8" y1="13" x2="16" y2="13" />
                  <line x1="8" y1="17" x2="12" y2="17" />
                  <circle
                    cx="17"
                    cy="17"
                    r="1.5"
                    fill="currentColor"
                    stroke="none"
                  />
                </svg>
              {:else if section.id === "muzeeka"}
                <svg
                  width="16"
                  height="16"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.75"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                >
                  <path
                    d="M12 3v10.55A4 4 0 1 0 14 17V7h4V3h-6Z"
                  />
                </svg>
              {/if}
            </span>
            <span class="nav-label">{section.label}</span>
            {#if section.id === "muzeeka" && trackRateOverride != null}
              <span class="nav-badge">{trackRateOverride.toFixed(2)}×</span>
            {/if}
          </button>
        {/each}
      </div>
    </aside>

    <div class="props-content">
      {#if !track}
        <div class="props-section">
          <h2 class="section-title">Metadata</h2>
          <p class="section-desc">Open a track from the list context menu…</p>
        </div>
      {:else if activeSection === "metadata"}
        <div class="props-section props-section-fill">
          <div class="section-head">
            <div>
              <h2 class="section-title">Metadata</h2>
              <p class="section-desc">
                Core tags + any extra fields present in the file. Empty value
                removes the field.
                {#if isStream}
                  Station name (Track Title) and artist are stored in the
                  library — streams have no file tags. While the station is
                  playing, the live track title replaces the station name in
                  the player.
                {:else if isCue}
                  CUE tracks write into the shared audio image.
                {/if}
              </p>
            </div>
            <div class="section-head-meta">
              {filledCount}/{rows.length} filled
            </div>
          </div>

          <div class="props-card props-top-strip">
            <div class="top-strip">
              <div class="cover-block">
                <div class="cover-frame">
                  {#if coverSrc}
                    <img
                      class="cover-img"
                      src={coverSrc}
                      alt=""
                      onerror={() => (coverFailed = true)}
                    />
                  {:else}
                    <img
                      class="cover-img cover-placeholder"
                      src={COVER_PLACEHOLDER_SRC}
                      alt=""
                    />
                  {/if}
                </div>
              </div>
              <div class="top-strip-info">
                <div>
                  <div class="card-label">File</div>
                  <div class="card-value card-value-path">{track.path}</div>
                  {#if isCue && track.audio_path}
                    <div class="card-value card-value-path">
                      Image: {track.audio_path}
                    </div>
                  {/if}
                </div>
                <div class="bottom-strip-info">
                  <button
                    type="button"
                    class="action-btn cover-btn"
                    disabled={coverBusy || saving}
                    onclick={() => void handleChangeCover()}
                  >
                    {coverBusy ? "Writing…" : "Cover…"}
                  </button>
                  <div class="tech-inline">
                    <span>{formatDuration(track.duration_secs)}</span>
                    <span>{formatBitrate(tech?.bitrate_kbps)}</span>
                    <span>{formatSampleRate(tech?.sample_rate_hz)}</span>
                    <span>{formatChannels(tech?.channels)}</span>
                    <span>{formatSize(track.size)}</span>
                    <span
                      >{track.extension
                        ? track.extension.toUpperCase()
                        : "—"}</span
                    >
                  </div>
                </div>
              </div>
            </div>
          </div>

          <div class="table-toolbar">
            <input
              class="props-input filter-input"
              type="search"
              placeholder="Filter by name / value…"
              bind:value={filter}
            />
          </div>

          <div class="props-card tag-table-card">
            {#if tableLoading}
              <div class="table-empty">Reading tags…</div>
            {:else if visibleRows.length === 0}
              <div class="table-empty">
                No fields{filter ? " match the filter" : ""}
              </div>
            {:else}
              <div class="tag-table" role="table">
                <div class="tag-table-head" role="row">
                  <div class="col-name" role="columnheader">Name</div>
                  <div class="col-value" role="columnheader">Value</div>
                </div>
                <div class="tag-table-body">
                  {#each visibleRows as row (row.id)}
                    <div
                      class="tag-table-row"
                      class:filled={!!row.value.trim()}
                      class:readonly={row.read_only}
                      role="row"
                    >
                      <div class="col-name" role="cell" title={row.id}>
                        <span class="field-name">{row.name}</span>
                      </div>
                      <div class="col-value" role="cell">
                        {#if row.read_only}
                          <span class="field-ro">{row.value || "—"}</span>
                        {:else if isMultilineField(row.id)}
                          <textarea
                            class="field-input field-textarea"
                            rows="2"
                            value={row.value}
                            oninput={(e) =>
                              updateRow(
                                row.id,
                                (e.currentTarget as HTMLTextAreaElement).value,
                              )}
                            disabled={saving}
                            spellcheck="false"
                          ></textarea>
                        {:else}
                          <input
                            class="field-input"
                            type="text"
                            value={row.value}
                            oninput={(e) =>
                              updateRow(
                                row.id,
                                (e.currentTarget as HTMLInputElement).value,
                              )}
                            disabled={saving}
                            spellcheck="false"
                          />
                        {/if}
                      </div>
                    </div>
                  {/each}
                </div>
              </div>
            {/if}
          </div>

          <div class="props-actions-bar">
            <div
              class="props-status"
              class:error={!!error}
              class:success={!!success && !error}
              class:muted={dirty && !error && !success}
            >
              {#if error}
                {error}
              {:else if success}
                {success}
              {:else if dirty}
                Unsaved changes
              {/if}
            </div>
            <div class="props-actions">
              <button
                type="button"
                class="action-btn"
                onclick={() => void closeWindow()}
              >
                Close
              </button>
              <button
                type="button"
                class="action-btn action-btn-primary"
                disabled={!track || saving || tableLoading}
                onclick={() => void handleSave()}
              >
                {saving ? "Saving…" : "Save"}
              </button>
            </div>
          </div>
        </div>
      {:else if activeSection === "details"}
        <div class="props-section props-section-fill">
          <div class="section-head">
            <div>
              <h2 class="section-title">Details</h2>
              <p class="section-desc">
                File location, stream properties, and playback statistics.
              </p>
            </div>
          </div>

          <div class="props-card details-card">
            {#each detailsGroups as group (group.title)}
              <div class="details-group">
                <div class="details-group-title">{group.title}</div>
                <div class="details-rows">
                  {#each group.rows as row (row.label)}
                    <div class="details-row">
                      <div class="details-label">{row.label}</div>
                      <div
                        class="details-value"
                        class:mono={row.mono}
                        title={row.value !== "—" ? row.value : undefined}
                      >
                        {row.value}
                      </div>
                    </div>
                  {/each}
                </div>
              </div>
            {/each}
          </div>

          <div class="props-actions-bar">
            <div class="props-status muted"></div>
            <div class="props-actions">
              <button
                type="button"
                class="action-btn"
                onclick={() => void closeWindow()}
              >
                Close
              </button>
            </div>
          </div>
        </div>
      {:else if activeSection === "lyrics"}
        <div class="props-section props-section-fill">
          <div class="section-head">
            <div>
              <h2 class="section-title">Lyrics</h2>
              <p class="section-desc">
                Find, import, edit, or clear lyrics for this track (TTML or
                plain text).
              </p>
            </div>
          </div>

          <div class="props-card lyrics-card">
            <div class="lyrics-toolbar">
              <button
                type="button"
                class="action-btn"
                disabled={lyricsBusy || lyricsLoading}
                onclick={() => void handleFindLyrics()}
              >
                Find
              </button>
              <button
                type="button"
                class="action-btn"
                disabled={lyricsBusy || lyricsLoading}
                onclick={() => void handleImportLyrics()}
              >
                Import…
              </button>
              <button
                type="button"
                class="action-btn"
                disabled={lyricsBusy ||
                  lyricsLoading ||
                  (!lyricsText.trim() && !lyricsDirty)}
                onclick={() => void handleClearLyrics()}
              >
                Clear
              </button>
              <div class="lyrics-toolbar-spacer"></div>
              <button
                type="button"
                class="action-btn action-btn-primary"
                disabled={lyricsBusy ||
                  lyricsLoading ||
                  !lyricsDirty ||
                  !lyricsText.trim()}
                onclick={() => void handleSaveLyrics()}
              >
                {lyricsBusy ? "Saving…" : "Save lyrics"}
              </button>
            </div>

            <textarea
              class="lyrics-editor"
              placeholder={lyricsLoading
                ? "Loading…"
                : "Paste or edit lyrics (TTML / plain text)…"}
              bind:value={lyricsText}
              oninput={markLyricsDirty}
              disabled={lyricsBusy || lyricsLoading}
              spellcheck="false"
            ></textarea>
          </div>

          <div class="props-actions-bar">
            <div
              class="props-status"
              class:error={!!lyricsError}
              class:success={!!lyricsSuccess && !lyricsError}
              class:muted={lyricsDirty && !lyricsError && !lyricsSuccess}
            >
              {#if lyricsError}
                {lyricsError}
              {:else if lyricsSuccess}
                {lyricsSuccess}
              {:else if lyricsDirty}
                Unsaved lyrics changes
              {:else if lyricsLoading}
                Loading…
              {/if}
            </div>
            <div class="props-actions">
              <button
                type="button"
                class="action-btn"
                onclick={() => void closeWindow()}
              >
                Close
              </button>
            </div>
          </div>
        </div>
      {:else if activeSection === "muzeeka"}
        <div class="props-section props-section-fill">
          <div class="section-head">
            <div>
              <h2 class="section-title">Muzeeka</h2>
              <p class="section-desc">
                App extras for this track. Speed stays in Muzeeka only; BPM is
                written into the file tags.
              </p>
            </div>
          </div>

          <div class="props-card muzeeka-card">
            <div class="muzeeka-feature">
              <div class="muzeeka-feature-head">
                <div>
                  <div class="card-label">Playback speed</div>
                  <div class="card-value">
                    {#if trackRateOverride != null}
                      Custom for this track (app-only)
                    {:else}
                      Using global speed from Settings ({getCachedGlobalPlaybackRate().toFixed(2)}×)
                    {/if}
                  </div>
                </div>
                <div class="rate-display">
                  <span class="rate-value-big">{trackRateDraft.toFixed(2)}×</span>
                </div>
              </div>

              <div class="rate-slider-row">
                <input
                  type="range"
                  class="rate-slider"
                  min="0.25"
                  max="2"
                  step="0.01"
                  value={trackRateDraft}
                  style={`--fill: ${rateFillPct(trackRateDraft)}%`}
                  disabled={!track || trackRateBusy}
                  oninput={onTrackRateInput}
                  onchange={onTrackRateCommit}
                  onpointerup={onTrackRateCommit}
                />
                <div class="rate-bounds">
                  <span>0.25×</span>
                  <span>2.00×</span>
                </div>
              </div>

              <div class="rate-presets">
                {#each RATE_PRESETS as r}
                  <button
                    type="button"
                    class="preset-btn"
                    class:active={trackRateOverride != null &&
                      Math.abs(trackRateDraft - r) < 0.01}
                    disabled={!track || trackRateBusy}
                    onclick={() => onTrackRatePreset(r)}
                  >
                    {r.toFixed(r === 1 ? 1 : 2)}×
                  </button>
                {/each}
                <button
                  type="button"
                  class="preset-btn"
                  disabled={!track ||
                    trackRateBusy ||
                    trackRateOverride == null}
                  onclick={() => onTrackRateReset()}
                  title="Clear override — use Settings global speed"
                >
                  Reset
                </button>
              </div>
            </div>

            <div class="muzeeka-divider"></div>

            <div class="muzeeka-feature">
              <div class="muzeeka-feature-head">
                <div>
                  <div class="card-label">BPM</div>
                  <div class="card-value">
                    {#if bpmValue != null}
                      In file tags: {formatBpm(bpmValue)}
                    {:else}
                      No BPM in tags yet — tap, detect, or type, then Write
                    {/if}
                  </div>
                </div>
                <div class="rate-display">
                  <span class="rate-value-big bpm-value">
                    {#if bpmTapEstimate != null}
                      {formatBpm(bpmTapEstimate)}
                    {:else if parseBpmInput(bpmDraft) != null}
                      {formatBpm(parseBpmInput(bpmDraft))}
                    {:else}
                      —
                    {/if}
                  </span>
                </div>
              </div>

              <div class="bpm-toolbar">
                <input
                  class="props-input bpm-input"
                  type="text"
                  inputmode="decimal"
                  placeholder="BPM"
                  value={bpmDraft}
                  disabled={!track || bpmBusy || bpmDetecting}
                  oninput={handleBpmDraftInput}
                  onkeydown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      void handleWriteBpm();
                    }
                  }}
                />
                <button
                  type="button"
                  class="action-btn bpm-btn"
                  class:flash={bpmTapFlash}
                  disabled={!track || bpmBusy || bpmDetecting}
                  onclick={handleBpmTap}
                  title="Tap along with the beat (fills BPM field)"
                >
                  Tap{#if bpmTapTimes.length > 0}&nbsp;·&nbsp;{bpmTapTimes.length}{/if}
                </button>
                <button
                  type="button"
                  class="action-btn bpm-btn"
                  disabled={!track || bpmBusy || bpmDetecting}
                  onclick={() => void handleDetectBpm()}
                >
                  {bpmDetecting ? "Detecting…" : "Auto detect"}
                </button>
                <button
                  type="button"
                  class="action-btn action-btn-primary bpm-btn"
                  disabled={!track ||
                    bpmBusy ||
                    bpmDetecting ||
                    parseBpmInput(bpmDraft) == null}
                  onclick={() => void handleWriteBpm()}
                >
                  {bpmBusy ? "Writing…" : "Write"}
                </button>
              </div>
            </div>
          </div>

          <div class="props-actions-bar">
            <div
              class="props-status"
              class:error={!!trackRateError || !!bpmError}
              class:success={(!!trackRateSuccess || !!bpmSuccess) &&
                !trackRateError &&
                !bpmError}
              class:muted={(trackRateOverride != null || bpmValue != null) &&
                !trackRateError &&
                !bpmError &&
                !trackRateSuccess &&
                !bpmSuccess}
            >
              {#if trackRateError}
                {trackRateError}
              {:else if bpmError}
                {bpmError}
              {:else if bpmSuccess}
                {bpmSuccess}
              {:else if trackRateSuccess}
                {trackRateSuccess}
              {:else if trackRateOverride != null}
                Speed override active
              {/if}
            </div>
            <div class="props-actions">
              <button
                type="button"
                class="action-btn"
                onclick={() => void closeWindow()}
              >
                Close
              </button>
            </div>
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>

<!-- styles: TrackPropertiesWindow.css imported in <script> -->
