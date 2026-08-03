import { convertFileSrc, invoke } from '@tauri-apps/api/core';

/** Asset-protocol URL strings are tiny — keep a modest LRU. */
const MAX_SRC_CACHE = 256;
/**
 * Base64 data URLs can be hundreds of KB each. Cap hard and never warm-decode them
 * in bulk (browser image cache holds decoded bitmaps).
 */
const MAX_DATA_URL_CACHE = 4;
const MAX_DATA_URL_CHARS = 180_000;
/** Lazy cover path resolve results (path → cover file). */
const MAX_TRACK_COVER_PATHS = 400;
/** Paths we already tried to warm-decode (thumbs only). */
const MAX_WARMED = 48;
const MAX_TRACK_COVER_JOBS = 2;
/** Default bulk prefetch budget — thumbs only, no decode by default. */
const DEFAULT_PREFETCH_LIMIT = 32;

const srcCache = new Map<string, string>();
const dataUrlCache = new Map<string, string>();
const warmed = new Set<string>();
const inflight = new Map<string, Promise<string | null>>();
const trackCoverPaths = new Map<string, string | null>();
const trackCoverInflight = new Map<string, Promise<string | null>>();
const trackCoverWaiters: Array<() => void> = [];
let activeTrackCoverJobs = 0;

