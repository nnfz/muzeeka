import type { LyricLine, LyricPart, SyncType } from './types';

export function detectSyncType(lines: LyricLine[]): SyncType {
  if (lines.length === 0) return 'none';
  if (lines.every((line) => line.startTimeMs === 0)) return 'none';
  // Multiple timed parts per line = word-level sync; a single span is line-level (e.g. LRCLIB).
  if (lines.some((line) => (line.parts?.length ?? 0) > 1)) {
    return 'richsync';
  }
  return 'synced';
}

export function lineStartSec(line: LyricLine): number {
  return line.startTimeMs / 1000;
}

export function lineEndSec(line: LyricLine, nextLine?: LyricLine): number {
  if (nextLine) return nextLine.startTimeMs / 1000;
  return line.startTimeMs / 1000 + line.durationMs / 1000;
}

export function partStartSec(part: LyricPart): number {
  return part.startTimeMs / 1000;
}

export function partEndSec(part: LyricPart, nextPart?: LyricPart, lineEnd?: number): number {
  if (nextPart) return nextPart.startTimeMs / 1000;
  if (part.durationMs > 0) {
    return part.startTimeMs / 1000 + part.durationMs / 1000;
  }
  if (lineEnd != null) return lineEnd;
  return part.startTimeMs / 1000;
}

export function findActiveLineIndex(lines: LyricLine[], currentTime: number): number {
  if (lines.length === 0) return -1;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const end = lineEndSec(line, lines[i + 1]);
    if (currentTime >= lineStartSec(line) && currentTime < end) {
      return i;
    }
  }

  if (currentTime >= lineStartSec(lines[lines.length - 1])) {
    return lines.length - 1;
  }

  return 0;
}

export function isLineActive(
  line: LyricLine,
  index: number,
  lines: LyricLine[],
  currentTime: number,
): boolean {
  const start = lineStartSec(line);
  const end = lineEndSec(line, lines[index + 1]);
  return currentTime >= start && currentTime < end + 0.05;
}

export function isPartActive(
  part: LyricPart,
  partIndex: number,
  parts: LyricPart[],
  line: LyricLine,
  lineIndex: number,
  lines: LyricLine[],
  currentTime: number,
): boolean {
  if (!isLineActive(line, lineIndex, lines, currentTime)) return false;

  const start = partStartSec(part);
  const nextPart = parts[partIndex + 1];
  const end = partEndSec(part, nextPart, lineEndSec(line, lines[lineIndex + 1]));
  return currentTime >= start && currentTime < end + 0.05;
}

export function isPartSung(
  part: LyricPart,
  partIndex: number,
  parts: LyricPart[],
  line: LyricLine,
  lineIndex: number,
  lines: LyricLine[],
  currentTime: number,
): boolean {
  if (isLinePast(line, lineIndex, lines, currentTime)) return true;

  const start = partStartSec(part);
  if (currentTime < start) return false;

  const nextPart = parts[partIndex + 1];
  const end = partEndSec(part, nextPart, lineEndSec(line, lines[lineIndex + 1]));
  return currentTime >= end - 0.02;
}

export function isLinePast(
  line: LyricLine,
  index: number,
  lines: LyricLine[],
  currentTime: number,
): boolean {
  const end = lineEndSec(line, lines[index + 1]);
  return currentTime >= end - 0.02;
}

/**
 * Line highlight lead (braccato: --braccato-timing-offset ~0.115s,
 * richsync ~0.15s). Scroll can feel slightly earlier than the sung word.
 */
export const LINE_ACTIVE_LEAD_SEC = 0.15;

/**
 * Active line index with braccato-style timing offset.
 */
export function findDisplayActiveLineIndex(lines: LyricLine[], currentTime: number): number {
  return findActiveLineIndex(lines, currentTime + LINE_ACTIVE_LEAD_SEC);
}

export function animationDelay(currentTime: number, startSec: number): string {
  return `${-(currentTime - startSec)}s`;
}

const HIGHLIGHT_IDLE_START = -0.2;
const HIGHLIGHT_IDLE_END = -0.1;
const HIGHLIGHT_FULL_START = 1.5;
const HIGHLIGHT_FULL_END = 1.6;
const HIGHLIGHT_SWIPE_LEAD = 0.05;

export function highlightAmounts(progress: number): { start: number; end: number } {
  const p = Math.min(Math.max(progress, 0), 1);
  const span = HIGHLIGHT_FULL_START - HIGHLIGHT_IDLE_START;
  return {
    start: HIGHLIGHT_IDLE_START + p * span,
    end: HIGHLIGHT_IDLE_END + p * (HIGHLIGHT_FULL_END - HIGHLIGHT_IDLE_END),
  };
}

export function partDurationSec(
  part: LyricPart,
  partIndex: number,
  parts: LyricPart[],
  line: LyricLine,
  lineIndex: number,
  lines: LyricLine[],
): number {
  const start = partStartSec(part);
  const end = partEndSec(part, parts[partIndex + 1], lineEndSec(line, lines[lineIndex + 1]));
  return Math.max(end - start, 0.05);
}

export function partHighlightProgress(
  part: LyricPart,
  partIndex: number,
  parts: LyricPart[],
  line: LyricLine,
  lineIndex: number,
  lines: LyricLine[],
  currentTime: number,
): number {
  if (isPartSung(part, partIndex, parts, line, lineIndex, lines, currentTime)) {
    return 1;
  }

  const start = partStartSec(part);
  const durationSec = partDurationSec(part, partIndex, parts, line, lineIndex, lines);
  const elapsed = currentTime - start;
  const fillStart = durationSec * HIGHLIGHT_SWIPE_LEAD;
  const fillWindow = durationSec * (1 - HIGHLIGHT_SWIPE_LEAD);

  if (elapsed <= fillStart) return 0;
  return Math.min((elapsed - fillStart) / fillWindow, 1);
}

export function lineHighlightProgress(
  line: LyricLine,
  lineIndex: number,
  lines: LyricLine[],
  currentTime: number,
): number {
  if (isLinePast(line, lineIndex, lines, currentTime)) {
    return 1;
  }

  const start = lineStartSec(line);
  const durationSec = Math.max(lineEndSec(line, lines[lineIndex + 1]) - start, 0.05);
  const elapsed = currentTime - start;
  const fillStart = durationSec * HIGHLIGHT_SWIPE_LEAD;
  const fillWindow = durationSec * (1 - HIGHLIGHT_SWIPE_LEAD);

  if (elapsed <= fillStart) return 0;
  return Math.min((elapsed - fillStart) / fillWindow, 1);
}