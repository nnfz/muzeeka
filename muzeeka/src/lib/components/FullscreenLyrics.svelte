<script lang="ts">
  import type { LyricLine, LyricPart, SyncType } from '$lib/lyrics/types';
  import { normalizePartSpaces } from '$lib/lyrics/normalizeParts';
  import {
    findActiveLineIndex,
    findDisplayActiveLineIndex,
    isPartActive,
    isPartSung,
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
  let lastScrolledIndex = -1;
  let scrollRaf = 0;
  let scrollToken = 0;

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

  function seekToLine(line: LyricLine) {
    onSeek?.(lineStartSec(line));
  }

  function seekToPart(part: LyricPart) {
    onSeek?.(part.startTimeMs / 1000);
  }

  function wordId(lineIndex: number, partIndex: number, part: LyricPart): string {
    return `${lineIndex}:${partIndex}:${part.startTimeMs}`;
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
    smoothWordFill = smoothWordFill + (eased - smoothWordFill) * 0.45;
    if (raw >= 0.999) smoothWordFill = 1;
    setWordFill(target.el, smoothWordFill);
  }

  $effect(() => {
    if (!viewportEl) return;
    void containerEl;
    void edgeTopEl;
    void edgeBottomEl;

    updateEdgeSpacers();

    const observer = new ResizeObserver(() => {
      updateEdgeSpacers();
      scrollActiveLine(false);
    });
    observer.observe(viewportEl);
    if (containerEl) observer.observe(containerEl);

    return () => {
      observer.disconnect();
      cancelScrollAnim();
      clearWordFill();
    };
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
    if (syncType !== 'richsync' || !viewportEl) {
      clearWordFill();
      return;
    }

    void lines;

    const tick = () => {
      paintWordFill();
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
                <span></span><span></span><span></span>
              </span>
            {:else if line.parts && line.parts.length > 0 && syncType === 'richsync'}
              {#each parts as part, partIndex (part.startTimeMs + ':' + partIndex)}
                {#if part.words}
                  {@const partActive = isPartActive(part, partIndex, parts, line, lineIndex, lines, currentTime)}
                  {@const partSung = isPartSung(part, partIndex, parts, line, lineIndex, lines, currentTime)}
                  {@const partAnimating = lineActive && partActive && !partSung}
                  {@const partUpcoming = lineActive && !partSung && !partAnimating}
                  <span class="fs-lyrics-word-wrap">
                    <span
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
                    >{#each Array.from(part.words) as ch, charIndex}
                      <span
                        class="fs-lyrics-char"
                        class:fs-lyrics-char-space={ch === ' ' || ch === '\t'}
                        style:--char-i={charIndex}
                        style:--char-n={part.words.length}
                      >{ch}</span>
                    {/each}</span>
                  </span>
                {/if}
              {/each}
            {:else}
              <span class="fs-lyrics-word-wrap">
                <span class="fs-lyrics-word">{line.words}</span>
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
