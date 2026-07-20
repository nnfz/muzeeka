<script lang="ts">
  import type { LyricLine, LyricPart, SyncType } from '$lib/lyrics/types';
  import { normalizePartSpaces } from '$lib/lyrics/normalizeParts';
  import {
    isRowStart,
    layoutAllLines,
    tokenizePlain,
    type LayoutToken,
    type LineLayout,
  } from '$lib/lyrics/layoutLines';
  import {
    findActiveLineIndex,
    findDisplayActiveLineIndex,
    isPartActive,
    isPartSung,
    lineEndSec,
    lineStartSec,
    partDurationSec,
    partStartSec,
  } from '$lib/lyrics/sync';

  interface Props {
    lines?: LyricLine[];
    syncType?: SyncType;
    currentTime?: number;
    isPlaying?: boolean;
    chromeVisible?: boolean;
    onSeek?: (timeSec: number) => void;
  }

  const SCROLL_MIN_MS = 520;
  const SCROLL_MAX_MS = 750;
  const SCROLL_PX_FACTOR = 0.55;
  /** Matches .fs-lyrics-line horizontal padding (0.35em each side). */
  const LINE_PAD_EM = 0.35;
  const DEFAULT_ACTIVE_SCALE = 1.05;
  const ACTIVE_FONT_WEIGHT = '700';

  let {
    lines = [],
    syncType = 'none',
    currentTime = 0,
    isPlaying = false,
    chromeVisible = true,
    onSeek,
  }: Props = $props();

  let viewportEl = $state<HTMLDivElement | undefined>();
  let containerEl = $state<HTMLDivElement | undefined>();
  let edgeTopEl = $state<HTMLDivElement | undefined>();
  let edgeBottomEl = $state<HTMLDivElement | undefined>();
  /** Precomputed soft wraps (active/bold metrics) so scale/weight don't reflow mid-anim. */
  let lineLayouts = $state<LineLayout[]>([]);
  let lastScrolledIndex = -1;
  let scrollRaf = 0;
  let scrollToken = 0;
  let layoutRaf = 0;

  /**
   * Smooth media clock: player position only arrives ~every 50ms.
   * Extrapolate with rAF while playing so word-fill runs at display refresh.
   */
  let clockPos = 0;
  let clockWall = 0;
  let clockPlaying = false;
  let fillRaf = 0;
  let fillWordEl: HTMLElement | null = null;
  let fillWordId = '';

  let activeLineIndex = $derived(findDisplayActiveLineIndex(lines, currentTime));

  function easeScroll(t: number): number {
    const u = 1 - t;
    return 1 - u * u * u * (1 + 2.2 * t);
  }

  function cancelScrollAnim() {
    if (scrollRaf) {
      cancelAnimationFrame(scrollRaf);
      scrollRaf = 0;
    }
    scrollToken += 1;
  }

  /** Half-viewport edges so first/last lines can sit in the optical center */
  function updateEdgeSpacers() {
    if (!viewportEl) return;
    const half = Math.max(0, viewportEl.clientHeight / 2);
    const px = `${half}px`;
    if (edgeTopEl) edgeTopEl.style.height = px;
    if (edgeBottomEl) edgeBottomEl.style.height = px;
  }

  /**
   * Scroll so the line's vertical center sits at the viewport center.
   * Edge spacers provide room for first/last lines — not padding on the text block.
   */
  function scrollLineToCenter(lineEl: HTMLElement, smooth: boolean) {
    if (!viewportEl || !containerEl) return;

    const viewportH = viewportEl.clientHeight;
    if (viewportH <= 0) return;

    const viewportRect = viewportEl.getBoundingClientRect();
    const lineRect = lineEl.getBoundingClientRect();
    const lineCenter =
      lineRect.top + lineRect.height / 2 - viewportRect.top + viewportEl.scrollTop;

    const targetScroll = Math.max(0, lineCenter - viewportH / 2);
    const startScroll = viewportEl.scrollTop;
    const delta = targetScroll - startScroll;

    if (Math.abs(delta) < 0.5) return;

    if (!smooth) {
      cancelScrollAnim();
      viewportEl.scrollTop = targetScroll;
      return;
    }

    cancelScrollAnim();
    const token = scrollToken;
    const duration = Math.min(
      SCROLL_MAX_MS,
      Math.max(SCROLL_MIN_MS, Math.abs(delta) * SCROLL_PX_FACTOR),
    );
    const startTime = performance.now();

    const tick = (now: number) => {
      if (token !== scrollToken || !viewportEl) return;

      const t = Math.min(1, (now - startTime) / duration);
      viewportEl.scrollTop = startScroll + delta * easeScroll(t);

      if (t < 1) {
        scrollRaf = requestAnimationFrame(tick);
      } else {
        scrollRaf = 0;
      }
    };

    scrollRaf = requestAnimationFrame(tick);
  }

  function scrollActiveLine(smooth: boolean) {
    if (!viewportEl || !containerEl || lines.length === 0 || syncType === 'none') return;
    if (activeLineIndex < 0) return;

    const lineEl = containerEl.querySelector<HTMLElement>(`[data-line="${activeLineIndex}"]`);
    if (!lineEl) return;

    scrollLineToCenter(lineEl, smooth);
  }

  function scrollActiveLineAfterLayout(smooth: boolean) {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => scrollActiveLine(smooth));
    });
  }

  function displayParts(line: LyricLine): LyricPart[] {
    if (line.parts && line.parts.length > 0) {
      // Trailing spaces (not leading) so a soft-wrapped row never starts with a space.
      return normalizePartSpaces(line.parts.map((part) => ({ ...part, words: part.words })));
    }
    return [{
      startTimeMs: line.startTimeMs,
      words: line.words,
      durationMs: line.durationMs,
    }];
  }

  function tokensForLine(line: LyricLine): LayoutToken[] {
    if (line.isInstrumental) return [];
    // Keep indices aligned with the richsync {#each parts} loop (empty parts width 0).
    if (line.parts && line.parts.length > 0 && syncType === 'richsync') {
      return displayParts(line).map((p) => ({
        text: p.words ?? '',
        isBackground: p.isBackground,
      }));
    }
    return tokenizePlain(line.words ?? '');
  }

  function readActiveScale(): number {
    if (!containerEl) return DEFAULT_ACTIVE_SCALE;
    const panel = containerEl.closest('.fs-lyrics-panel') as HTMLElement | null;
    const raw = panel
      ? getComputedStyle(panel).getPropertyValue('--lyric-active-scale').trim()
      : '';
    const n = parseFloat(raw);
    return Number.isFinite(n) && n > 0 ? n : DEFAULT_ACTIVE_SCALE;
  }

  /**
   * Measure wraps with active font-weight + reserved scale width so when a line
   * becomes bold/scaled it already occupies the final number of visual rows.
   */
  function recomputeLineLayouts() {
    if (!containerEl || lines.length === 0) {
      lineLayouts = [];
      return;
    }

    const width = containerEl.clientWidth;
    if (width <= 0) {
      lineLayouts = [];
      return;
    }

    const cs = getComputedStyle(containerEl);
    const fontSizePx = parseFloat(cs.fontSize);
    if (!Number.isFinite(fontSizePx) || fontSizePx <= 0) {
      lineLayouts = [];
      return;
    }

    const padX = LINE_PAD_EM * fontSizePx * 2;
    const contentWidth = Math.max(0, width - padX);
    const activeScale = readActiveScale();

    const linesTokens = lines.map((line) => tokensForLine(line));
    lineLayouts = layoutAllLines(linesTokens, {
      contentWidth,
      activeScale,
      styles: {
        fontFamily: cs.fontFamily,
        fontSizePx,
        fontWeight: ACTIVE_FONT_WEIGHT,
        letterSpacing: cs.letterSpacing,
        fontStyle: cs.fontStyle,
        // Richsync words are per-glyph inline-block (wider than kerned runs)
        perCharInlineBlock: syncType === 'richsync',
      },
    });
  }

  function scheduleLayoutRecompute() {
    if (layoutRaf) cancelAnimationFrame(layoutRaf);
    layoutRaf = requestAnimationFrame(() => {
      layoutRaf = 0;
      recomputeLineLayouts();
      // Heights change once soft-breaks apply — re-center without smooth scroll
      if (activeLineIndex >= 0) {
        requestAnimationFrame(() => scrollActiveLine(false));
      }
    });
  }

  function plainRows(lineIndex: number, fallback: string): string[] {
    const layout = lineLayouts[lineIndex];
    if (layout?.rows?.length) return layout.rows;
    return fallback ? [fallback] : [];
  }

  function richPartRowBreak(lineIndex: number, partIndex: number): boolean {
    return partIndex > 0 && isRowStart(lineLayouts[lineIndex], partIndex);
  }

  function seekToLine(line: LyricLine) {
    onSeek?.(lineStartSec(line));
  }

  function seekToPart(part: LyricPart) {
    onSeek?.(part.startTimeMs / 1000);
  }

  function wordId(lineIndex: number, partIndex: number, part: LyricPart): string {
    return `${lineIndex}:${partIndex}:${part.startTimeMs}`;
  }

  /** 0…1 progress through an instrumental break (past = full, future = empty). */
  function instrumentalProgress(line: LyricLine, lineIndex: number, time: number): number {
    const start = lineStartSec(line);
    const end = lineEndSec(line, lines[lineIndex + 1]);
    const dur = Math.max(end - start, 0.001);
    return Math.min(1, Math.max(0, (time - start) / dur));
  }

  function setWordFill(el: HTMLElement, fill: number) {
    el.style.setProperty('--word-fill', String(Math.min(1, Math.max(0, fill))));
  }

  function stopFillLoop() {
    if (fillRaf) {
      cancelAnimationFrame(fillRaf);
      fillRaf = 0;
    }
  }

  function clearWordFill() {
    stopFillLoop();
    if (fillWordEl) {
      // Leave at current visual state — snapping to 1 causes a white flash
      fillWordEl = null;
    }
    fillWordId = '';
    smoothWordFill = 0;
  }

  function paintInstrumentalFill() {
    if (!viewportEl || lines.length === 0 || syncType === 'none') return;

    const t = mediaNow();
    const bars = viewportEl.querySelectorAll<HTMLElement>('.fs-lyrics-instrumental');
    for (const el of bars) {
      const lineEl = el.closest<HTMLElement>('[data-line]');
      if (!lineEl) continue;
      const i = Number(lineEl.dataset.line);
      const line = lines[i];
      if (!line?.isInstrumental) continue;
      el.style.setProperty(
        '--instrumental-fill',
        String(instrumentalProgress(line, i, t)),
      );
    }
  }

  /** Soft media clock: avoid hard resync every position tick (causes fill jitter). */
  function mediaNow(): number {
    if (!clockPlaying) return clockPos;
    return clockPos + (performance.now() - clockWall) / 1000;
  }

  /** Ease-in so word highlight doesn't pop at t=0. */
  function easeInFill(p: number): number {
    const x = Math.min(1, Math.max(0, p));
    // smootherstep-ish: slow start, natural finish
    return x * x * (3 - 2 * x);
  }

  let smoothWordFill = 0;

  function findActiveWordTarget(mediaTime: number): {
    id: string;
    el: HTMLElement;
    startSec: number;
    durationSec: number;
  } | null {
    if (!viewportEl || syncType !== 'richsync' || lines.length === 0) return null;

    const lineIndex = findActiveLineIndex(lines, mediaTime);
    if (lineIndex < 0) return null;

    const line = lines[lineIndex];
    if (!line || line.isInstrumental) return null;

    const parts = displayParts(line);
    for (let partIndex = 0; partIndex < parts.length; partIndex++) {
      const part = parts[partIndex];
      if (!part.words) continue;

      const active = isPartActive(part, partIndex, parts, line, lineIndex, lines, mediaTime);
      const sung = isPartSung(part, partIndex, parts, line, lineIndex, lines, mediaTime);
      if (!active || sung) continue;

      const id = wordId(lineIndex, partIndex, part);
      const el = viewportEl.querySelector<HTMLElement>(`[data-word="${id}"]`);
      if (!el) return null;

      return {
        id,
        el,
        startSec: partStartSec(part),
        durationSec: partDurationSec(part, partIndex, parts, line, lineIndex, lines),
      };
    }

    return null;
  }

  function paintWordFill() {
    if (syncType !== 'richsync' || !viewportEl) return;

    const t = mediaNow();
    const target = findActiveWordTarget(t);

    if (!target) {
      // Finish current word gently instead of a hard flash to full white
      if (fillWordEl) {
        smoothWordFill = Math.min(1, smoothWordFill + 0.12);
        setWordFill(fillWordEl, smoothWordFill);
        if (smoothWordFill >= 0.999) {
          fillWordEl = null;
          fillWordId = '';
          smoothWordFill = 0;
        }
      }
      return;
    }

    if (fillWordId !== target.id) {
      // Previous word: settle without snap
      if (fillWordEl && fillWordEl !== target.el) {
        setWordFill(fillWordEl, 1);
      }
      fillWordId = target.id;
      fillWordEl = target.el;
      smoothWordFill = 0;
    }

    fillWordEl = target.el;

    const safeDur = Math.max(target.durationSec, 0.08);
    // Small lead so first glyph doesn't jump from dim→lit in one frame
    const raw = Math.min(Math.max((t - target.startSec) / safeDur, 0), 1);
    const eased = easeInFill(raw);
    // Light EMA — kills micro-jitter from clock resync without lagging behind
    smoothWordFill = smoothWordFill + (eased - smoothWordFill) * 0.6;
    if (raw >= 0.999) smoothWordFill = 1;
    setWordFill(target.el, smoothWordFill);
  }

  $effect(() => {
    if (!viewportEl) return;
    void containerEl;
    void edgeTopEl;
    void edgeBottomEl;

    updateEdgeSpacers();
    scheduleLayoutRecompute();

    // Geist may load after first paint — remeasure once metrics are final
    let fontsAlive = true;
    document.fonts?.ready?.then(() => {
      if (fontsAlive) scheduleLayoutRecompute();
    });

    const observer = new ResizeObserver(() => {
      updateEdgeSpacers();
      scheduleLayoutRecompute();
      scrollActiveLine(false);
    });
    observer.observe(viewportEl);
    if (containerEl) observer.observe(containerEl);

    return () => {
      fontsAlive = false;
      observer.disconnect();
      if (layoutRaf) {
        cancelAnimationFrame(layoutRaf);
        layoutRaf = 0;
      }
      cancelScrollAnim();
      clearWordFill();
    };
  });

  $effect(() => {
    // lines / syncType change → rebuild token wraps after DOM settles
    void lines;
    void syncType;
    scheduleLayoutRecompute();
  });

  // chromeVisible kept for API stability
  $effect(() => {
    void chromeVisible;
  });

  $effect(() => {
    if (!viewportEl || !containerEl || lines.length === 0 || syncType === 'none') return;
    if (activeLineIndex < 0) return;
    if (activeLineIndex === lastScrolledIndex) return;

    lastScrolledIndex = activeLineIndex;
    requestAnimationFrame(() => scrollActiveLine(isPlaying));
  });

  $effect(() => {
    lines;
    lastScrolledIndex = -1;
    clearWordFill();
    if (viewportEl) viewportEl.scrollTop = 0;
    scrollActiveLineAfterLayout(false);
  });

  $effect(() => {
    const next = currentTime;
    const now = performance.now();
    const extrapolated = clockPlaying ? clockPos + (now - clockWall) / 1000 : clockPos;
    const drift = next - extrapolated;

    // Only hard-snap on seek / big desync; soft-correct otherwise (stops text jitter)
    if (!clockPlaying || Math.abs(drift) > 0.12) {
      clockPos = next;
      clockWall = now;
    } else {
      clockPos = extrapolated + drift * 0.35;
      clockWall = now;
    }
    clockPlaying = isPlaying;
  });

  $effect(() => {
    if (!viewportEl || lines.length === 0 || syncType === 'none') {
      clearWordFill();
      return;
    }

    void lines;
    void syncType;

    const tick = () => {
      if (syncType === 'richsync') paintWordFill();
      paintInstrumentalFill();
      fillRaf = requestAnimationFrame(tick);
    };

    fillRaf = requestAnimationFrame(tick);

    return () => {
      stopFillLoop();
    };
  });
