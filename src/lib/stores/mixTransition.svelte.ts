import { emit, emitTo } from '@tauri-apps/api/event';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import {
  trackDisplayArtist,
  trackDisplayTitle,
  type MusicFile,
} from '$lib/stores/player.svelte';

export const MIX_TRANSITION_LABEL = 'mix-transition';

const PENDING_KEY = 'muzeeka:mix-transition:pending';
const MEMORY_KEY_PREFIX = 'muzeeka:mix-edge:';

/** Saved editor state for one playlist edge (prev → next). */
export interface MixTransitionMemory {
  fromViewStart: number;
  toViewStart: number;
  pxPerSec: number;
  /** Cut / playhead on previous track (track-relative seconds). */
  playheadFromSecs: number;
  fromGridOffset: number;
  toGridOffset: number;
  savedAt: number;
}

function pathKey(path: string): string {
  let p = path.trim();
  if (p.startsWith('\\\\?\\')) p = p.slice(4);
  else if (p.startsWith('//?/')) p = p.slice(4);
  return p.replace(/\//g, '\\').toLowerCase();
}

export function mixTransitionMemoryKey(
  playlistId: string,
  fromPath: string,
  toPath: string,
): string {
  return `${MEMORY_KEY_PREFIX}${playlistId}::${pathKey(fromPath)}::${pathKey(toPath)}`;
}

export function loadMixTransitionMemory(
  playlistId: string,
  fromPath: string,
  toPath: string,
): MixTransitionMemory | null {
  try {
    const raw = localStorage.getItem(
      mixTransitionMemoryKey(playlistId, fromPath, toPath),
    );
    if (!raw) return null;
    const parsed = JSON.parse(raw) as MixTransitionMemory;
    if (
      typeof parsed?.fromViewStart !== 'number' ||
      typeof parsed?.toViewStart !== 'number' ||
      typeof parsed?.pxPerSec !== 'number' ||
      typeof parsed?.playheadFromSecs !== 'number'
    ) {
      return null;
    }
    return {
      fromViewStart: parsed.fromViewStart,
      toViewStart: parsed.toViewStart,
      pxPerSec: parsed.pxPerSec,
      playheadFromSecs: parsed.playheadFromSecs,
      fromGridOffset:
        typeof parsed.fromGridOffset === 'number' ? parsed.fromGridOffset : 0,
      toGridOffset:
        typeof parsed.toGridOffset === 'number' ? parsed.toGridOffset : 0,
      savedAt: typeof parsed.savedAt === 'number' ? parsed.savedAt : 0,
    };
  } catch {
    return null;
  }
}

export function saveMixTransitionMemory(
  playlistId: string,
  fromPath: string,
  toPath: string,
  memory: Omit<MixTransitionMemory, 'savedAt'>,
): void {
  try {
    const payload: MixTransitionMemory = {
      ...memory,
      savedAt: Date.now(),
    };
    localStorage.setItem(
      mixTransitionMemoryKey(playlistId, fromPath, toPath),
      JSON.stringify(payload),
    );
  } catch {
    /* quota / private mode */
  }
}

const WINDOW_OPTIONS = {
  url: import.meta.env.DEV ? 'http://localhost:1420/' : 'index.html',
  title: 'Mix transition',
  width: 1040,
  height: 560,
  minWidth: 760,
  minHeight: 420,
  decorations: false,
  resizable: true,
  visible: false,
  theme: 'dark' as const,
};

export interface MixTransitionOpenPayload {
  from: MusicFile;
  to: MusicFile;
  playlistId: string;
  fromIndex: number;
}

export function isMixTransitionWindowLabel(label: string | null | undefined): boolean {
  return label === MIX_TRANSITION_LABEL;
}

function windowTitleFor(from: MusicFile, to: MusicFile): string {
  return `${trackDisplayTitle(from)} → ${trackDisplayTitle(to)}`;
}

export function stashPendingMixTransition(payload: MixTransitionOpenPayload) {
  try {
    localStorage.setItem(PENDING_KEY, JSON.stringify(payload));
  } catch {
    /* quota / private mode */
  }
}

export function takePendingMixTransition(): MixTransitionOpenPayload | null {
  try {
    const raw = localStorage.getItem(PENDING_KEY);
    if (!raw) return null;
    localStorage.removeItem(PENDING_KEY);
    const parsed = JSON.parse(raw) as MixTransitionOpenPayload;
    if (
      !parsed?.from?.path ||
      !parsed?.to?.path ||
      typeof parsed.from.path !== 'string' ||
      typeof parsed.to.path !== 'string'
    ) {
      return null;
    }
    return parsed;
  } catch {
    try {
      localStorage.removeItem(PENDING_KEY);
    } catch {
      /* ignore */
    }
    return null;
  }
}

function clearPending() {
  try {
    localStorage.removeItem(PENDING_KEY);
  } catch {
    /* ignore */
  }
}

async function showWindow(win: WebviewWindow, from: MusicFile, to: MusicFile) {
  try {
    await win.setTitle(windowTitleFor(from, to));
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

async function deliverOpen(payload: MixTransitionOpenPayload) {
  stashPendingMixTransition(payload);
  try {
    await emitTo(MIX_TRANSITION_LABEL, 'mix-transition:open', payload);
  } catch {
    await emit('mix-transition:open', payload);
  }
}

/** Open (or focus + reload) the Mix transition editor for a track pair. */
export async function openMixTransitionWindow(
  from: MusicFile,
  to: MusicFile,
  playlistId: string,
  fromIndex: number,
) {
  const payload: MixTransitionOpenPayload = {
    from,
    to,
    playlistId,
    fromIndex,
  };

  try {
    let win = await WebviewWindow.getByLabel(MIX_TRANSITION_LABEL);

    if (win) {
      await deliverOpen(payload);
      await showWindow(win, from, to);
      return;
    }

    win = new WebviewWindow(MIX_TRANSITION_LABEL, {
      ...WINDOW_OPTIONS,
      title: windowTitleFor(from, to),
    });

    stashPendingMixTransition(payload);

    win.once('tauri://error', (e: { payload?: string }) => {
      console.error('[mix-transition] creation error:', e?.payload || e);
      clearPending();
    });

    let delivered = false;
    const openWithPayload = async () => {
      if (delivered) return;
      delivered = true;
      await deliverOpen(payload);
      await showWindow(win!, from, to);
    };

    win.once('tauri://created', () => {
      void openWithPayload();
    });
    setTimeout(() => {
      void openWithPayload();
    }, 280);
  } catch (err) {
    console.error('[mix-transition] Failed to open window:', err);
  }
}

export function trackLabel(track: MusicFile): string {
  const title = trackDisplayTitle(track);
  const artist = trackDisplayArtist(track);
  return artist ? `${title} — ${artist}` : title;
}
