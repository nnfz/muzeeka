import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';
import { getContext, setContext } from 'svelte';
import { clampPlaybackRate, setCachedGlobalPlaybackRate } from '$lib/trackPrefs';
import { reorderItemsAtBoundary } from '$lib/trackOrder';
import {
  BAND_COUNT,
  MAX_SLOTS,
  clampSlot,
  defaultSettings,
  makeSlot,
  mintSlotId,
  type ChainSlot,
  type DspChainStatus,
  type EffectKind,
  type EffectSettingsMap,
  type EqualizerSettings,
  type LimiterSettings,
} from '$lib/dsp/effects';

/**
 * A partial patch for any one effect's settings.
 *
 * Union of partials rather than a partial of the union: an editor only ever
 * sends its own fields, and `clampSlot` drops anything foreign to the slot's
 * kind on the way out.
 */
export type EffectPatch =
  | Partial<EffectSettingsMap['equalizer']>
  | Partial<EffectSettingsMap['filter']>
  | Partial<EffectSettingsMap['limiter']>;

export {
  BAND_COUNT,
  BAND_FREQUENCIES,
  LIMITER_MAX_GAIN_DB,
  type ChainSlot,
  type DspChainStatus,
  type EffectKind,
  type EqualizerSettings,
  type FilterSettings,
  type LimiterSettings,
  type SlotStatus,
} from '$lib/dsp/effects';

/** Classic random vs no-repeat until every track in the playlist has played. */
export type ShuffleMode = 'normal' | 'smart';

export interface AppSettings {
  /** The effect rack. Absent only in a settings file written before the rack. */
  dsp_chain?: ChainSlot[] | null;
  /** Legacy singletons — read once at migration time, then never written again. */
  equalizer?: EqualizerSettings;
  limiter?: LimiterSettings;
  playback_rate?: number;
  pitch_enabled?: boolean;
  /** EQ presets (legacy field name). */
  custom_presets?: EQPreset[];
  filter_presets?: FilterPreset[];
  limiter_presets?: LimiterPreset[];
  chain_presets?: ChainPreset[];
  download_folder?: string | null;
  download_playlist_id?: string | null;
  discord_rpc_enabled?: boolean;
  shuffle_mode?: ShuffleMode;
  developer_mode?: boolean;
}

export interface EQPreset {
  name: string;
  preamp_db: number;
  bands_db: number[];
}

export interface FilterPreset {
  name: string;
  lp_hz: number;
  hp_hz: number;
  resonance: number;
}

export interface LimiterPreset {
  name: string;
  gain_db: number;
  ceiling_db: number;
  release_ms: number;
  clip: boolean;
}

/** Full rack snapshot. Slot ids are reminted when the preset is applied. */
export interface ChainPreset {
  name: string;
  slots: ChainSlot[];
}

export type EffectPreset = EQPreset | FilterPreset | LimiterPreset;

/** Global playback rate changed — every window mirrors it (see `bindGlobalRateSync`). */
const RATE_EVENT = 'settings:playback-rate';

/** The rack, in order. Slots are plain values; identity comes from `id`. */
let dspChain = $state<ChainSlot[]>([]);
/** User-saved EQ presets only — no factory defaults. */
let customPresets = $state<EQPreset[]>([]);
let filterPresets = $state<FilterPreset[]>([]);
let limiterPresets = $state<LimiterPreset[]>([]);
let chainPresets = $state<ChainPreset[]>([]);
let playbackRate = $state(1.0);
let pitchEnabled = $state(true);
let downloadFolder = $state<string | null>(null);
let downloadPlaylistId = $state<string | null>(null);
let discordRpcEnabled = $state(true);
/** Default smart: avoid replaying tracks until the playlist cycle is complete. */
let shuffleMode = $state<ShuffleMode>('smart');
let developerMode = $state(false);
let defaultDownloadFolder = $state<string | null>(null);
let isReady = $state(false);
let saveTimer: ReturnType<typeof setTimeout> | null = null;
/** Coalesce slider spam so backend gets one settled target and a full smooth ramp. */
let rateApplyTimer: ReturnType<typeof setTimeout> | null = null;
let rateApplySeq = 0;
let rateSyncBound = false;

