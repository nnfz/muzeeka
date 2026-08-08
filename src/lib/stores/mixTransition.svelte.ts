import { emit, emitTo } from '@tauri-apps/api/event';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import {
  trackDisplayArtist,
  trackDisplayTitle,
  type MusicFile,
} from '$lib/stores/player.svelte';

// Per-edge saved layout lives in `$lib/mix/memory` (leaf module, no player import);
// re-exported here so the editor keeps a single import site.
export {
  loadMixTransitionMemory,
  mixTransitionMemoryKey,
  saveMixTransitionMemory,
  type MixTransitionMemory,
} from '$lib/mix/memory';

export const MIX_TRANSITION_LABEL = 'mix-transition';

const PENDING_KEY = 'muzeeka:mix-transition:pending';

const WINDOW_OPTIONS = {
  url: import.meta.env.DEV ? 'http://localhost:1420/' : 'index.html',
  title: 'Mix transition',
  width: 1040,
  height: 640,
  minWidth: 760,
  minHeight: 500,
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
