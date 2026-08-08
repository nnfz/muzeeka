// The effect catalog: one source of truth for what can go in the rack, what a
// fresh instance looks like, and what each field's legal range is.
//
// Every clamp here mirrors a `clamp()` in the matching Rust module. The backend
// clamps again on its own — this copy exists so the UI never shows a value the
// engine would quietly reject.

/** Wire tag for `EffectSettings` in src-tauri/src/dsp_chain.rs. */
export type EffectKind = 'equalizer' | 'filter' | 'limiter';

export const BAND_COUNT = 17;

/** Must match `BAND_FREQUENCIES` in src-tauri/src/equalizer.rs (top band = high-shelf). */
export const BAND_FREQUENCIES = [
  25, 40, 63, 100, 160, 250, 400, 630, 1000, 1600, 2500, 4000, 6300, 10000, 12500, 16000, 20000,
] as const;

export interface EqualizerSettings {
  enabled: boolean;
  preamp_db: number;
  bands_db: number[];
}

/** Resonant LP/HP. Mirrors `FilterSettings` in src-tauri/src/filter.rs. */
export interface FilterSettings {
  enabled: boolean;
  /** Low-pass cutoff. `LP_OPEN_HZ` = stage open. */
  lp_hz: number;
  /** High-pass cutoff. `HP_OPEN_HZ` = stage open. */
  hp_hz: number;
  /** Q for both stages. 0.707 is flat; higher peaks at the cutoff. */
  resonance: number;
}

/** Brickwall limiter — pushes the signal in hard and holds the ceiling (or chops it). */
export interface LimiterSettings {
  enabled: boolean;
  /** Pre-limiter gain, 0..+12 dB. */
  gain_db: number;
  /** Output ceiling in dBFS, -6..0. */
  ceiling_db: number;
  /** Recovery time, 10..1000 ms. */
  release_ms: number;
  /** Chop peaks at the ceiling instead of limiting them. Distortion on purpose. */
  clip: boolean;
}

export interface EffectSettingsMap {
  equalizer: EqualizerSettings;
  filter: FilterSettings;
  limiter: LimiterSettings;
}

/**
 * One rack row, in the exact shape the backend persists and expects.
 *
 * Written as a distributed union so `slot.kind === 'filter'` narrows
 * `slot.settings` to `FilterSettings` — the alternative (a wide settings type)
 * would push casts into every editor.
 */
export type ChainSlot = {
  [K in EffectKind]: {
    id: string;
    /** Row bypass. Folded into the effect's own `enabled` by the backend. */
    enabled: boolean;
    kind: K;
    settings: EffectSettingsMap[K];
  };
}[EffectKind];

/** Per-slot readout from `player_get_dsp_chain_status`. */
export interface SlotStatus {
  id: string;
  kind: EffectKind;
  /** Per-kind meaning; today only the limiter reports anything. */
  meter_db: number;
  process_count: number;
}

export interface DspChainStatus {
  attached: boolean;
  process_count: number;
  slots: SlotStatus[];
}

/** Must match `MAX_SLOTS` in src-tauri/src/dsp_chain.rs. */
export const MAX_SLOTS = 16;

export const LIMITER_MAX_GAIN_DB = 12;

/** Filter cutoffs at these ends count as fully open — see src-tauri/src/filter.rs. */
export const LP_OPEN_HZ = 20000;
export const HP_OPEN_HZ = 20;

const DEFAULT_EQUALIZER: EqualizerSettings = {
  enabled: true,
  preamp_db: 0,
  bands_db: Array(BAND_COUNT).fill(0),
};

/** Both stages open, so a fresh Filter is inaudible until a cutoff moves. */
const DEFAULT_FILTER: FilterSettings = {
  enabled: true,
  lp_hz: LP_OPEN_HZ,
  hp_hz: HP_OPEN_HZ,
  resonance: 0.707,
};

const DEFAULT_LIMITER: LimiterSettings = {
  enabled: true,
  gain_db: 0,
  ceiling_db: -0.3,
  release_ms: 120,
  clip: false,
};

export interface EffectMeta {
  kind: EffectKind;
  label: string;
  /** One line for the catalog row. */
  blurb: string;
}

/** Catalog order — what the picker column lists, top to bottom. */
export const EFFECT_CATALOG: readonly EffectMeta[] = [
  {
    kind: 'equalizer',
    label: 'Equalizer',
    blurb: '17-band graphic EQ with preamp',
  },
  {
    kind: 'filter',
    label: 'Filter',
    blurb: 'Resonant low-pass / high-pass',
  },
  {
    kind: 'limiter',
    label: 'Limiter',
    blurb: 'Brickwall ceiling, or hard clip',
  },
] as const;

export function effectMeta(kind: EffectKind): EffectMeta {
  return EFFECT_CATALOG.find((e) => e.kind === kind) ?? EFFECT_CATALOG[0];
}

