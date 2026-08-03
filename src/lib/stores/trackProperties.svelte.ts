import { emit, emitTo } from '@tauri-apps/api/event';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import {
  trackDisplayArtist,
  trackDisplayTitle,
  type MusicFile,
} from '$lib/stores/player.svelte';

/** Prefix for all track-properties windows (`track-properties-<hash>`). */
export const TRACK_PROPERTIES_LABEL_PREFIX = 'track-properties-';

/** @deprecated Use {@link TRACK_PROPERTIES_LABEL_PREFIX} + per-track hash. */
export const TRACK_PROPERTIES_LABEL = 'track-properties';

const PENDING_KEY_PREFIX = 'muzeeka:track-properties:pending:';

export interface TrackPropertiesRemovedPayload {
  paths: string[];
}

const WINDOW_OPTIONS = {
  url: import.meta.env.DEV ? 'http://localhost:1420/' : 'index.html',
  title: 'Track properties',
  width: 900,
  height: 700,
  minWidth: 720,
  minHeight: 560,
  decorations: false,
  resizable: true,
  visible: false,
  theme: 'dark' as const,
};

export interface TrackPropertiesOpenPayload {
  track: MusicFile;
  /** Target window label — each properties webview ignores foreign labels. */
  windowLabel: string;
}

/** Stable window label for a track path (Tauri labels are restricted charset). */
export function trackPropertiesLabelForPath(path: string): string {
  return `${TRACK_PROPERTIES_LABEL_PREFIX}${hashPath(path)}`;
}

export function isTrackPropertiesWindowLabel(label: string | null | undefined): boolean {
  if (!label) return false;
  return (
    label === TRACK_PROPERTIES_LABEL ||
    label.startsWith(TRACK_PROPERTIES_LABEL_PREFIX)
  );
}

function hashPath(path: string): string {
  // Match player path normalization so the same track always gets the same window.
  let p = path.trim();
  if (p.startsWith('\\\\?\\')) p = p.slice(4);
  else if (p.startsWith('//?/')) p = p.slice(4);
  p = p.replace(/\//g, '\\').toLowerCase();

  // FNV-1a 32-bit — short, stable, alphanumeric hex label suffix.
  let h = 0x811c9dc5;
  for (let i = 0; i < p.length; i++) {
    h ^= p.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16).padStart(8, '0');
}

function pendingKey(windowLabel: string): string {
  return `${PENDING_KEY_PREFIX}${windowLabel}`;
}

/** Stash track for a specific properties window (survives boot race). */
export function stashPendingTrackProperties(windowLabel: string, track: MusicFile) {
  try {
    localStorage.setItem(
      pendingKey(windowLabel),
      JSON.stringify({ track, windowLabel } satisfies TrackPropertiesOpenPayload),
    );
  } catch {
    /* quota / private mode */
  }
}

/** Read + clear pending payload for this window label. */
export function takePendingTrackProperties(
  windowLabel: string,
): TrackPropertiesOpenPayload | null {
  try {
    const raw = localStorage.getItem(pendingKey(windowLabel));
    if (!raw) return null;
    localStorage.removeItem(pendingKey(windowLabel));
    const parsed = JSON.parse(raw) as TrackPropertiesOpenPayload;
    if (!parsed?.track || typeof parsed.track.path !== 'string' || !parsed.track.path) {
      return null;
    }
    return {
      track: parsed.track,
      windowLabel: parsed.windowLabel || windowLabel,
    };
  } catch {
    try {
      localStorage.removeItem(pendingKey(windowLabel));
    } catch {
      /* ignore */
    }
    return null;
  }
}

function clearPendingTrackProperties(windowLabel: string) {
  try {
    localStorage.removeItem(pendingKey(windowLabel));
  } catch {
    /* ignore */
  }
}

function windowTitleFor(track: MusicFile): string {
  const title = trackDisplayTitle(track);
  const artist = trackDisplayArtist(track);
  return `${title} — ${artist}`;
}

/** Cascade offset so multiple properties windows are not stacked perfectly. */
function cascadeOffset(windowLabel: string): { x: number; y: number } {
  const hex = windowLabel.slice(TRACK_PROPERTIES_LABEL_PREFIX.length) || '0';
  const n = Number.parseInt(hex.slice(-2), 16);
  const step = Number.isFinite(n) ? n % 10 : 0;
  return { x: 48 + step * 28, y: 48 + step * 28 };
}

async function showWindow(win: WebviewWindow, track: MusicFile) {
  try {
    await win.setTitle(windowTitleFor(track));
    await win.setSize(new LogicalSize(WINDOW_OPTIONS.width, WINDOW_OPTIONS.height));
    await win.show();
    await win.setFocus();
  } catch {
    setTimeout(async () => {
      try {
        await win.show();
        await win.setFocus();
      } catch {
        /* ignore */
      }
    }, 60);
  }
}

async function deliverOpen(windowLabel: string, track: MusicFile) {
  const payload: TrackPropertiesOpenPayload = { track, windowLabel };
  stashPendingTrackProperties(windowLabel, track);
  try {
    await emitTo(windowLabel, 'track-properties:open', payload);
  } catch {
    await emit('track-properties:open', payload);
  }
}

/**
 * Open a dedicated properties window for this track.
 * Same track again → focus the existing window (no second copy).
 * Different tracks → separate windows.
 */
export async function openTrackPropertiesWindow(track: MusicFile) {
  const windowLabel = trackPropertiesLabelForPath(track.path);
  try {
    let win = await WebviewWindow.getByLabel(windowLabel);

    if (win) {
      await deliverOpen(windowLabel, track);
      await showWindow(win, track);
      return;
    }

    const { x, y } = cascadeOffset(windowLabel);
    win = new WebviewWindow(windowLabel, {
      ...WINDOW_OPTIONS,
      title: windowTitleFor(track),
      x,
      y,
    });

    // Stash before the webview boots so onMount can pick it up without a race.
    stashPendingTrackProperties(windowLabel, track);

    win.once('tauri://error', (e: { payload?: string }) => {
      console.error('[track-properties] creation error:', e?.payload || e);
      clearPendingTrackProperties(windowLabel);
    });

    let delivered = false;
    const openWithPayload = async () => {
      if (delivered) return;
      delivered = true;
      await deliverOpen(windowLabel, track);
      await showWindow(win!, track);
    };

    win.once('tauri://created', () => {
      void openWithPayload();
    });
    // Native create ≠ JS listener ready; delayed show covers cold boot.
    setTimeout(() => {
      void openWithPayload();
    }, 280);
  } catch (err) {
    console.error('[track-properties] Failed to open window:', err);
  }
}

/** No-op: properties windows are created on demand per track. */
export function precreateTrackPropertiesWindow() {
  // Intentionally empty — multi-window model does not share a hidden shell.
}

/** Tell properties windows showing any of these paths to close. */
export function notifyTrackPropertiesPathsRemoved(paths: string[]) {
  if (paths.length === 0) return;
  void emit('track-properties:tracks-removed', {
    paths,
  } satisfies TrackPropertiesRemovedPayload);
}

/** Force-close every properties window (e.g. library cleared). */
export function notifyTrackPropertiesCloseAll() {
  void emit('track-properties:close-all', {});
}