</script>

{#if lines.length > 0}
  <div class="fs-lyrics-panel">
    <div
      class="fs-lyrics-viewport"
      bind:this={viewportEl}
    >
      <div class="fs-lyrics-edge" aria-hidden="true" bind:this={edgeTopEl}></div>
      <div
        class="fs-lyrics-container"
        data-sync={syncType}
        bind:this={containerEl}
      >
        {#each lines as line, lineIndex (line.startTimeMs + ':' + lineIndex)}
          {@const lineActive = lineIndex === activeLineIndex}
          {@const linePast = lineIndex < activeLineIndex}
          {@const parts = displayParts(line)}
          <div
            class="fs-lyrics-line"
            class:is-active={lineActive}
            class:is-past={linePast}
            data-line={lineIndex}
            data-agent={line.agent}
            onclick={() => seekToLine(line)}
            onkeydown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                seekToLine(line);
              }
            }}
            role="button"
            tabindex="0"
          >
            {#if line.isInstrumental}
              <span class="fs-lyrics-instrumental" aria-label="Instrumental">
                <span class="fs-lyrics-instrumental-track">
                  <span class="fs-lyrics-instrumental-fill"></span>
                </span>
              </span>
            {:else if line.parts && line.parts.length > 0 && syncType === 'richsync'}
              <!--
                Keep richsync markup whitespace-free between words/glyphs.
                Spaces already live as trailing chars on the previous part;
                extra HTML text nodes between inline/inline-flex boxes double gaps.
              -->
              {#each parts as part, partIndex (part.startTimeMs + ':' + partIndex)}{#if part.words}{@const partActive = isPartActive(part, partIndex, parts, line, lineIndex, lines, currentTime)}{@const partSung = isPartSung(part, partIndex, parts, line, lineIndex, lines, currentTime)}{@const partAnimating = lineActive && partActive && !partSung}{@const partUpcoming = lineActive && !partSung && !partAnimating}{#if richPartRowBreak(lineIndex, partIndex)}<br class="fs-lyrics-soft-break" aria-hidden="true" />{/if}<span class="fs-lyrics-word-wrap"><span
                      class="fs-lyrics-word"
                      class:is-background={part.isBackground}
                      class:is-sung={partSung}
                      class:is-animating={partAnimating}
                      class:is-upcoming={partUpcoming}
                      data-word={wordId(lineIndex, partIndex, part)}
                      onclick={(e) => {
                        e.stopPropagation();
                        seekToPart(part);
                      }}
                      role="presentation"
                    >{#each Array.from(part.words) as ch, charIndex}<span
                        class="fs-lyrics-char"
                        class:fs-lyrics-char-space={ch === ' ' || ch === '\t'}
                        style:--char-i={charIndex}
                        style:--char-n={part.words.length}
                      >{ch}</span>{/each}</span></span>{/if}{/each}
            {:else}
              <span class="fs-lyrics-word-wrap">
                <span class="fs-lyrics-word">{#each plainRows(lineIndex, line.words) as row, rowIndex}{#if rowIndex > 0}<br class="fs-lyrics-soft-break" aria-hidden="true" />{/if}{row}{/each}</span>
              </span>
            {/if}
          </div>
        {/each}
      </div>
      <div class="fs-lyrics-edge" aria-hidden="true" bind:this={edgeBottomEl}></div>
    </div>
  </div>
{/if}

<style>
  @import './FullscreenLyrics.css';
</style>
