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

/** Heuristic: URL is a playlist/album/set (not a single track). */
function looksLikePlaylistOrAlbumUrl(url: string): boolean {
  const u = url.toLowerCase();
  return (
    u.includes('/playlist') ||
    u.includes('/album') ||
    u.includes('/sets/') ||
    u.includes('/music/playlist') ||
    u.includes('/music/album') ||
    u.includes('list=') ||
    u.includes('/artist/') ||
    (u.includes('soundcloud.com') && u.includes('/sets/'))
  );
}

/** Build a readable playlist name from a media URL path. */
function playlistNameFromUrl(url: string): string {
  try {
    const parsed = new URL(url);
    const parts = parsed.pathname.split('/').filter(Boolean);
    // Prefer last meaningful segment (skip ids-only when possible)
    for (let i = parts.length - 1; i >= 0; i--) {
      const seg = decodeURIComponent(parts[i]).replace(/[-_]+/g, ' ').trim();
      if (!seg || /^(track|tracks|playlist|playlists|album|albums|sets|artist)$/i.test(seg)) {
        continue;
      }
      if (/^[a-zA-Z0-9]{10,}$/.test(seg) && !seg.includes(' ')) {
        // opaque id — keep looking for a name segment
        continue;
      }
      return seg.length > 80 ? seg.slice(0, 80) : seg;
    }
    return parts[parts.length - 1]
      ? decodeURIComponent(parts[parts.length - 1]).slice(0, 80)
      : 'Downloaded playlist';
  } catch {
    return 'Downloaded playlist';
  }
}

/** Safe folder name for Windows/macOS/Linux. */
function sanitizeFolderName(name: string): string {
  let s = name
    .replace(/[<>:"/\\|?*\u0000-\u001f]/g, '_')
    .replace(/\s+/g, ' ')
    .replace(/[. ]+$/g, '')
    .trim();
  if (!s) s = 'Playlist';
  if (s.length > 100) s = s.slice(0, 100).trim();
  return s;
}

function joinPath(base: string, segment: string): string {
  const sep = base.includes('\\') ? '\\' : '/';
  return `${base.replace(/[\\/]+$/, '')}${sep}${segment}`;
}

/**
 * Resolve where files should land:
 * - single track → download folder root
 * - playlist/album → download folder / <playlist name>/
 */
async function resolveDownloadOutputDir(namedFolder: string | null): Promise<string> {
  const { downloadFolder } = readDownloadSettings();
  let base = downloadFolder?.trim() || '';
  if (!base) {
    base = await invoke<string>('ytdlp_default_download_dir');
  }
  if (!namedFolder) return base;
  return joinPath(base, sanitizeFolderName(namedFolder));
}

function resolveNamedDownload(normalizedUrl: string): string | null {
  if (probe?.is_playlist && probe.title.trim()) {
    return probe.title.trim();
  }
  if (looksLikePlaylistOrAlbumUrl(normalizedUrl)) {
    return playlistNameFromUrl(normalizedUrl);
  }
  return null;
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
      const next = Math.max(0, Math.min(100, Math.round(event.payload.percent)));
      // Monotonic while downloading so the bar never jumps backwards.
      downloadPercent =
        downloadPercent == null ? next : Math.max(downloadPercent, next);
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
        const { downloadPlaylistId } = readDownloadSettings();
        // Same name for disk folder + library playlist (albums/playlists/sets).
        const named =
          resolveNamedDownload(normalized) ??
          null;

        const outputDir = await resolveDownloadOutputDir(named);

        const result = await invoke<{ files: MusicFile[] }>('ytdlp_download', {
          url: normalized,
          outputDir,
          allowPlaylist: probe?.is_playlist ?? looksLikePlaylistOrAlbumUrl(normalized),
        });

        // Multi-file fallback: if we didn't know the name before download, still
        // create a library playlist when several tracks arrived from a set URL.
        let namedPlaylist = named;
        if (!namedPlaylist && result.files.length > 1 && looksLikePlaylistOrAlbumUrl(normalized)) {
          namedPlaylist = playlistNameFromUrl(normalized);
        }

        await emit('ytdlp:downloaded', {
          files: result.files,
          playlistId: downloadPlaylistId ?? null,
          namedPlaylist,
          // Probe thumbnail → playlist cover (VK / Spotify / SoundCloud / YouTube)
          coverUrl:
            namedPlaylist && probe?.thumbnail?.trim()
              ? probe.thumbnail.trim()
              : null,
        });

        downloadPercent = 100;
        progress = { status: 'Done', percent: 100, url: normalized };
        return result.files.length;
      } catch (e) {
        error = typeof e === 'string' ? e : String(e);
        progress = null;
        downloadPercent = null;
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