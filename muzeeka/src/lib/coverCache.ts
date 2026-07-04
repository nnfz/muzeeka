import { convertFileSrc } from '@tauri-apps/api/core';

const srcCache = new Map<string, string>();
const warmed = new Set<string>();

export function getCoverSrc(coverPath: string | null | undefined): string | null {
  const path = coverPath?.trim();
  if (!path) return null;

  const cached = srcCache.get(path);
  if (cached) return cached;

  const src = convertFileSrc(path);
  srcCache.set(path, src);
  return src;
}

export function prefetchCoverPaths(
  paths: Iterable<string | null | undefined>,
  limit = 96
) {
  let count = 0;
  for (const raw of paths) {
    if (count >= limit) break;
    const path = raw?.trim();
    if (!path || warmed.has(path)) continue;

    const src = getCoverSrc(path);
    if (!src) continue;

    warmed.add(path);
    const img = new Image();
    img.decoding = 'async';
    img.src = src;
    count += 1;
  }
}