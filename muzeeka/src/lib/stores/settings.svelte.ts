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
  custom_presets?: EQPreset[];
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
let isReady = $state(false);
let saveTimer: ReturnType<typeof setTimeout> | null = null;

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
      custom_presets: customPresets.map((p) => ({
        name: p.name,
        preamp_db: p.preamp_db,
        bands_db: [...p.bands_db],
      })),
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
      await ensurePlayerReady();
      await invoke('player_set_equalizer', { settings: equalizer });
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
    get customPresets() {
      return [...customPresets];
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
  };
}

const SETTINGS_KEY = Symbol('settings');

export function setSettingsStore(store: ReturnType<typeof createSettingsStore>) {
  setContext(SETTINGS_KEY, store);
}

export function getSettingsStore() {
  return getContext<ReturnType<typeof createSettingsStore>>(SETTINGS_KEY);
}