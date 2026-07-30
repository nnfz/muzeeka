export interface LyricPart {
  startTimeMs: number;
  words: string;
  durationMs: number;
  isBackground?: boolean;
}

export interface LyricLine {
  startTimeMs: number;
  words: string;
  durationMs: number;
  parts?: LyricPart[];
  isInstrumental?: boolean;
  agent?: string;
}

export type SyncType = 'richsync' | 'synced' | 'none';

export interface LyricsResult {
  lines: LyricLine[];
  syncType: SyncType;
  language?: string;
}