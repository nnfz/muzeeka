import type { LyricLine, LyricPart } from './types';
import { normalizePartSpaces } from './normalizeParts';

const INSTRUMENTAL_GAP_MS = 3000;

function parseTime(timeStr: string | undefined): number {
  if (!timeStr) return 0;

  const offsetMatch = timeStr.match(/^([\d.]+)(h|m|s|ms)$/);
  if (offsetMatch) {
    const value = parseFloat(offsetMatch[1]);
    const unit = offsetMatch[2];
    if (unit === 'h') return Math.round(value * 3600 * 1000);
    if (unit === 'm') return Math.round(value * 60 * 1000);
    if (unit === 's') return Math.round(value * 1000);
    if (unit === 'ms') return Math.round(value);
  }

  const parts = timeStr.split(':').map((v) => v.replace(/[^0-9.]/g, ''));
  try {
    if (parts.length === 1) {
      return Math.round(parseFloat(parts[0]) * 1000);
    }
    if (parts.length === 2) {
      const minutes = parseInt(parts[0], 10);
      const seconds = parseFloat(parts[1]);
      return Math.round(minutes * 60 * 1000 + seconds * 1000);
    }
    if (parts.length === 3) {
      const hours = parseInt(parts[0], 10);
      const minutes = parseInt(parts[1], 10);
      const seconds = parseFloat(parts[2]);
      return Math.round(hours * 3600 * 1000 + minutes * 60 * 1000 + seconds * 1000);
    }
  } catch {
    return 0;
  }

  return 0;
}

function childElements(parent: Element, localName: string): Element[] {
  return [...parent.children].filter((child) => child.localName === localName);
}

function findLyricParagraphs(doc: Document): Element[] {
  const paragraphs: Element[] = [];
  const root = doc.documentElement;
  if (!root) return paragraphs;

  const stack: Element[] = [root];
  while (stack.length > 0) {
    const element = stack.pop()!;
    if (element.localName === 'p' && element.getAttribute('begin')) {
      paragraphs.push(element);
    }

    for (let i = element.children.length - 1; i >= 0; i--) {
      stack.push(element.children[i] as Element);
    }
  }

  return paragraphs;
}

function getPrefixedAttribute(element: Element, name: string): string | undefined {
  return (
    element.getAttribute(name)
    ?? element.getAttribute(`ttm:${name}`)
    ?? element.getAttribute(`itunes:${name}`)
    ?? undefined
  );
}

function declareMissingNamespaces(content: string): string {
  const rootMatch = content.match(/<tt\b[^>]*>/);
  if (!rootMatch) return content;

  const rootTag = rootMatch[0];
  const declared = new Set(['xml', 'xmlns']);
  for (const match of rootTag.matchAll(/xmlns:([A-Za-z][\w.-]*)\s*=/g)) {
    declared.add(match[1]);
  }

  const used = new Set<string>();
  for (const match of content.matchAll(/<(?:\/)?([A-Za-z][\w.-]*):/g)) {
    used.add(match[1]);
  }
  for (const match of content.matchAll(/\s([A-Za-z][\w.-]*):[\w.-]+\s*=/g)) {
    used.add(match[1]);
  }

  const missing = [...used].filter((prefix) => !declared.has(prefix));
  if (missing.length === 0) return content;

  const additions = missing
    .map((prefix) => ` xmlns:${prefix}="urn:better-lyrics:unbound:${prefix}"`)
    .join('');
  const patchedRoot = rootTag.replace(/>$/, `${additions}>`);
  return content.replace(rootTag, patchedRoot);
}

function pushSpanPart(
  span: Element,
  parts: LyricPart[],
  isBackground: boolean,
  prefix = '',
) {
  const spanText = span.textContent ?? '';
  const words = `${prefix}${spanText}`;
  if (!words) return;

  const startTimeMs = parseTime(span.getAttribute('begin') ?? undefined);
  const endTimeMs = parseTime(span.getAttribute('end') ?? undefined);

  parts.push({
    startTimeMs,
    words,
    durationMs: Math.max(endTimeMs - startTimeMs, 0),
    isBackground,
  });
}

