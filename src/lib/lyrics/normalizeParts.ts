import type { LyricPart } from './types';

/**
 * Move leading spaces onto the previous part (trailing).
 * Leading space on the first part is dropped so a wrapped line never starts with a space.
 * Mutates `parts` in place and returns the same array (empty parts removed).
 */
export function normalizePartSpaces(parts: LyricPart[]): LyricPart[] {
  for (let i = 0; i < parts.length; i++) {
    const match = parts[i].words.match(/^(\s*)([\s\S]*)$/);
    if (!match) continue;
    const lead = match[1];
    const rest = match[2];
    if (!lead) continue;
    if (i > 0) {
      parts[i - 1].words += lead;
    }
    parts[i].words = rest;
  }
  return parts.filter((part) => part.words.length > 0);
}
