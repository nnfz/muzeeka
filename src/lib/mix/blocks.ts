/**
 * Mix editor blocks — transition zone + effect inserts.
 *
 * Timeline: all times are on the **previous** track (same as playhead).
 * Next-track mapping is always `t_next = t_prev + (to.viewStart − from.viewStart)`.
 *
 * The transition block is the container: mixer / heavy FX should only run inside
 * its [start, start+duration] window so the rest of the tracks stay cheap.
 *
 * Effect blocks target either the upper deck (`from`) or lower deck (`to`).
 */

export type MixBlockKind =
  | "transition"
  | "lowpass"
  | "highpass"
  | "volume"
  | "speed"
  | "filter";

/** Which deck an effect applies to. Transition containers use null. */
export type MixBlockTarget = "from" | "to";

/**
 * Automation point along a block.
 * `t` = 0..1 of block duration, `v` = 0..1 normalized param value.
 */
export type EnvelopePoint = {
  t: number;
  v: number;
};

/** How envelope points are joined: straight segments or Catmull-Rom smooth. */
export type EnvelopeCurve = "linear" | "smooth";

export type MixBlockParams = {
  /** 0..1 linear gain (volume block static fallback). */
  gain?: number;
  /** Hz cutoff (filter / lowpass / highpass static fallback). */
  cutoffHz?: number;
  /** 0..1 resonance / Q. */
  resonance?: number;
  /** Primary automation curve (volume→gain, filters→cutoff). */
  envelope?: EnvelopePoint[];
  /** Envelope interpolation mode (default linear). */
  curve?: EnvelopeCurve;
};

export type MixBlock = {
  id: string;
  kind: MixBlockKind;
  /** Start on previous-track timeline (seconds). */
  startFromSecs: number;
  /** Length on the shared mix timeline (seconds). */
  durationSecs: number;
  /** Locked — no move/resize (still selectable). */
  pinned: boolean;
  /**
   * Parent transition id for effect blocks.
   * `null` only for top-level transition containers.
   */
  parentId: string | null;
  /**
   * Deck this effect rides on.
   * `null` for transition containers.
   */
  targetLane: MixBlockTarget | null;
  params: MixBlockParams;
};

export type PaletteItem = {
  kind: MixBlockKind;
  label: string;
  short: string;
  description: string;
  /** Container that hosts effect children. */
  isContainer: boolean;
  /** Available to drag in v1 UI (false = shown greyed “soon”). */
  enabled: boolean;
  accent: string;
};

export const MIX_PALETTE: PaletteItem[] = [
  {
    kind: "transition",
    label: "Transition",
    short: "XF",
    description: "Main mix zone — place effects inside; mixer only works here",
    isContainer: true,
    enabled: true,
    accent: "#7c5cff",
  },
  {
    kind: "lowpass",
    label: "Low-pass",
    short: "LP",
    description: "Low-pass on prev (top) or next (bottom) deck",
    isContainer: false,
    enabled: true,
    accent: "#38bdf8",
  },
  {
    kind: "highpass",
    label: "High-pass",
    short: "HP",
    description: "High-pass on prev (top) or next (bottom) deck",
    isContainer: false,
    enabled: true,
    accent: "#34d399",
  },
  {
    kind: "volume",
    label: "Volume",
    short: "Vol",
    description: "Gain envelope on prev (top) or next (bottom) deck",
    isContainer: false,
    enabled: true,
    accent: "#fbbf24",
  },
  {
    kind: "speed",
    label: "Speed",
    short: "Spd",
    description:
      "Retune this deck (0.5×–2×) to match the other — playhead stays 1× on the graph",
    isContainer: false,
    enabled: true,
    accent: "#fb7185",
  },
  {
    kind: "filter",
    label: "Filter",
    short: "FX",
    description: "Generic filter rack (soon)",
    isContainer: false,
    enabled: false,
    accent: "#f472b6",
  },
];

export const MIN_BLOCK_SECS = 0.25;
export const DEFAULT_TRANSITION_SECS = 8;
export const DEFAULT_EFFECT_SECS = 4;

export function newBlockId(): string {
  return `b_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 7)}`;
}

export function paletteItem(kind: MixBlockKind): PaletteItem | undefined {
  return MIX_PALETTE.find((p) => p.kind === kind);
}