function trackPathKey(path: string): string {
  return path.trim().replace(/\//g, '\\').toLowerCase();
}

function coverSourcePath(path: string): string {
  return path.replace(/#cue:\d+$/i, '');
}

function isCoverCachePath(path: string): boolean {
  return /[\\/]covers[\\/]/i.test(path) || /[\\/]playlist_covers[\\/]/i.test(path);
}

function isFullCoverPath(path: string): boolean {
  return /-full\./i.test(path);
}

function lruTouchGet<V>(map: Map<string, V>, key: string): V | undefined {
  if (!map.has(key)) return undefined;
  const value = map.get(key)!;
  map.delete(key);
  map.set(key, value);
  return value;
}

function lruSet<V>(map: Map<string, V>, key: string, value: V, max: number) {
  if (map.has(key)) map.delete(key);
  map.set(key, value);
  while (map.size > max) {
    const oldest = map.keys().next().value;
    if (oldest === undefined) break;
    map.delete(oldest);
  }
}

function warmedAdd(path: string) {
  if (warmed.has(path)) {
    warmed.delete(path);
    warmed.add(path);
    return;
  }
  warmed.add(path);
  while (warmed.size > MAX_WARMED) {
    const oldest = warmed.values().next().value;
    if (oldest === undefined) break;
    warmed.delete(oldest);
  }
}

function acquireTrackCoverSlot(): Promise<void> {
  if (activeTrackCoverJobs < MAX_TRACK_COVER_JOBS) {
    activeTrackCoverJobs += 1;
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    trackCoverWaiters.push(() => {
      activeTrackCoverJobs += 1;
      resolve();
    });
  });
}

function releaseTrackCoverSlot() {
  activeTrackCoverJobs = Math.max(0, activeTrackCoverJobs - 1);
  trackCoverWaiters.shift()?.();
}

/**
 * Prefer full-res cover for fullscreen / large UI.
 * Content-addressed cache stores pairs: c-…-thumb.webp + c-…-full.jpg|webp.
 * Never treat a *-thumb.* path as full (that upscales 96px in fullscreen).
 */
export function preferFullCoverPath(
  thumb: string | null | undefined,
  full: string | null | undefined,
): string | null {
  const fullPath = full?.trim();
  if (fullPath && !/-thumb\./i.test(fullPath)) return fullPath;

  const thumbPath = thumb?.trim();
  if (!thumbPath) return null;

  // Map list thumb → full siblings. Prefer lossy JPEG (new), then legacy WebP.
  if (/-thumb\.webp$/i.test(thumbPath)) {
    return thumbPath.replace(/-thumb\.webp$/i, '-full.jpg');
  }
  if (/-thumb\./i.test(thumbPath)) {
    return thumbPath.replace(/-thumb\./i, '-full.');
  }
  return null;
}

function cacheSrc(path: string, src: string) {
  if (src.startsWith('data:')) {
    if (src.length > MAX_DATA_URL_CHARS) {
      // Too large to keep as a string in the JS heap.
      return;
    }
    lruSet(dataUrlCache, path, src, MAX_DATA_URL_CACHE);
    return;
  }
  lruSet(srcCache, path, src, MAX_SRC_CACHE);
}

function lookupCachedSrc(path: string): string | null {
  return lruTouchGet(srcCache, path) ?? lruTouchGet(dataUrlCache, path) ?? null;
}

export function getCoverSrc(coverPath: string | null | undefined): string | null {
  const path = coverPath?.trim();
  if (!path) return null;

  const cached = lookupCachedSrc(path);
  if (cached) return cached;

  // convertFileSrc is a tiny asset:// string — safe to cache for app-data covers.
  const src = convertFileSrc(path);
  cacheSrc(path, src);
  return src;
}

export async function resolveCoverSrc(
  coverPath: string | null | undefined,
): Promise<string | null> {
  const path = coverPath?.trim();
  if (!path) return null;

  const cached = lookupCachedSrc(path);
  if (cached) return cached;

  const pending = inflight.get(path);
  if (pending) return pending;

  const task = (async () => {
    // On-disk cover cache lives under $APPDATA — asset protocol is enough.
    if (isCoverCachePath(path)) {
      const src = convertFileSrc(path);
      cacheSrc(path, src);
      return src;
    }

    // Paths outside asset scope: data URL is a last resort (and size-capped).
    try {
      const dataUrl = await invoke<string | null>('library_cover_data_url', { path });
      if (dataUrl) {
        cacheSrc(path, dataUrl);
        return lookupCachedSrc(path) ?? dataUrl;
      }
    } catch {
      // Fall through to asset URL (may 403 for out-of-scope paths).
    }

    const src = convertFileSrc(path);
    cacheSrc(path, src);
    return src;
  })();

  inflight.set(path, task);

  try {
    return await task;
  } finally {
    inflight.delete(path);
  }
}

/** Lazily extract a track cover only when its virtualized row is on screen. */
export function resolveTrackCoverPath(trackPath: string): Promise<string | null> {
  const path = coverSourcePath(trackPath.trim());
  if (!path) return Promise.resolve(null);

  const key = trackPathKey(path);
  if (trackCoverPaths.has(key)) {
    return Promise.resolve(lruTouchGet(trackCoverPaths, key) ?? null);
  }

  const pending = trackCoverInflight.get(key);
  if (pending) return pending;

  const task = (async () => {
    await acquireTrackCoverSlot();
    try {
      const coverPath = await invoke<string | null>('library_resolve_cover', { path });
      lruSet(trackCoverPaths, key, coverPath, MAX_TRACK_COVER_PATHS);
      return coverPath;
    } catch {
      lruSet(trackCoverPaths, key, null, MAX_TRACK_COVER_PATHS);
      return null;
    } finally {
      releaseTrackCoverSlot();
      trackCoverInflight.delete(key);
    }
  })();

  trackCoverInflight.set(key, task);
  return task;
}

/** Decode into the browser image cache so the next <img src> paints immediately. */
export function warmImageSrc(src: string | null | undefined): Promise<boolean> {
  const url = src?.trim();
  if (!url) return Promise.resolve(false);
  // Never force-decode multi-hundred-KB data URLs into GPU/CPU bitmaps.
  if (url.startsWith('data:') && url.length > 64_000) {
    return Promise.resolve(false);
  }

  return new Promise((resolve) => {
    const img = new Image();
    let settled = false;
    const finish = (ok: boolean) => {
      if (settled) return;
      settled = true;
      // Drop references so the Image can be GC'd after the browser caches the decode.
      img.onload = null;
      img.onerror = null;
      img.src = '';
      resolve(ok);
    };

    img.onload = () => {
      if (typeof img.decode === 'function') {
        img.decode().then(() => finish(true)).catch(() => finish(true));
      } else {
        finish(true);
      }
    };
    img.onerror = () => finish(false);
    img.decoding = 'async';
    img.src = url;
    // Already in memory (e.g. list thumb).
    if (img.complete && img.naturalWidth > 0) {
      finish(true);
    }
  });
}

export type PrefetchOptions = {
  /** Decode into browser image cache (expensive). Default false. */
  warm?: boolean;
  /** Allow *-full.* paths. Default false — full covers are large. */
  includeFull?: boolean;
};

/**
 * Prefetch cover URL strings for upcoming rows.
 * By default only thumbs, no warm-decode (avoids filling the image bitmap cache).
 */
export function prefetchCoverPaths(
  paths: Iterable<string | null | undefined>,
  limit = DEFAULT_PREFETCH_LIMIT,
  options?: PrefetchOptions,
) {
  const warm = options?.warm === true;
  const includeFull = options?.includeFull === true;
  let count = 0;
  for (const raw of paths) {
    if (count >= limit) break;
    const path = raw?.trim();
    if (!path) continue;
    if (!includeFull && isFullCoverPath(path)) continue;
    if (warmed.has(path)) continue;

    warmedAdd(path);
    void resolveCoverSrc(path).then((src) => {
      if (src && warm) void warmImageSrc(src);
    });
    count += 1;
  }
}

/** Drop in-memory URL maps after the on-disk cover cache is rebuilt. */
export function clearCoverSrcCache() {
  srcCache.clear();
  dataUrlCache.clear();
  warmed.clear();
  inflight.clear();
  trackCoverPaths.clear();
  trackCoverInflight.clear();
}

/**
 * Soft trim for long sessions — keeps a few hot entries, drops the rest.
 * Safe to call when fullscreen closes or the window loses focus.
 */
export function trimCoverMemory(keep = 32) {
  const keepSrc = Math.max(8, Math.min(keep, MAX_SRC_CACHE));
  while (srcCache.size > keepSrc) {
    const oldest = srcCache.keys().next().value;
    if (oldest === undefined) break;
    srcCache.delete(oldest);
  }
  dataUrlCache.clear();
  while (warmed.size > Math.min(16, MAX_WARMED)) {
    const oldest = warmed.values().next().value;
    if (oldest === undefined) break;
    warmed.delete(oldest);
  }
  // trackCoverPaths is path→disk path (small); keep a modest tail.
  while (trackCoverPaths.size > 120) {
    const oldest = trackCoverPaths.keys().next().value;
    if (oldest === undefined) break;
    trackCoverPaths.delete(oldest);
  }
}