function num(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function clampNum(value: unknown, min: number, max: number, fallback: number): number {
  return Math.max(min, Math.min(max, num(value, fallback)));
}

/** Mirrors `EqualizerSettings::clamp` in src-tauri/src/equalizer.rs. */
export function clampEqualizer(s: Partial<EqualizerSettings> | undefined): EqualizerSettings {
  const bands = s?.bands_db ?? [];
  return {
    enabled: s?.enabled !== false,
    preamp_db: clampNum(s?.preamp_db, -15, 15, 0),
    bands_db: Array.from({ length: BAND_COUNT }, (_, i) => clampNum(bands[i], -20, 20, 0)),
  };
}

/** Mirrors `FilterSettings::clamp` in src-tauri/src/filter.rs. */
export function clampFilter(s: Partial<FilterSettings> | undefined): FilterSettings {
  return {
    enabled: s?.enabled !== false,
    lp_hz: clampNum(s?.lp_hz, HP_OPEN_HZ, LP_OPEN_HZ, LP_OPEN_HZ),
    hp_hz: clampNum(s?.hp_hz, HP_OPEN_HZ, LP_OPEN_HZ, HP_OPEN_HZ),
    resonance: clampNum(s?.resonance, 0.5, 8, 0.707),
  };
}

/** Mirrors `LimiterSettings::clamp` in src-tauri/src/limiter.rs. */
export function clampLimiter(s: Partial<LimiterSettings> | undefined): LimiterSettings {
  return {
    enabled: s?.enabled !== false,
    gain_db: clampNum(s?.gain_db, 0, LIMITER_MAX_GAIN_DB, 0),
    ceiling_db: clampNum(s?.ceiling_db, -6, 0, DEFAULT_LIMITER.ceiling_db),
    release_ms: clampNum(s?.release_ms, 10, 1000, DEFAULT_LIMITER.release_ms),
    clip: s?.clip === true,
  };
}

export function defaultSettings<K extends EffectKind>(kind: K): EffectSettingsMap[K];
export function defaultSettings(kind: EffectKind): EqualizerSettings | FilterSettings | LimiterSettings {
  switch (kind) {
    case 'equalizer':
      return { ...DEFAULT_EQUALIZER, bands_db: [...DEFAULT_EQUALIZER.bands_db] };
    case 'filter':
      return { ...DEFAULT_FILTER };
    default:
      return { ...DEFAULT_LIMITER };
  }
}

/** Clamp a whole slot, normalizing an unknown kind to an equalizer. */
export function clampSlot(slot: ChainSlot): ChainSlot {
  const enabled = slot.enabled !== false;
  switch (slot.kind) {
    case 'filter':
      return { id: slot.id, enabled, kind: 'filter', settings: clampFilter(slot.settings) };
    case 'limiter':
      return { id: slot.id, enabled, kind: 'limiter', settings: clampLimiter(slot.settings) };
    default:
      return { id: slot.id, enabled, kind: 'equalizer', settings: clampEqualizer(slot.settings) };
  }
}

let slotSeq = 0;

/**
 * Mint a slot id.
 *
 * Ids are identity in the backend rack: a moved row keeps its DSP node (and so
 * its filter memory) only because its id is unchanged. Uniqueness matters more
 * than readability, hence the counter alongside the timestamp — two clicks in
 * the same millisecond must not collide.
 */
export function mintSlotId(kind: EffectKind): string {
  slotSeq += 1;
  return `${kind}-${Date.now().toString(36)}-${slotSeq.toString(36)}`;
}

export function makeSlot(kind: EffectKind): ChainSlot {
  return clampSlot({
    id: mintSlotId(kind),
    enabled: true,
    kind,
    settings: defaultSettings(kind),
  } as ChainSlot);
}

/** Short right-hand summary for a collapsed rack row. */
export function slotSummary(slot: ChainSlot): string {
  switch (slot.kind) {
    case 'equalizer': {
      const s = slot.settings;
      const touched = s.bands_db.filter((b) => Math.abs(b) > 0.05).length;
      if (!touched && Math.abs(s.preamp_db) < 0.05) return 'Flat';
      const parts: string[] = [];
      if (Math.abs(s.preamp_db) >= 0.05) {
        parts.push(`${s.preamp_db > 0 ? '+' : ''}${s.preamp_db.toFixed(1)} dB pre`);
      }
      if (touched) parts.push(`${touched} band${touched === 1 ? '' : 's'}`);
      return parts.join(' · ');
    }
    case 'filter': {
      const s = slot.settings;
      const lp = s.lp_hz < LP_OPEN_HZ;
      const hp = s.hp_hz > HP_OPEN_HZ;
      if (!lp && !hp) return 'Open';
      const parts: string[] = [];
      if (hp) parts.push(`HP ${formatHz(s.hp_hz)}`);
      if (lp) parts.push(`LP ${formatHz(s.lp_hz)}`);
      if (s.resonance > 0.75) parts.push(`Q ${s.resonance.toFixed(1)}`);
      return parts.join(' · ');
    }
    default: {
      const s = slot.settings;
      const head = s.clip ? 'Clip' : 'Limit';
      return `${head} ${s.ceiling_db.toFixed(1)} dBFS · +${s.gain_db.toFixed(1)} dB`;
    }
  }
}

export function formatHz(hz: number): string {
  if (hz >= 1000) {
    const k = hz / 1000;
    return `${Number.isInteger(k) ? k : k.toFixed(1)}k`;
  }
  return `${Math.round(hz)}`;
}