/** Which param the envelope edits for this block kind. */
export function envelopeParamLabel(kind: MixBlockKind): string {
  switch (kind) {
    case "volume":
      return "Gain";
    case "speed":
      return "Rate";
    case "lowpass":
    case "highpass":
    case "filter":
      return "Cutoff";
    default:
      return "Value";
  }
}

export function defaultEnvelope(kind: MixBlockKind): EnvelopePoint[] {
  switch (kind) {
    case "volume":
      // Full → full (user draws fade)
      return [
        { t: 0, v: 1 },
        { t: 1, v: 1 },
      ];
    case "speed":
      // Center = 1.0× (see envelopeVToRate)
      return [
        { t: 0, v: 0.5 },
        { t: 1, v: 0.5 },
      ];
    case "lowpass":
      // Open filter → can sweep down
      return [
        { t: 0, v: 0.85 },
        { t: 1, v: 0.85 },
      ];
    case "highpass":
      return [
        { t: 0, v: 0.15 },
        { t: 1, v: 0.15 },
      ];
    case "filter":
      return [
        { t: 0, v: 0.5 },
        { t: 1, v: 0.5 },
      ];
    default:
      return [
        { t: 0, v: 0.5 },
        { t: 1, v: 0.5 },
      ];
  }
}

export function defaultParams(kind: MixBlockKind): MixBlockParams {
  switch (kind) {
    case "volume":
      return { gain: 1, envelope: defaultEnvelope(kind), curve: "linear" };
    case "speed":
      return { envelope: defaultEnvelope(kind), curve: "linear" };
    case "lowpass":
      return {
        cutoffHz: 12000,
        resonance: 0.2,
        envelope: defaultEnvelope(kind),
        curve: "linear",
      };
    case "highpass":
      return {
        cutoffHz: 80,
        resonance: 0.2,
        envelope: defaultEnvelope(kind),
        curve: "linear",
      };
    case "filter":
      return {
        cutoffHz: 2000,
        resonance: 0.3,
        envelope: defaultEnvelope(kind),
        curve: "linear",
      };
    default:
      return {};
  }
}

/** Normalized envelope 0..1 → playback rate 0.5×..2× (center 0.5 → 1×). */
export function envelopeVToRate(v: number): number {
  const x = Math.max(0, Math.min(1, v));
  // 2^((v-0.5)*2): 0→0.5×, 0.5→1×, 1→2×
  return Math.pow(2, (x - 0.5) * 2);
}

export function normalizeCurve(raw: unknown): EnvelopeCurve {
  return raw === "smooth" ? "smooth" : "linear";
}

/** Map normalized 0..1 envelope value → display number. */
export function envelopeVToDisplay(kind: MixBlockKind, v: number): number {
  const x = Math.max(0, Math.min(1, v));
  switch (kind) {
    case "volume":
      return Math.round(x * 100); // %
    case "speed":
      return Math.round(envelopeVToRate(x) * 100) / 100; // e.g. 1.25
    case "lowpass":
    case "filter": {
      // log 80 Hz .. 18 kHz
      const hz = 80 * Math.pow(18000 / 80, x);
      return Math.round(hz);
    }
    case "highpass": {
      const hz = 20 * Math.pow(8000 / 20, x);
      return Math.round(hz);
    }
    default:
      return Math.round(x * 100);
  }
}

export function formatEnvelopeDisplay(kind: MixBlockKind, v: number): string {
  const n = envelopeVToDisplay(kind, v);
  if (kind === "volume") return `${n}%`;
  if (kind === "speed") return `${n.toFixed(2)}×`;
  return `${n >= 1000 ? `${(n / 1000).toFixed(1)}k` : n} Hz`;
}

