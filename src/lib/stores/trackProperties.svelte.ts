import { emit } from '@tauri-apps/api/event';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import type { MusicFile } from '$lib/stores/player.svelte';

export const TRACK_PROPERTIES_LABEL = 'track-properties';

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
}

async function showWindow(win: WebviewWindow) {
  try {
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

/** Open the track properties window with the given track snapshot. */
export async function openTrackPropertiesWindow(track: MusicFile) {
  const payload: TrackPropertiesOpenPayload = { track };
  try {
    let win = await WebviewWindow.getByLabel(TRACK_PROPERTIES_LABEL);

    if (win) {
      await emit('track-properties:open', payload);
      await showWindow(win);
      return;
    }

    win = new WebviewWindow(TRACK_PROPERTIES_LABEL, WINDOW_OPTIONS);

    win.once('tauri://error', (e: { payload?: string }) => {
      console.error('[track-properties] creation error:', e?.payload || e);
    });

    const openWithPayload = async () => {
      await emit('track-properties:open', payload);
      await showWindow(win!);
    };

    win.once('tauri://created', openWithPayload);
    setTimeout(openWithPayload, 120);
  } catch (err) {
    console.error('[track-properties] Failed to open window:', err);
  }
}

/** Pre-create the properties window hidden (main window only). */
export function precreateTrackPropertiesWindow() {
  queueMicrotask(async () => {
    try {
      const existing = await WebviewWindow.getByLabel(TRACK_PROPERTIES_LABEL);
      if (existing) return;
      new WebviewWindow(TRACK_PROPERTIES_LABEL, WINDOW_OPTIONS);
    } catch {
      // non-fatal
    }
  });
}

/** Tell the properties window to close if it is showing any of these paths. */
export function notifyTrackPropertiesPathsRemoved(paths: string[]) {
  if (paths.length === 0) return;
  void emit('track-properties:tracks-removed', {
    paths,
  } satisfies TrackPropertiesRemovedPayload);
}

/** Force-close properties (e.g. library cleared). */
export function notifyTrackPropertiesCloseAll() {
  void emit('track-properties:close', {});
}
