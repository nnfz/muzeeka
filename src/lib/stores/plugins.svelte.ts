import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export interface HttpStatus {
  enabled: boolean;
  running: boolean;
  port: number;
  local_ip: string | null;
  local_ips: string[];
  urls: string[];
  last_error: string | null;
}

export type PluginSettingType = 'number' | 'boolean' | 'string';

export interface PluginSettingSpec {
  key: string;
  type: PluginSettingType;
  label: string;
  description: string;
  min?: number | null;
  max?: number | null;
  default?: unknown;
}

export type PluginRuntime = 'js' | 'native';

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  author: string;
  description: string;
  enabled: boolean;
  permissions: string[];
  settings: PluginSettingSpec[];
  config: Record<string, unknown>;
  runtime: PluginRuntime;
  error: string | null;
  http: HttpStatus;
}

let plugins = $state<PluginInfo[]>([]);
let pluginsDir = $state('');
let ready = $state(false);
let bound = false;

function normalizePlugin(plugin: PluginInfo): PluginInfo {
  return {
    ...plugin,
    settings: Array.isArray(plugin.settings) ? plugin.settings : [],
    config: plugin.config && typeof plugin.config === 'object' ? plugin.config : {},
    description: plugin.description ?? '',
    author: plugin.author ?? '',
    runtime: plugin.runtime === 'native' ? 'native' : 'js',
    http: plugin.http ?? {
      enabled: false,
      running: false,
      port: 0,
      local_ip: null,
      local_ips: [],
      urls: [],
      last_error: null,
    },
  };
}

async function refresh() {
  try {
    const [list, dir] = await Promise.all([
      invoke<PluginInfo[]>('plugins_list'),
      invoke<string>('plugins_dir'),
    ]);
    plugins = list.map(normalizePlugin);
    pluginsDir = dir;
  } catch (e) {
    console.error('Failed to list plugins:', e);
  } finally {
    ready = true;
  }
}

function bindEvents() {
  if (bound || typeof window === 'undefined') return;
  bound = true;
  void listen<PluginInfo[]>('plugins:updated', (event) => {
    if (Array.isArray(event.payload)) {
      plugins = event.payload.map(normalizePlugin);
    }
  }).catch(() => {
    bound = false;
  });
}

export function createPluginsStore() {
  bindEvents();
  if (!ready) void refresh();
  return getPluginsStore();
}

export function getPluginsStore() {
  return {
    get list() {
      return plugins;
    },
    get dir() {
      return pluginsDir;
    },
    get ready() {
      return ready;
    },
    async refresh() {
      await refresh();
    },
    async setEnabled(id: string, enabled: boolean) {
      const info = await invoke<PluginInfo>('plugins_set_enabled', { id, enabled });
      plugins = plugins.map((p) => (p.id === id ? normalizePlugin(info) : p));
      return info;
    },
    async setConfig(id: string, patch: Record<string, unknown>) {
      const config = await invoke<Record<string, unknown>>('plugin_settings_set', {
        id,
        data: patch,
      });
      plugins = plugins.map((p) => (p.id === id ? { ...p, config: config ?? p.config } : p));
      return config;
    },
  };
}
