import { invoke } from '@tauri-apps/api/core';
import { getContext, setContext } from 'svelte';

export const BAND_COUNT = 15;

export const BAND_FREQUENCIES = [
  25, 40, 63, 100, 160, 250, 400, 630, 1000, 1600, 2500, 4000, 6300, 10000, 16000,
] as const;

export interface EqualizerSettings {
  enabled: boolean;
  preamp_db: number;
  bands_db: number[];
}

export interface AppSettings {
  equalizer: EqualizerSettings;
  playback_rate?: number;
  pitch_enabled?: boolean;
  custom_presets?: EQPreset[];
  download_folder?: string | null;
  download_playlist_id?: string | null;
  discord_rpc_enabled?: boolean;
}

export interface EQPreset {
  name: string;
  preamp_db: number;
  bands_db: number[];
}


const DEFAULT_EQUALIZER: EqualizerSettings = {
  enabled: false,
  preamp_db: 0,
  bands_db: Array(BAND_COUNT).fill(0),
};

let equalizer = $state<EqualizerSettings>({ ...DEFAULT_EQUALIZER, bands_db: [...DEFAULT_EQUALIZER.bands_db] });
let customPresets = $state<EQPreset[]>([]);
let playbackRate = $state(1.0);
let pitchEnabled = $state(true);
let downloadFolder = $state<string | null>(null);
let downloadPlaylistId = $state<string | null>(null);
let discordRpcEnabled = $state(true);
let defaultDownloadFolder = $state<string | null>(null);
let isReady = $state(false);
let saveTimer: ReturnType<typeof setTimeout> | null = null;
/** Coalesce slider spam so backend gets one settled target and a full smooth ramp. */
let rateApplyTimer: ReturnType<typeof setTimeout> | null = null;
let rateApplySeq = 0;

function clampEqualizer(settings: EqualizerSettings): EqualizerSettings {
  return {
    enabled: settings.enabled,
    preamp_db: Math.max(-15, Math.min(15, settings.preamp_db)),
    bands_db: settings.bands_db.map((g) => Math.max(-20, Math.min(20, g))),
  };
}

function scheduleSave() {
  if (!isReady) return;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = null;
    const payload: AppSettings = {
      equalizer: clampEqualizer(equalizer),
      playback_rate: playbackRate,
      pitch_enabled: pitchEnabled,
      custom_presets: customPresets.map((p) => ({
        name: p.name,
        preamp_db: p.preamp_db,
        bands_db: [...p.bands_db],
      })),
      download_folder: downloadFolder,
      download_playlist_id: downloadPlaylistId,
      discord_rpc_enabled: discordRpcEnabled,
    };
    invoke('settings_save', { data: payload }).catch((e) => {
      console.error('Failed to save settings:', e);
    });
  }, 250);
}

async function applyEqualizer(settings: EqualizerSettings) {
  const clamped = clampEqualizer(settings);
  equalizer = {
    enabled: clamped.enabled,
    preamp_db: clamped.preamp_db,
    bands_db: [...clamped.bands_db],
  };
  try {
    await invoke('player_set_equalizer', { settings: clamped });
  } catch (e) {
    console.error('Failed to apply equalizer:', e);
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

  const send = async (value: number) => {
    const seq = ++rateApplySeq;
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

export function createSettingsStore(ensurePlayerReady: () => Promise<void>) {
  async function bootstrap() {
    try {
      const data = await invoke<AppSettings>('settings_load');
      if (data.equalizer) {
        const bands = data.equalizer.bands_db ?? [];
        equalizer = clampEqualizer({
          enabled: data.equalizer.enabled ?? false,
          preamp_db: data.equalizer.preamp_db ?? 0,
          bands_db: Array.from({ length: BAND_COUNT }, (_, i) => bands[i] ?? 0),
        });
      }
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
      if (typeof data.playback_rate === 'number' && data.playback_rate > 0) {
        playbackRate = Math.max(0.25, Math.min(2, data.playback_rate));
      } else {
        playbackRate = 1.0;
      }
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
      try {
        defaultDownloadFolder = await invoke<string>('ytdlp_default_download_dir');
      } catch {
        defaultDownloadFolder = null;
      }
      await ensurePlayerReady();
      await invoke('player_set_equalizer', { settings: equalizer });
      if (playbackRate !== 1.0) {
        await invoke('player_set_playback_rate', { rate: playbackRate }).catch(() => {});
      }
      await invoke('player_set_pitch_enabled', { enabled: pitchEnabled }).catch(() => {});
    } catch (e) {
      console.error('Failed to load settings:', e);
    } finally {
      isReady = true;
    }
  }

  void bootstrap();

  return {
    get equalizer() {
      return equalizer;
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
    get downloadFolder() {
      return downloadFolder;
    },
    get downloadPlaylistId() {
      return downloadPlaylistId;
    },
    get discordRpcEnabled() {
      return discordRpcEnabled;
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
    async setEqualizerEnabled(enabled: boolean) {
      await applyEqualizer({ ...equalizer, enabled });
    },
    async setPreamp(db: number) {
      await applyEqualizer({ ...equalizer, preamp_db: db, enabled: true });
    },
    async setBandGain(index: number, db: number) {
      const bands_db = [...equalizer.bands_db];
      bands_db[index] = db;
      await applyEqualizer({ ...equalizer, bands_db, enabled: true });
    },
    async resetEqualizer() {
      await applyEqualizer({ ...DEFAULT_EQUALIZER, bands_db: [...DEFAULT_EQUALIZER.bands_db] });
    },
    async applyPreset(name: string) {
      const p = customPresets.find((p) => p.name === name);
      if (!p) return;
      await applyEqualizer({
        enabled: true,
        preamp_db: p.preamp_db,
        bands_db: [...p.bands_db],
      });
    },
    async savePreset(name: string) {
      const trimmed = name.trim();
      if (!trimmed) return;
      const newPreset: EQPreset = {
        name: trimmed,
        preamp_db: equalizer.preamp_db,
        bands_db: [...equalizer.bands_db],
      };
      // Overwrite if same name exists (put at end to indicate recently saved)
      customPresets = [
        ...customPresets.filter((p) => p.name !== trimmed),
        newPreset,
      ];
      scheduleSave();
    },
    async deletePreset(name: string) {
      customPresets = customPresets.filter((p) => p.name !== name);
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