/**
 * Mirror the Settings global rate into this window.
 *
 * Every webview runs its own module instance, so the rate the Settings window sets
 * would otherwise stay invisible here — and the main window pushes its own stale
 * copy back into the player on the next track change (`applyEffectivePlaybackRate`),
 * which is what made a global rate "work once and reset".
 *
 * Listener only mirrors state; it never re-applies or re-emits, so the broadcast
 * that reaches its own sender is harmless.
 */
function bindGlobalRateSync() {
  if (rateSyncBound || typeof window === 'undefined') return;
  rateSyncBound = true;
  void listen<{ rate: number }>(RATE_EVENT, (event) => {
    const rate = clampPlaybackRate(event.payload?.rate ?? 1);
    playbackRate = rate;
    setCachedGlobalPlaybackRate(rate);
  }).catch(() => {
    rateSyncBound = false;
  });
}

function parseShuffleMode(value: unknown): ShuffleMode {
  return value === 'normal' ? 'normal' : 'smart';
}

function scheduleSave() {
  if (!isReady) return;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = null;
    const payload: AppSettings = {
      dsp_chain: dspChain.map((slot) => clampSlot(slot)),
      playback_rate: playbackRate,
      pitch_enabled: pitchEnabled,
      custom_presets: customPresets.map((p) => ({
        name: p.name,
        preamp_db: p.preamp_db,
        bands_db: [...p.bands_db],
      })),
      filter_presets: filterPresets.map((p) => ({ ...p })),
      limiter_presets: limiterPresets.map((p) => ({ ...p })),
      chain_presets: chainPresets.map((p) => ({
        name: p.name,
        slots: p.slots.map((s) => clampSlot(s)),
      })),
      download_folder: downloadFolder,
      download_playlist_id: downloadPlaylistId,
      discord_rpc_enabled: discordRpcEnabled,
      shuffle_mode: shuffleMode,
      developer_mode: developerMode,
    };
    invoke('settings_save', { data: payload }).catch((e) => {
      console.error('Failed to save settings:', e);
    });
  }, 250);
}

/**
 * Push the whole rack to the player.
 *
 * Every mutation (add, remove, reorder, bypass, slider drag) funnels through
 * here. The backend reuses DSP nodes by id, so a reorder or a drag allocates
 * nothing and never clicks; only the empty↔non-empty transition touches BASS.
 */
async function applyChain() {
  const clamped = dspChain.slice(0, MAX_SLOTS).map((slot) => clampSlot(slot));
  dspChain = clamped;
  try {
    await invoke('player_set_dsp_chain', { slots: clamped });
  } catch (e) {
    console.error('Failed to apply DSP chain:', e);
    throw e;
  }
  scheduleSave();
}

/**
 * Apply playback rate to the player.
 * - UI updates immediately.
 * - Backend is debounced while dragging so one full smooth ramp runs to the final value.
 * - Pass `immediate: true` for presets / explicit commits.
 */
async function applyPlaybackRate(rate: number, opts?: { immediate?: boolean }) {
  const clamped = Math.max(0.25, Math.min(2, rate));
  playbackRate = clamped;
  setCachedGlobalPlaybackRate(clamped);

  const send = async (value: number) => {
    const seq = ++rateApplySeq;
    // Other windows hold their own copy of the global rate — broadcast on the same
    // (debounced) beat as the player push, so dragging the slider stays cheap.
    void emit(RATE_EVENT, { rate: value }).catch(() => {});
    try {
      await invoke('player_set_playback_rate', { rate: value });
    } catch (e) {
      console.error('Failed to set playback rate:', e);
    }
    // Only the latest request may schedule a settings write.
    if (seq === rateApplySeq) {
      scheduleSave();
    }
  };

  if (rateApplyTimer) {
    clearTimeout(rateApplyTimer);
    rateApplyTimer = null;
  }

  if (opts?.immediate) {
    await send(clamped);
    return;
  }

  // Trailing debounce: continuous slider input → one ramp after settle.
  rateApplyTimer = setTimeout(() => {
    rateApplyTimer = null;
    void send(playbackRate);
  }, 120);
}

