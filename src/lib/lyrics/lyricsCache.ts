import { fetchLyrics, type FetchLyricsParams } from './fetchLyrics';
import type { LyricsResult } from './types';

/** In-memory lyrics by track path — instant apply on gapless / next so layout does not jump. */
const byPath = new Map<string, LyricsResult | null>();
const inflight = new Map<string, Promise<LyricsResult | null>>();

export function peekLyricsCache(path: string): LyricsResult | null | undefined {
  if (!path) return undefined;
  return byPath.has(path) ? byPath.get(path) : undefined;
}

export function setLyricsCache(path: string, result: LyricsResult | null) {
  if (!path) return;
  byPath.set(path, result);
}

export function invalidateLyricsCache(path?: string) {
  if (!path) {
    byPath.clear();
    inflight.clear();
    return;
  }
  byPath.delete(path);
  inflight.delete(path);
}

/**
 * Fetch lyrics for a track path (deduped). Always stores a settled entry (including null).
 */
export function loadLyricsForPath(
  path: string,
  params: FetchLyricsParams,
): Promise<LyricsResult | null> {
  if (!path) return Promise.resolve(null);

  if (byPath.has(path)) {
    return Promise.resolve(byPath.get(path) ?? null);
  }

  const existing = inflight.get(path);
  if (existing) return existing;

  const promise = fetchLyrics(params)
    .then((result) => {
      byPath.set(path, result);
      return result;
    })
    .catch((error: unknown) => {
      console.warn('[lyrics] fetch failed', error);
      byPath.set(path, null);
      return null;
    })
    .finally(() => {
      inflight.delete(path);
    });

  inflight.set(path, promise);
  return promise;
}

/** Warm next tracks without blocking the UI. */
export function prefetchLyricsForPath(path: string, params: FetchLyricsParams) {
  if (!path || byPath.has(path) || inflight.has(path)) return;
  void loadLyricsForPath(path, params);
}