function parseParagraphParts(
  paragraph: Element,
  beginTimeMs: number,
): { parts: LyricPart[]; text: string; isWordSynced: boolean } {
  const parts: LyricPart[] = [];
  let isWordSynced = false;
  let pendingSpace = '';

  const nodes = [...paragraph.childNodes];
  if (nodes.length === 0) {
    const fallback = paragraph.textContent?.trim() ?? '';
    return { parts: [], text: fallback, isWordSynced: false };
  }

  for (const node of nodes) {
    if (node.nodeType === Node.TEXT_NODE) {
      const chunk = node.textContent ?? '';
      if (!chunk) continue;
      if (!chunk.trim()) {
        pendingSpace += chunk;
      } else {
        parts.push({
          startTimeMs: parts.at(-1)
            ? parts.at(-1)!.startTimeMs + parts.at(-1)!.durationMs
            : beginTimeMs,
          words: chunk,
          durationMs: 0,
        });
      }
      continue;
    }

    if (node.nodeType !== Node.ELEMENT_NODE) continue;
    const element = node as Element;

    if (getPrefixedAttribute(element, 'role') === 'x-bg') {
      const bgSpans = childElements(element, 'span');
      const bgNodes = bgSpans.length > 0 ? bgSpans : [element];
      for (const span of bgNodes) {
        pushSpanPart(span, parts, true, pendingSpace);
        pendingSpace = '';
        if (span.textContent) isWordSynced = true;
      }
      continue;
    }

    if (element.localName === 'span' || element.localName === 'text') {
      pushSpanPart(element, parts, false, pendingSpace);
      pendingSpace = '';
      if (element.textContent) isWordSynced = true;
      continue;
    }

    const chunk = element.textContent ?? '';
    if (chunk) {
      parts.push({
        startTimeMs: parts.at(-1)
          ? parts.at(-1)!.startTimeMs + parts.at(-1)!.durationMs
          : beginTimeMs,
        words: `${pendingSpace}${chunk}`,
        durationMs: 0,
      });
      pendingSpace = '';
    }
  }

  if (pendingSpace && parts.length > 0) {
    parts[parts.length - 1].words += pendingSpace;
  }

  const text = paragraph.textContent?.replace(/\s+/g, ' ').trim() ?? '';

  if (!isWordSynced) {
    return { parts: [], text, isWordSynced: false };
  }

  return { parts: normalizePartSpaces(parts), text, isWordSynced };
}

function finalizeDurations(lines: LyricLine[], songDurationMs: number) {
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const next = lines[i + 1];

    if (line.durationMs === 0) {
      line.durationMs = next
        ? Math.max(next.startTimeMs - line.startTimeMs, 0)
        : Math.max(songDurationMs - line.startTimeMs, 0);
    }

    if (line.parts && line.parts.length > 0) {
      for (let j = 0; j < line.parts.length; j++) {
        const part = line.parts[j];
        const nextPart = line.parts[j + 1];
        if (part.durationMs === 0) {
          if (nextPart) {
            part.durationMs = Math.max(nextPart.startTimeMs - part.startTimeMs, 0);
          } else {
            const lineEndMs = line.startTimeMs + line.durationMs;
            const remainingMs = Math.max(lineEndMs - part.startTimeMs, 0);
            const prevPart = line.parts[j - 1];
            const guessMs = prevPart
              ? Math.max(prevPart.durationMs, 250)
              : Math.min(remainingMs, 600);
            part.durationMs = Math.min(remainingMs, guessMs);
          }
        }
      }
    }
  }
}

function insertInstrumentalBreaks(lines: LyricLine[], songDurationMs: number): LyricLine[] {
  if (lines.length === 0) return lines;

  const result: LyricLine[] = [];
  const createBreak = (startTimeMs: number, durationMs: number): LyricLine => ({
    startTimeMs,
    durationMs,
    words: '',
    isInstrumental: true,
  });

  if (lines[0].startTimeMs > INSTRUMENTAL_GAP_MS) {
    result.push(createBreak(0, lines[0].startTimeMs));
  }

  for (let i = 0; i < lines.length; i++) {
    result.push(lines[i]);
    if (i < lines.length - 1) {
      const currentEnd = lines[i].startTimeMs + lines[i].durationMs;
      const nextStart = lines[i + 1].startTimeMs;
      const gap = nextStart - currentEnd;
      if (gap > INSTRUMENTAL_GAP_MS) {
        result.push(createBreak(currentEnd, gap));
      }
    }
  }

  const last = lines[lines.length - 1];
  const lastEnd = last.startTimeMs + last.durationMs;
  const outroGap = songDurationMs - lastEnd;
  if (outroGap > INSTRUMENTAL_GAP_MS) {
    result.push(createBreak(lastEnd, outroGap));
  }

  return result;
}

export function parseTtml(ttml: string, songDurationMs: number): LyricLine[] {
  const sanitized = declareMissingNamespaces(ttml);
  const doc = new DOMParser().parseFromString(sanitized, 'application/xml');
  const parserError = doc.querySelector('parsererror');
  if (parserError) {
    const detail = parserError.textContent?.trim();
    throw new Error(detail ? `Invalid TTML document: ${detail}` : 'Invalid TTML document');
  }

  const paragraphs = findLyricParagraphs(doc);
  const lines: LyricLine[] = [];

  for (const paragraph of paragraphs) {
    const begin = paragraph.getAttribute('begin');
    if (!begin) continue;

    const beginMs = parseTime(begin);
    const endMs = parseTime(paragraph.getAttribute('end') ?? undefined);
    const parsed = parseParagraphParts(paragraph, beginMs);
    if (!parsed.text) continue;

    lines.push({
      startTimeMs: beginMs,
      durationMs: Math.max(endMs - beginMs, 0),
      words: parsed.text,
      parts: parsed.isWordSynced && parsed.parts.length > 0 ? parsed.parts : undefined,
      agent: getPrefixedAttribute(paragraph, 'agent'),
    });
  }

  lines.sort((a, b) => a.startTimeMs - b.startTimeMs);
  finalizeDurations(lines, songDurationMs);

  const lang =
    doc.querySelector('tt')?.getAttribute('xml:lang')
    ?? doc.querySelector('tt')?.getAttribute('lang')
    ?? undefined;

  void lang;
  return insertInstrumentalBreaks(lines, songDurationMs);
}