/**
 * Pre-compute soft line wraps for lyrics using active (widest) font metrics.
 * Prevents mid-animation reflow when font-weight/scale change (600→700, scale 1.05).
 */

export type LayoutToken = {
  text: string;
  isBackground?: boolean;
};

export type LineLayout = {
  /** Token indices that begin a visual row (includes 0 when tokens exist). */
  rowStarts: number[];
  /** Plain/synced: joined text per visual row. Empty for richsync (use rowStarts). */
  rows: string[];
};

export type MeasureStyles = {
  fontFamily: string;
  fontSizePx: number;
  fontWeight: string;
  letterSpacing: string;
  fontStyle?: string;
  /** Matches .fs-lyrics-word.is-background */
  backgroundScale?: number;
  /**
   * When true, measure like richsync karaoke: each glyph is inline-block
   * (no kerning) — slightly wider than plain text runs.
   */
  perCharInlineBlock?: boolean;
};

const DEFAULT_BG_SCALE = 0.78;

/** Split plain text into tokens that keep trailing whitespace with the word. */
export function tokenizePlain(text: string): LayoutToken[] {
  if (!text) return [];
  const tokens: LayoutToken[] = [];
  const re = /\S+\s*|\s+/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    tokens.push({ text: m[0] });
  }
  return tokens;
}

/**
 * Greedy wrap: place tokens on rows that fit maxWidth (active/bold metrics).
 * First token on a row always stays even if wider than maxWidth.
 */
export function wrapTokens(
  tokens: LayoutToken[],
  maxWidth: number,
  measure: (token: LayoutToken) => number,
): LineLayout {
  if (tokens.length === 0) {
    return { rowStarts: [], rows: [] };
  }
  if (maxWidth <= 0) {
    return {
      rowStarts: tokens.map((_, i) => i),
      rows: tokens.map((t) => t.text),
    };
  }

  const rowStarts = [0];
  let rowWidth = 0;

  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    // Empty placeholders (aligned richsync indices) never force a wrap alone.
    if (!token.text) {
      if (i === rowStarts[rowStarts.length - 1]) rowWidth = 0;
      continue;
    }
    const w = measure(token);
    if (i === rowStarts[rowStarts.length - 1]) {
      rowWidth = w;
      continue;
    }
    if (rowWidth + w > maxWidth) {
      rowStarts.push(i);
      rowWidth = w;
    } else {
      rowWidth += w;
    }
  }

  const rows: string[] = [];
  for (let r = 0; r < rowStarts.length; r++) {
    const start = rowStarts[r];
    const end = r + 1 < rowStarts.length ? rowStarts[r + 1] : tokens.length;
    let s = '';
    for (let i = start; i < end; i++) s += tokens[i].text;
    rows.push(s);
  }

  return { rowStarts, rows };
}

export function createTextMeasurer(styles: MeasureStyles) {
  const bgScale = styles.backgroundScale ?? DEFAULT_BG_SCALE;
  const perChar = styles.perCharInlineBlock === true;
  const el = document.createElement('span');
  el.setAttribute('aria-hidden', 'true');
  el.style.cssText = [
    'position:absolute',
    'visibility:hidden',
    'pointer-events:none',
    'white-space:pre',
    'top:0',
    'left:0',
    'margin:0',
    'padding:0',
    'border:0',
    `font-family:${styles.fontFamily}`,
    `font-size:${styles.fontSizePx}px`,
    `font-weight:${styles.fontWeight}`,
    `font-style:${styles.fontStyle ?? 'normal'}`,
    `letter-spacing:${styles.letterSpacing}`,
    // Match lyrics container optical settings
    'font-variant-ligatures:none',
  ].join(';');
  document.body.appendChild(el);

  const cache = new Map<string, number>();

  function fillPerChar(text: string) {
    el.replaceChildren();
    for (const ch of text) {
      const span = document.createElement('span');
      span.style.display = 'inline-block';
      span.style.whiteSpace = 'pre';
      span.textContent = ch;
      el.appendChild(span);
    }
  }

  function measure(token: LayoutToken): number {
    const key = `${perChar ? 'c' : 'p'}${token.isBackground ? 'b' : 'n'}\0${token.text}`;
    const hit = cache.get(key);
    if (hit != null) return hit;

    const size = token.isBackground
      ? styles.fontSizePx * bgScale
      : styles.fontSizePx;
    el.style.fontSize = `${size}px`;
    if (perChar) {
      fillPerChar(token.text);
    } else {
      el.textContent = token.text;
    }
    const w = el.getBoundingClientRect().width;
    cache.set(key, w);
    return w;
  }

  function destroy() {
    el.remove();
    cache.clear();
  }

  return { measure, destroy };
}

export type LayoutOptions = {
  /** Content box width available for text (after horizontal padding). */
  contentWidth: number;
  /** Active line scale — reserve so scaled text still fits visually. */
  activeScale?: number;
  /** Safety factor against subpixel under-measure (slightly earlier wraps). */
  safety?: number;
  styles: MeasureStyles;
};

/**
 * Layout one lyric line from already-built tokens.
 */
export function layoutTokens(
  tokens: LayoutToken[],
  options: LayoutOptions,
): LineLayout {
  const scale = options.activeScale ?? 1.05;
  const safety = options.safety ?? 0.98;
  const maxWidth = Math.max(0, (options.contentWidth / scale) * safety);
  const measurer = createTextMeasurer(options.styles);
  try {
    return wrapTokens(tokens, maxWidth, measurer.measure);
  } finally {
    measurer.destroy();
  }
}

/**
 * Batch-layout many lines with one shared measurer (faster for full track).
 */
export function layoutAllLines(
  linesTokens: LayoutToken[][],
  options: LayoutOptions,
): LineLayout[] {
  const scale = options.activeScale ?? 1.05;
  const safety = options.safety ?? 0.98;
  const maxWidth = Math.max(0, (options.contentWidth / scale) * safety);
  const measurer = createTextMeasurer(options.styles);
  try {
    return linesTokens.map((tokens) => wrapTokens(tokens, maxWidth, measurer.measure));
  } finally {
    measurer.destroy();
  }
}

/** Whether token index starts a precomputed visual row. */
export function isRowStart(layout: LineLayout | undefined, tokenIndex: number): boolean {
  if (!layout || layout.rowStarts.length === 0) {
    // No layout yet — only index 0 is a row start (natural wrap elsewhere).
    return tokenIndex === 0;
  }
  return layout.rowStarts.includes(tokenIndex);
}
