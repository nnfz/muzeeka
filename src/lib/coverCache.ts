import { convertFileSrc, invoke } from '@tauri-apps/api/core';

const srcCache = new Map<string, string>();
const warmed = new Set<string>();
const inflight = new Map<string, Promise<string | null>>();
const trackCoverPaths = new Map<string, string | null>();
const trackCoverInflight = new Map<string, Promise<string | null>>();
const trackCoverWaiters: Array<() => void> = [];
let activeTrackCoverJobs = 0;
const MAX_TRACK_COVER_JOBS = 2;

function trackPathKey(path: string): string {
  return path.trim().replace(/\//g, '\\').toLowerCase();
}

function coverSourcePath(path: string): string {
  return path.replace(/#cue:\d+$/i, '');
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

function isCoverCachePath(path: string): boolean {
  return /[\\/]covers[\\/]/i.test(path) || /[\\/]playlist_covers[\\/]/i.test(path);
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

export function getCoverSrc(coverPath: string | null | undefined): string | null {
  const path = coverPath?.trim();
  if (!path) return null;

  const cached = srcCache.get(path);
  if (cached) return cached;

  const src = convertFileSrc(path);
  srcCache.set(path, src);
  return src;
}

export async function resolveCoverSrc(
  coverPath: string | null | undefined,
): Promise<string | null> {
  const path = coverPath?.trim();
  if (!path) return null;

  const cached = srcCache.get(path);
  if (cached) return cached;

  const pending = inflight.get(path);
  if (pending) return pending;

  const task = (async () => {
    if (isCoverCachePath(path)) {
      const src = convertFileSrc(path);
      srcCache.set(path, src);
      return src;
    }

    try {
      const dataUrl = await invoke<string | null>('library_cover_data_url', { path });
      if (dataUrl) {
        srcCache.set(path, dataUrl);
        return dataUrl;
      }
    } catch {
      // Fall back to asset URL for scoped paths.
    }

    const src = convertFileSrc(path);
    srcCache.set(path, src);
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
    return Promise.resolve(trackCoverPaths.get(key) ?? null);
  }

  const pending = trackCoverInflight.get(key);
  if (pending) return pending;

  const task = (async () => {
    await acquireTrackCoverSlot();
    try {
      const coverPath = await invoke<string | null>('library_resolve_cover', { path });
      trackCoverPaths.set(key, coverPath);
      return coverPath;
    } catch {
      trackCoverPaths.set(key, null);
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

  return new Promise((resolve) => {
    const img = new Image();
    let settled = false;
    const finish = (ok: boolean) => {
      if (settled) return;
      settled = true;
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

export function prefetchCoverPaths(
  paths: Iterable<string | null | undefined>,
  limit = 96,
) {
  let count = 0;
  for (const raw of paths) {
    if (count >= limit) break;
    const path = raw?.trim();
    if (!path || warmed.has(path)) continue;

    warmed.add(path);
    void resolveCoverSrc(path).then((src) => {
      if (src) void warmImageSrc(src);
    });
    count += 1;
  }
}

/** Drop in-memory URL maps after the on-disk cover cache is rebuilt. */
export function clearCoverSrcCache() {
  srcCache.clear();
  warmed.clear();
  inflight.clear();
  trackCoverPaths.clear();
  trackCoverInflight.clear();
}
