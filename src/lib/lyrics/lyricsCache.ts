import { fetchLyrics, type FetchLyricsParams } from './fetchLyrics';
import type { LyricsResult } from './types';

/** Cap parsed lyrics payloads — each can hold hundreds of timed lines. */
const MAX_LYRICS_CACHE = 24;

/** In-memory lyrics by track path — instant apply on gapless / next so layout does not jump. */
const byPath = new Map<string, LyricsResult | null>();
const inflight = new Map<string, Promise<LyricsResult | null>>();

function lruSet(path: string, result: LyricsResult | null) {
  if (byPath.has(path)) byPath.delete(path);
  byPath.set(path, result);
  while (byPath.size > MAX_LYRICS_CACHE) {
    const oldest = byPath.keys().next().value;
    if (oldest === undefined) break;
    byPath.delete(oldest);
  }
}

export function peekLyricsCache(path: string): LyricsResult | null | undefined {
  if (!path) return undefined;
  if (!byPath.has(path)) return undefined;
  const value = byPath.get(path);
  // Touch for LRU.
  byPath.delete(path);
  byPath.set(path, value ?? null);
  return value;
}

export function setLyricsCache(path: string, result: LyricsResult | null) {
  if (!path) return;
  lruSet(path, result);
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

  const peeked = peekLyricsCache(path);
  if (peeked !== undefined) {
    return Promise.resolve(peeked);
  }

  const existing = inflight.get(path);
  if (existing) return existing;

  const promise = fetchLyrics(params)
    .then((result) => {
      lruSet(path, result);
      return result;
    })
    .catch((error: unknown) => {
      console.warn('[lyrics] fetch failed', error);
      lruSet(path, null);
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