async function applyPitchEnabled(enabled: boolean) {
  pitchEnabled = enabled;
  try {
    await invoke('player_set_pitch_enabled', { enabled });
  } catch (e) {
    console.error('Failed to set pitch mode:', e);
  }
  scheduleSave();
}

export function createSettingsStore(
  ensurePlayerReady: () => Promise<void>,
  opts?: { applyToPlayer?: boolean },
) {
  /** Main window applies the rack/rate/pitch on boot. Secondary windows only display state. */
  const applyToPlayer = opts?.applyToPlayer !== false;

  bindGlobalRateSync();

  async function bootstrap() {
    try {
      const data = await invoke<AppSettings>('settings_load');
      // The backend migrates a pre-rack file into a chain before it answers, so
      // `dsp_chain` is only missing if the load itself failed.
      dspChain = Array.isArray(data.dsp_chain)
        ? data.dsp_chain.slice(0, MAX_SLOTS).map((slot) => clampSlot(slot))
        : [];
      if (Array.isArray(data.custom_presets)) {
        customPresets = data.custom_presets.map((p) => {
          const b = p.bands_db ?? [];
          return {
            name: p.name,
            preamp_db: p.preamp_db ?? 0,
            bands_db: Array.from({ length: BAND_COUNT }, (_, i) => b[i] ?? 0),
          };
        });
      }
      if (Array.isArray(data.filter_presets)) {
        filterPresets = data.filter_presets.map((p) => ({
          name: p.name,
          lp_hz: p.lp_hz ?? 20000,
          hp_hz: p.hp_hz ?? 20,
          resonance: p.resonance ?? 0.707,
        }));
      }
      if (Array.isArray(data.limiter_presets)) {
        limiterPresets = data.limiter_presets.map((p) => ({
          name: p.name,
          gain_db: p.gain_db ?? 0,
          ceiling_db: p.ceiling_db ?? -0.3,
          release_ms: p.release_ms ?? 120,
          clip: !!p.clip,
        }));
      }
      if (Array.isArray(data.chain_presets)) {
        chainPresets = data.chain_presets.map((p) => ({
          name: p.name,
          slots: Array.isArray(p.slots)
            ? p.slots.slice(0, MAX_SLOTS).map((s) => clampSlot(s as ChainSlot))
            : [],
        }));
      }
      if (typeof data.playback_rate === 'number' && data.playback_rate > 0) {
        playbackRate = Math.max(0.25, Math.min(2, data.playback_rate));
      } else {
        playbackRate = 1.0;
      }
      setCachedGlobalPlaybackRate(playbackRate);
      pitchEnabled = data.pitch_enabled !== false;
      if (typeof data.download_folder === 'string' && data.download_folder.trim()) {
        downloadFolder = data.download_folder.trim();
      } else {
        downloadFolder = null;
      }
      if (typeof data.download_playlist_id === 'string' && data.download_playlist_id.trim()) {
        downloadPlaylistId = data.download_playlist_id.trim();
      } else {
        downloadPlaylistId = null;
      }
      discordRpcEnabled = data.discord_rpc_enabled !== false;
      shuffleMode = parseShuffleMode(data.shuffle_mode);
      developerMode = data.developer_mode === true;
      try {
        defaultDownloadFolder = await invoke<string>('ytdlp_default_download_dir');
      } catch {
        defaultDownloadFolder = null;
      }
      // Never re-apply DSP from the settings/download webview on open: that races the
      // main player, can rebuild pitch topology mid-playback, and freezes the UI via
      // run_on_main_thread while BASS is busy.
      // On main window: re-push the rack (cheap — one ArcSwap store unless it has to
      // attach). Rate/pitch only when non-default: backend no-ops when already
      // matching, so this is safe after Ctrl+R.
      if (applyToPlayer) {
        await ensurePlayerReady();
        // Backend already applied the saved rack during setup; re-push only when
        // there is one, so an empty rack costs no IPC on every reload.
        if (dspChain.length) {
          await invoke('player_set_dsp_chain', { slots: dspChain }).catch(() => {});
        }
        if (playbackRate !== 1.0) {
          await invoke('player_set_playback_rate', { rate: playbackRate }).catch(() => {});
        }
        // pitch_enabled defaults true on backend; only push when user disabled pitch
        // (avoids a redundant IPC on every reload).
        if (!pitchEnabled) {
          await invoke('player_set_pitch_enabled', { enabled: false }).catch(() => {});
        }
      }
    } catch (e) {
      console.error('Failed to load settings:', e);
    } finally {
      isReady = true;
    }
  }

  void bootstrap();

  return {
    get dspChain() {
      return dspChain;
    },
    get playbackRate() {
      return playbackRate;
    },
    get pitchEnabled() {
      return pitchEnabled;
    },
    get customPresets() {
      return [...customPresets];
    },
    get filterPresets() {
      return [...filterPresets];
    },
    get limiterPresets() {
      return [...limiterPresets];
    },
    get chainPresets() {
      return chainPresets.map((p) => ({
        name: p.name,
        slots: p.slots.map((s) => clampSlot(s)),
      }));
    },
    /** User presets for a given effect kind. */
    presetsFor(kind: EffectKind): EffectPreset[] {
      if (kind === 'equalizer') return [...customPresets];
      if (kind === 'filter') return [...filterPresets];
      return [...limiterPresets];
    },
    get downloadFolder() {
      return downloadFolder;
    },
    get downloadPlaylistId() {
      return downloadPlaylistId;
    },
    get discordRpcEnabled() {
      return discordRpcEnabled;
    },
    get shuffleMode() {
      return shuffleMode;
    },
    get developerMode() {
      return developerMode;
    },
    get effectiveDownloadFolder() {
      return downloadFolder ?? defaultDownloadFolder ?? '';
    },
    setDownloadFolder(folder: string | null) {
      downloadFolder = folder?.trim() || null;
      scheduleSave();
    },
    setDownloadPlaylistId(id: string | null) {
      downloadPlaylistId = id?.trim() || null;
      scheduleSave();
    },
    setDiscordRpcEnabled(enabled: boolean) {
      discordRpcEnabled = enabled;
      scheduleSave();
    },
    setShuffleMode(mode: ShuffleMode) {
      shuffleMode = parseShuffleMode(mode);
      scheduleSave();
    },
    setDeveloperMode(enabled: boolean) {
      developerMode = enabled;
      scheduleSave();
    },
    // ── Effect rack ────────────────────────────────────────────────────────────
    /** Insert a fresh effect at `atIndex` (default: append). Returns its slot id. */
    async addEffect(kind: EffectKind, atIndex?: number): Promise<string | null> {
      if (dspChain.length >= MAX_SLOTS) return null;
      const slot = makeSlot(kind);
      const at =
        atIndex === undefined ? dspChain.length : Math.max(0, Math.min(atIndex, dspChain.length));
      dspChain = [...dspChain.slice(0, at), slot, ...dspChain.slice(at)];
      await applyChain();
      return slot.id;
    },
    async removeSlot(id: string) {
      const next = dspChain.filter((slot) => slot.id !== id);
      if (next.length === dspChain.length) return;
      dspChain = next;
      await applyChain();
    },
    /**
     * Move a slot to the boundary `insertIndex` of the *current* list.
     *
     * Boundary, not target index: `insertIndex` counts gaps between rows, which
     * is what a drop line points at. `reorderItemsAtBoundary` handles the shift
     * from removing the row before re-inserting it.
     */
    async moveSlot(id: string, insertIndex: number) {
      const next = reorderItemsAtBoundary(dspChain, [id], insertIndex, (slot) => slot.id);
      if (next.every((slot, i) => slot.id === dspChain[i]?.id)) return;
      dspChain = next;
      await applyChain();
    },
    /** Row bypass. The backend folds this into the effect's own `enabled`. */
    async setSlotEnabled(id: string, enabled: boolean) {
      dspChain = dspChain.map((slot) => (slot.id === id ? { ...slot, enabled } : slot));
      await applyChain();
    },
    /**
     * Patch one slot's settings. Keys foreign to the slot's kind are dropped by
     * `clampSlot`, so an editor can only ever change its own effect.
     */
    async updateSlot(id: string, patch: EffectPatch) {
      dspChain = dspChain.map((slot) =>
        slot.id === id ? ({ ...slot, settings: { ...slot.settings, ...patch } } as ChainSlot) : slot,
      );
      await applyChain();
    },
    /** Back to the effect's defaults, staying enabled and keeping its position. */
    async resetSlot(id: string) {
      dspChain = dspChain.map((slot) =>
        slot.id === id
          ? ({ ...slot, enabled: true, settings: defaultSettings(slot.kind) } as ChainSlot)
          : slot,
      );
      await applyChain();
    },
    async clearChain() {
      if (!dspChain.length) return;
      dspChain = [];
      await applyChain();
    },
    async fetchChainStatus(): Promise<DspChainStatus | null> {
      try {
        return await invoke<DspChainStatus>('player_get_dsp_chain_status');
      } catch {
        return null;
      }
    },

    // ── Per-effect user presets (no factory defaults) ──────────────────────────
    async applyPreset(slotId: string, name: string) {
      const slot = dspChain.find((s) => s.id === slotId);
      if (!slot) return;
      if (slot.kind === 'equalizer') {
        const preset = customPresets.find((p) => p.name === name);
        if (!preset) return;
        dspChain = dspChain.map((s) =>
          s.id === slotId && s.kind === 'equalizer'
            ? {
                ...s,
                enabled: true,
                settings: {
                  enabled: true,
                  preamp_db: preset.preamp_db,
                  bands_db: [...preset.bands_db],
                },
              }
            : s,
        );
      } else if (slot.kind === 'filter') {
        const preset = filterPresets.find((p) => p.name === name);
        if (!preset) return;
        dspChain = dspChain.map((s) =>
          s.id === slotId && s.kind === 'filter'
            ? {
                ...s,
                enabled: true,
                settings: {
                  enabled: true,
                  lp_hz: preset.lp_hz,
                  hp_hz: preset.hp_hz,
                  resonance: preset.resonance,
                },
              }
            : s,
        );
      } else {
        const preset = limiterPresets.find((p) => p.name === name);
        if (!preset) return;
        dspChain = dspChain.map((s) =>
          s.id === slotId && s.kind === 'limiter'
            ? {
                ...s,
                enabled: true,
                settings: {
                  enabled: true,
                  gain_db: preset.gain_db,
                  ceiling_db: preset.ceiling_db,
                  release_ms: preset.release_ms,
                  clip: preset.clip,
                },
              }
            : s,
        );
      }
      await applyChain();
    },
    async savePreset(slotId: string, name: string) {
      const trimmed = name.trim();
      if (!trimmed) return;
      const slot = dspChain.find((s) => s.id === slotId);
      if (!slot) return;
      if (slot.kind === 'equalizer') {
        const newPreset: EQPreset = {
          name: trimmed,
          preamp_db: slot.settings.preamp_db,
          bands_db: [...slot.settings.bands_db],
        };
        customPresets = [...customPresets.filter((p) => p.name !== trimmed), newPreset];
      } else if (slot.kind === 'filter') {
        const newPreset: FilterPreset = {
          name: trimmed,
          lp_hz: slot.settings.lp_hz,
          hp_hz: slot.settings.hp_hz,
          resonance: slot.settings.resonance,
        };
        filterPresets = [...filterPresets.filter((p) => p.name !== trimmed), newPreset];
      } else {
        const newPreset: LimiterPreset = {
          name: trimmed,
          gain_db: slot.settings.gain_db,
          ceiling_db: slot.settings.ceiling_db,
          release_ms: slot.settings.release_ms,
          clip: slot.settings.clip,
        };
        limiterPresets = [...limiterPresets.filter((p) => p.name !== trimmed), newPreset];
      }
      scheduleSave();
    },
    async deletePreset(kind: EffectKind, name: string) {
      if (kind === 'equalizer') {
        customPresets = customPresets.filter((p) => p.name !== name);
      } else if (kind === 'filter') {
        filterPresets = filterPresets.filter((p) => p.name !== name);
      } else {
        limiterPresets = limiterPresets.filter((p) => p.name !== name);
      }
      scheduleSave();
    },

    // ── Full chain presets ─────────────────────────────────────────────────────
    async applyChainPreset(name: string) {
      const preset = chainPresets.find((p) => p.name === name);
      if (!preset) return;
      // Remint ids so applying a preset always builds fresh nodes rather than
      // colliding with whatever is currently in the rack.
      dspChain = preset.slots.slice(0, MAX_SLOTS).map((slot) =>
        clampSlot({
          ...slot,
          id: mintSlotId(slot.kind),
        } as ChainSlot),
      );
      await applyChain();
    },
    async saveChainPreset(name: string) {
      const trimmed = name.trim();
      if (!trimmed) return;
      const newPreset: ChainPreset = {
        name: trimmed,
        slots: dspChain.map((slot) => clampSlot(slot)),
      };
      chainPresets = [...chainPresets.filter((p) => p.name !== trimmed), newPreset];
      scheduleSave();
    },
    async deleteChainPreset(name: string) {
      chainPresets = chainPresets.filter((p) => p.name !== name);
      scheduleSave();
    },

    async setPlaybackRate(rate: number, opts?: { immediate?: boolean }) {
      await applyPlaybackRate(rate, opts);
    },
    async setPitchEnabled(enabled: boolean) {
      await applyPitchEnabled(enabled);
    },
  };
}

