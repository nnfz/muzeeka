import { invoke } from '@tauri-apps/api/core';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { emit } from '@tauri-apps/api/event';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { readDownloadSettings } from '$lib/stores/settings.svelte';
import { normalizeMediaUrl } from '$lib/urlUtils';
import type { MusicFile } from '$lib/stores/player.svelte';

export interface YtdlpProbeResult {
  title: string;
  uploader: string | null;
  duration_secs: number | null;
  thumbnail: string | null;
  is_playlist: boolean;
  entry_count: number | null;
}

export interface YtdlpProgress {
  status: string;
  percent: number | null;
  url: string;
}

export const DOWNLOAD_WINDOW_LABEL = 'download';

const APP_URL = () => (import.meta.env.DEV ? 'http://localhost:1420/' : 'index.html');

const DOWNLOAD_WINDOW_SIZE = new LogicalSize(400, 280);

const DOWNLOAD_WINDOW_OPTIONS = {
  url: APP_URL(),
  title: 'Download',
  width: 400,
  height: 280,
  minWidth: 400,
  minHeight: 280,
  maxWidth: 400,
  maxHeight: 280,
  decorations: false,
  resizable: false,
  maximizable: false,
  minimizable: false,
  visible: false,
  theme: 'dark' as const,
};

let url = $state('');
let probe = $state<YtdlpProbeResult | null>(null);
let progress = $state<YtdlpProgress | null>(null);
let downloadPercent = $state<number | null>(null);
let error = $state<string | null>(null);
let isProbing = $state(false);
let isDownloading = $state(false);
let ytdlpReady = $state<boolean | null>(null);
let ffmpegReady = $state<boolean | null>(null);

let unlistenProgress: UnlistenFn | null = null;

async function ensureProgressListener() {
  if (unlistenProgress) return;
  unlistenProgress = await listen<YtdlpProgress>('ytdlp:progress', (event) => {
    progress = event.payload;
    if (event.payload.percent != null) {
      downloadPercent = Math.max(0, Math.min(100, Math.round(event.payload.percent)));
    }
  });
}

async function checkAvailability() {
  try {
    ytdlpReady = await invoke<boolean>('ytdlp_available');
  } catch {
    ytdlpReady = false;
  }
  try {
    ffmpegReady = await invoke<boolean>('ytdlp_ffmpeg_available');
  } catch {
    ffmpegReady = false;
  }
}

function resetState() {
  url = '';
  probe = null;
  progress = null;
  downloadPercent = null;
  error = null;
}

async function lockDownloadWindowSize(win: WebviewWindow) {
  await win.setResizable(false);
  await win.setMaximizable(false);
  if (await win.isMaximized()) {
    await win.unmaximize();
  }
  await win.setSize(DOWNLOAD_WINDOW_SIZE);
}

async function showDownloadWindow(win: WebviewWindow) {
  try {
    await lockDownloadWindowSize(win);
    await win.show();
    await win.setFocus();
  } catch {
    setTimeout(async () => {
      try {
        await lockDownloadWindowSize(win);
        await win.show();
        await win.setFocus();
      } catch { /* ignore */ }
    }, 60);
  }
}

/** Pre-create the download window hidden (main window only). */
export function precreateDownloadWindow() {
  queueMicrotask(async () => {
    try {
      const existing = await WebviewWindow.getByLabel(DOWNLOAD_WINDOW_LABEL);
      if (existing) return;

      new WebviewWindow(DOWNLOAD_WINDOW_LABEL, DOWNLOAD_WINDOW_OPTIONS);
    } catch {
      // non-fatal
    }
  });
}

/** Open the download window (same pattern as settings). */
export async function openDownloadWindow(initialUrl = '') {
  try {
    let win = await WebviewWindow.getByLabel(DOWNLOAD_WINDOW_LABEL);

    if (win) {
      await emit('download:open', { url: initialUrl });
      await showDownloadWindow(win);
      return;
    }

    win = new WebviewWindow(DOWNLOAD_WINDOW_LABEL, DOWNLOAD_WINDOW_OPTIONS);

    win.once('tauri://error', (e: any) => {
      console.error('[download] creation error:', e?.message || e);
    });

    const openWithPayload = async () => {
      await emit('download:open', { url: initialUrl });
      await showDownloadWindow(win!);
    };

    win.once('tauri://created', openWithPayload);
    setTimeout(openWithPayload, 120);
  } catch (err) {
    console.error('[download] Failed to open download window:', err);
  }
}

export function createDownloadStore() {
  void checkAvailability();
  void ensureProgressListener();

  return {
    get url() { return url; },
    get probe() { return probe; },
    get progress() { return progress; },
    get downloadPercent() { return downloadPercent; },
    get error() { return error; },
    get isProbing() { return isProbing; },
    get isDownloading() { return isDownloading; },
    get ytdlpReady() { return ytdlpReady; },
    get ffmpegReady() { return ffmpegReady; },

    resetForOpen(initialUrl = '') {
      resetState();
      url = initialUrl;
      void checkAvailability();
      if (initialUrl.trim()) {
        void this.probeUrl();
      }
    },

    async closeWindow() {
      if (isDownloading) return;
      resetState();
      try {
        await getCurrentWindow().hide();
      } catch {
        await getCurrentWindow().close();
      }
    },

    setUrl(value: string) {
      url = value;
      probe = null;
      error = null;
    },

    clearProbeState() {
      if (isDownloading) return;
      probe = null;
      error = null;
      progress = null;
      downloadPercent = null;
    },

    async probeUrl(targetUrl?: string) {
      const normalized = normalizeMediaUrl(targetUrl ?? url);
      if (!normalized) {
        error = 'Enter a valid media URL';
        probe = null;
        return;
      }

      url = normalized;
      isProbing = true;
      error = null;
      probe = null;

      try {
        probe = await invoke<YtdlpProbeResult>('ytdlp_probe', { url: normalized });
      } catch (e) {
        error = typeof e === 'string' ? e : String(e);
      } finally {
        isProbing = false;
      }
    },

    async download(targetUrl?: string) {
      const normalized = normalizeMediaUrl(targetUrl ?? url);
      if (!normalized) {
        error = 'Enter a valid media URL';
        return 0;
      }

      isDownloading = true;
      error = null;
      downloadPercent = 0;
      progress = { status: 'Starting…', percent: 0, url: normalized };

      try {
        const { downloadFolder, downloadPlaylistId } = readDownloadSettings();
        const result = await invoke<{ files: MusicFile[] }>('ytdlp_download', {
          url: normalized,
          outputDir: downloadFolder ?? null,
          allowPlaylist: probe?.is_playlist ?? false,
        });

        await emit('ytdlp:downloaded', {
          files: result.files,
          playlistId: downloadPlaylistId ?? null,
        });

        progress = null;
        downloadPercent = null;
        return result.files.length;
      } catch (e) {
        error = typeof e === 'string' ? e : String(e);
        return 0;
      } finally {
        isDownloading = false;
      }
    },

    async cancel() {
      try {
        await invoke('ytdlp_cancel');
      } catch { /* ignore */ }
      isDownloading = false;
      progress = null;
      downloadPercent = null;
      error = 'Download cancelled';
    },
  };
}

let _instance: ReturnType<typeof createDownloadStore> | null = null;

export function getDownloadStore() {
  if (!_instance) {
    _instance = createDownloadStore();
  }
  return _instance;
}