export function normalizeEnvelope(points: EnvelopePoint[] | undefined): EnvelopePoint[] {
  if (!points || points.length === 0) {
    return [
      { t: 0, v: 0.5 },
      { t: 1, v: 0.5 },
    ];
  }
  const cleaned = points
    .map((p) => ({
      t: Math.max(0, Math.min(1, Number(p.t) || 0)),
      v: Math.max(0, Math.min(1, Number(p.v) || 0)),
    }))
    .sort((a, b) => a.t - b.t);
  // Always pin endpoints at 0 and 1 (clone nearest if missing).
  if (cleaned[0]!.t > 0.001) cleaned.unshift({ t: 0, v: cleaned[0]!.v });
  else cleaned[0] = { t: 0, v: cleaned[0]!.v };
  const last = cleaned[cleaned.length - 1]!;
  if (last.t < 0.999) cleaned.push({ t: 1, v: last.v });
  else cleaned[cleaned.length - 1] = { t: 1, v: last.v };
  return cleaned;
}

/** Centripetal-ish Catmull-Rom on values (uniform in segment parameter u). */
function catmullRom(p0: number, p1: number, p2: number, p3: number, u: number): number {
  const u2 = u * u;
  const u3 = u2 * u;
  return (
    0.5 *
    (2 * p1 +
      (-p0 + p2) * u +
      (2 * p0 - 5 * p1 + 4 * p2 - p3) * u2 +
      (-p0 + 3 * p1 - 3 * p2 + p3) * u3)
  );
}

export function sampleEnvelope(
  points: EnvelopePoint[] | undefined,
  t: number,
  curve: EnvelopeCurve = "linear",
): number {
  const pts = normalizeEnvelope(points);
  const x = Math.max(0, Math.min(1, t));
  if (x <= pts[0]!.t) return pts[0]!.v;
  for (let i = 1; i < pts.length; i++) {
    const a = pts[i - 1]!;
    const b = pts[i]!;
    if (x <= b.t + 1e-12) {
      const span = b.t - a.t;
      if (span < 1e-9) return b.v;
      const u = (x - a.t) / span;
      if (curve === "smooth" && pts.length >= 2) {
        const p0 = pts[Math.max(0, i - 2)]!.v;
        const p1 = a.v;
        const p2 = b.v;
        const p3 = pts[Math.min(pts.length - 1, i + 1)]!.v;
        return Math.max(0, Math.min(1, catmullRom(p0, p1, p2, p3, u)));
      }
      return a.v + (b.v - a.v) * u;
    }
  }
  return pts[pts.length - 1]!.v;
}

export function defaultDuration(
  kind: MixBlockKind,
  bpm: number | null,
): number {
  if (kind === "transition") {
    if (bpm != null && bpm > 0) {
      // 4 bars of 4/4
      return Math.max(MIN_BLOCK_SECS, (60 / bpm) * 16);
    }
    return DEFAULT_TRANSITION_SECS;
  }
  if (bpm != null && bpm > 0) {
    return Math.max(MIN_BLOCK_SECS, (60 / bpm) * 8);
  }
  return DEFAULT_EFFECT_SECS;
}

export function isContainerKind(kind: MixBlockKind): boolean {
  return kind === "transition";
}

export function createBlock(
  kind: MixBlockKind,
  startFromSecs: number,
  opts?: {
    durationSecs?: number;
    parentId?: string | null;
    targetLane?: MixBlockTarget | null;
    bpm?: number | null;
  },
): MixBlock {
  return {
    id: newBlockId(),
    kind,
    startFromSecs,
    durationSecs: opts?.durationSecs ?? defaultDuration(kind, opts?.bpm ?? null),
    pinned: false,
    parentId: isContainerKind(kind) ? null : (opts?.parentId ?? null),
    targetLane: isContainerKind(kind)
      ? null
      : (opts?.targetLane ?? "from"),
    params: defaultParams(kind),
  };
}

export function blockEnd(b: MixBlock): number {
  return b.startFromSecs + b.durationSecs;
}

export function clampBlock(
  b: MixBlock,
  bounds?: { minStart?: number; maxEnd?: number },
): MixBlock {
  let start = b.startFromSecs;
  let dur = Math.max(MIN_BLOCK_SECS, b.durationSecs);
  const minStart = bounds?.minStart ?? -Infinity;
  const maxEnd = bounds?.maxEnd ?? Infinity;
  if (start < minStart) start = minStart;
  if (start + dur > maxEnd) {
    dur = Math.max(MIN_BLOCK_SECS, maxEnd - start);
  }
  if (start + dur > maxEnd) {
    start = Math.max(minStart, maxEnd - dur);
  }
  return { ...b, startFromSecs: start, durationSecs: dur };
}

