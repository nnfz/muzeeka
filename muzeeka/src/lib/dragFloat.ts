import { invoke } from '@tauri-apps/api/core';

export interface DragFloatContent {
  title: string;
  artist: string;
  coverPath?: string | null;
  count: number;
  isCopy?: boolean;
  rotate?: number;
}

function toPayload(content: DragFloatContent) {
  return {
    title: content.title,
    artist: content.artist,
    coverPath: content.coverPath ?? null,
    count: content.count,
    isCopy: !!content.isCopy,
    rotate: content.rotate ?? -2.5,
  };
}

/** Show outside-window float and start OS cursor following. */
export async function dragFloatShow(content: DragFloatContent): Promise<void> {
  try {
    await invoke('drag_float_show', { payload: toPayload(content) });
  } catch (e) {
    console.error('drag_float_show failed:', e);
  }
}

/** Update card while visible (title/cover/tilt). */
export async function dragFloatUpdate(content: DragFloatContent): Promise<void> {
  try {
    await invoke('drag_float_update', { payload: toPayload(content) });
  } catch {
    /* ignore if not open */
  }
}

/** Hide float and stop cursor follow. */
export async function dragFloatHide(): Promise<void> {
  try {
    await invoke('drag_float_hide');
  } catch {
    /* ignore */
  }
}
