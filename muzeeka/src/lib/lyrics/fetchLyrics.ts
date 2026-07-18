import { invoke } from '@tauri-apps/api/core';
import { parseTtml } from './parseTtml';
import { detectSyncType } from './sync';
import type { LyricsResult } from './types';

const LYRICS_API = 'https://lyrics-api.boidu.dev';
const USER_AGENT = 'Muzeeka/0.1.0';
const NO_LYRICS_SENTINEL = '__NO_LYRICS__';

export interface FetchLyricsParams {
  title: string;
  artist: string;
  album?: string | null;
  durationSecs?: number | null;
}

function buildLyricsUrl(params: FetchLyricsParams, durationSecs: number | null): string {
  const title = params.title.trim();
  const artist = params.artist.trim();
  const search = new URLSearchParams({
    s: title,
    a: artist,
  });

  const album = params.album?.trim();
  if (album) {
    search.set('al', album);
  }

  if (durationSecs != null && durationSecs > 0) {
    search.set('d', String(durationSecs));
  }

  return `${LYRICS_API}/getLyrics?${search.toString()}`;
}

async function fetchTtmlFromApi(url: string): Promise<string | null> {
  const response = await fetch(url, {
    headers: {
      Accept: 'application/json',
      'User-Agent': USER_AGENT,
    },
  });

  if (response.status === 404 || response.status === 401 || response.status === 429) {
    return null;
  }

  if (!response.ok) {
    throw new Error(`Lyrics API returned HTTP ${response.status}`);
  }

  const body = (await response.json()) as { ttml?: string | null };
  const ttml = body.ttml?.trim();
  if (!ttml || ttml === NO_LYRICS_SENTINEL) {
    return null;
  }

  return ttml;
}

async function fetchTtml(params: FetchLyricsParams, durationSecs: number | null): Promise<string | null> {
  // Rust path tries Better Lyrics → LRCLIB → Unison. Do not fall through to a
  // browser-only BL call on null — that just 401s on cache misses and confuses debugging.
  try {
    const ttml = await invoke<string | null>('lyrics_fetch', {
      title: params.title.trim(),
      artist: params.artist.trim(),
      album: params.album?.trim() || null,
      durationSecs,
    });
    return ttml?.trim() ? ttml : null;
  } catch (rustError) {
    const url = buildLyricsUrl(params, durationSecs);
    try {
      return await fetchTtmlFromApi(url);
    } catch (browserError) {
      const rustMessage = rustError instanceof Error ? rustError.message : String(rustError);
      const browserMessage = browserError instanceof Error ? browserError.message : String(browserError);
      throw new Error(`${rustMessage} (browser fallback: ${browserMessage})`);
    }
  }
}

export async function fetchLyrics(params: FetchLyricsParams): Promise<LyricsResult | null> {
  const title = params.title.trim();
  const artist = params.artist.trim();
  if (!title && !artist) return null;

  const durationSecs =
    params.durationSecs != null && params.durationSecs > 0
      ? Math.round(params.durationSecs)
      : null;

  const ttml = await fetchTtml(params, durationSecs);
  if (!ttml?.trim()) return null;

  const songDurationMs = Math.max((durationSecs ?? 0) * 1000, 1);
  let lines;
  try {
    lines = parseTtml(ttml, songDurationMs);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`Failed to parse lyrics: ${message}`);
  }

  if (lines.length === 0) return null;

  return {
    lines,
    syncType: detectSyncType(lines),
  };
}