/** Top-level transition blocks only. */
export function transitionBlocks(blocks: MixBlock[]): MixBlock[] {
  return blocks.filter((b) => b.kind === "transition" && b.parentId == null);
}

export function childrenOf(blocks: MixBlock[], parentId: string): MixBlock[] {
  return blocks
    .filter((b) => b.parentId === parentId)
    .sort((a, b) => a.startFromSecs - b.startFromSecs);
}

export function childrenOnLane(
  blocks: MixBlock[],
  parentId: string,
  lane: MixBlockTarget,
): MixBlock[] {
  return childrenOf(blocks, parentId).filter(
    (b) => (b.targetLane ?? "from") === lane,
  );
}

/** Transition that contains time `t` (prev-track secs), if any. */
export function transitionAt(
  blocks: MixBlock[],
  t: number,
): MixBlock | null {
  for (const b of transitionBlocks(blocks)) {
    if (t >= b.startFromSecs - 1e-6 && t <= blockEnd(b) + 1e-6) return b;
  }
  return null;
}

/** Primary transition for preview/mix (first by start time). */
export function primaryTransition(blocks: MixBlock[]): MixBlock | null {
  const list = transitionBlocks(blocks);
  if (list.length === 0) return null;
  return [...list].sort((a, b) => a.startFromSecs - b.startFromSecs)[0]!;
}

export function serializeBlocks(blocks: MixBlock[]): MixBlock[] {
  return blocks.map((b) => ({
    id: b.id,
    kind: b.kind,
    startFromSecs: b.startFromSecs,
    durationSecs: b.durationSecs,
    pinned: !!b.pinned,
    parentId: b.parentId,
    targetLane: b.targetLane,
    params: {
      ...b.params,
      curve: normalizeCurve(b.params.curve),
      envelope: b.params.envelope
        ? normalizeEnvelope(b.params.envelope)
        : undefined,
    },
  }));
}

function parseTargetLane(raw: unknown, kind: MixBlockKind): MixBlockTarget | null {
  if (isContainerKind(kind)) return null;
  if (raw === "to" || raw === "from") return raw;
  return "from";
}

function parseEnvelope(raw: unknown): EnvelopePoint[] | undefined {
  if (!Array.isArray(raw)) return undefined;
  const pts: EnvelopePoint[] = [];
  for (const p of raw) {
    if (!p || typeof p !== "object") continue;
    const o = p as Record<string, unknown>;
    if (typeof o.t !== "number" || typeof o.v !== "number") continue;
    pts.push({ t: o.t, v: o.v });
  }
  return pts.length >= 1 ? normalizeEnvelope(pts) : undefined;
}

export function parseBlocks(raw: unknown): MixBlock[] {
  if (!Array.isArray(raw)) return [];
  const out: MixBlock[] = [];
  for (const item of raw) {
    if (!item || typeof item !== "object") continue;
    const o = item as Record<string, unknown>;
    const kind = o.kind as MixBlockKind;
    if (!MIX_PALETTE.some((p) => p.kind === kind)) continue;
    if (typeof o.id !== "string" || !o.id) continue;
    if (typeof o.startFromSecs !== "number" || !Number.isFinite(o.startFromSecs))
      continue;
    if (typeof o.durationSecs !== "number" || !Number.isFinite(o.durationSecs))
      continue;
    const base = defaultParams(kind);
    const paramsIn =
      o.params && typeof o.params === "object"
        ? (o.params as MixBlockParams)
        : {};
    const envelope =
      parseEnvelope(paramsIn.envelope) ??
      (isContainerKind(kind) ? undefined : defaultEnvelope(kind));
    out.push({
      id: o.id,
      kind,
      startFromSecs: o.startFromSecs,
      durationSecs: Math.max(MIN_BLOCK_SECS, o.durationSecs),
      pinned: o.pinned === true,
      parentId:
        typeof o.parentId === "string"
          ? o.parentId
          : o.parentId === null
            ? null
            : null,
      targetLane: parseTargetLane(o.targetLane, kind),
      params: {
        ...base,
        ...paramsIn,
        envelope,
        curve: normalizeCurve(paramsIn.curve ?? base.curve),
      },
    });
  }
  return out;
}
