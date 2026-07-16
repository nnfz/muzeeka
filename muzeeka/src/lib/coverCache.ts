import { convertFileSrc, invoke } from '@tauri-apps/api/core';

const srcCache = new Map<string, string>();
const warmed = new Set<string>();
const inflight = new Map<string, Promise<string | null>>();

function isCoverCachePath(path: string): boolean {
  return /[\\/]covers[\\/]/i.test(path) || /[\\/]playlist_covers[\\/]/i.test(path);
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
      if (!src) return;
      const img = new Image();
      img.decoding = 'async';
      img.src = src;
    });
    count += 1;
  }
}