/** Compare two chains for preset matching — order, kinds, bypass, settings (not ids). */
export function chainsMatch(a: ChainSlot[], b: ChainSlot[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    const x = a[i]!;
    const y = b[i]!;
    if (x.kind !== y.kind || x.enabled !== y.enabled) return false;
    if (x.kind === 'equalizer' && y.kind === 'equalizer') {
      if (Math.abs(x.settings.preamp_db - y.settings.preamp_db) >= 0.05) return false;
      if (x.settings.bands_db.length !== y.settings.bands_db.length) return false;
      if (!x.settings.bands_db.every((g, j) => Math.abs(g - (y.settings.bands_db[j] ?? 0)) < 0.05)) {
        return false;
      }
    } else if (x.kind === 'filter' && y.kind === 'filter') {
      if (Math.abs(x.settings.lp_hz - y.settings.lp_hz) >= 1) return false;
      if (Math.abs(x.settings.hp_hz - y.settings.hp_hz) >= 1) return false;
      if (Math.abs(x.settings.resonance - y.settings.resonance) >= 0.02) return false;
    } else if (x.kind === 'limiter' && y.kind === 'limiter') {
      if (Math.abs(x.settings.gain_db - y.settings.gain_db) >= 0.05) return false;
      if (Math.abs(x.settings.ceiling_db - y.settings.ceiling_db) >= 0.05) return false;
      if (Math.abs(x.settings.release_ms - y.settings.release_ms) >= 1) return false;
      if (x.settings.clip !== y.settings.clip) return false;
    } else {
      return false;
    }
  }
  return true;
}

const SETTINGS_KEY = Symbol('settings');

export function setSettingsStore(store: ReturnType<typeof createSettingsStore>) {
  setContext(SETTINGS_KEY, store);
}

export function getSettingsStore() {
  return getContext<ReturnType<typeof createSettingsStore>>(SETTINGS_KEY);
}

/** Read download settings without Svelte context (safe from async handlers / stores). */
export function readDownloadSettings(): {
  downloadFolder: string | null;
  downloadPlaylistId: string | null;
} {
  return {
    downloadFolder,
    downloadPlaylistId,
  };
}

/** Current shuffle algorithm (safe outside Svelte context). */
export function readShuffleMode(): ShuffleMode {
  return shuffleMode;
}

export function applyShuffleModeFromSettings(mode: unknown): ShuffleMode {
  shuffleMode = parseShuffleMode(mode);
  return shuffleMode;
}
