/**
 * Persisted editor state for one playlist edge (prev → next).
 *
 * Kept in its own leaf module: both the player store and the Mix Transition window
 * read it, and the store must not pull in the window-opening code (Tauri window
 * helpers, cover/label lookups) just to resolve a saved layout.
 */

import { parseBlocks, serializeBlocks, type MixBlock } from '$lib/mix/blocks';

const MEMORY_KEY_PREFIX = 'muzeeka:mix-edge:';

export interface MixTransitionMemory {
  fromViewStart: number;
  toViewStart: number;
  pxPerSec: number;
  /** Cut / playhead on previous track (track-relative seconds). */
  playheadFromSecs: number;
  fromGridOffset: number;
  toGridOffset: number;
  /** Transition + effect blocks on the timeline. */
  blocks: MixBlock[];
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
      blocks: parseBlocks(
        (parsed as MixTransitionMemory & { blocks?: unknown }).blocks,
      ),
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
      blocks: serializeBlocks(memory.blocks ?? []),
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
