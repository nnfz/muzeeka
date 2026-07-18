<script lang="ts">
  import type { LyricLine, LyricPart, SyncType } from '$lib/lyrics/types';
  import {
    findActiveLineIndex,
    isLineActive,
    isLinePast,
    isPartActive,
    isPartSung,
    lineStartSec,
  } from '$lib/lyrics/sync';

  interface Props {
    lines?: LyricLine[];
    syncType?: SyncType;
    currentTime?: number;
    isPlaying?: boolean;
    loading?: boolean;
    error?: string | null;
    chromeVisible?: boolean;
    onSeek?: (timeSec: number) => void;
  }

  /** Apple-like scroll easing: soft ease-out, interruptible */
  const SCROLL_MIN_MS = 420;
  const SCROLL_MAX_MS = 820;
  const SCROLL_PX_FACTOR = 0.65;

  let {
    lines = [],
    syncType = 'none',
    currentTime = 0,
    isPlaying = false,
    loading = false,
    error = null,
    chromeVisible = true,
    onSeek,
  }: Props = $props();

  let viewportEl = $state<HTMLDivElement | undefined>();
  let lastScrolledIndex = -1;
  let centerYOffset = $state(0);
  let scrollRaf = 0;
  let scrollToken = 0;

  let activeLineIndex = $derived(findActiveLineIndex(lines, currentTime));

  function updateViewportMetrics() {
    if (!viewportEl) return;

    const height = viewportEl.clientHeight;
    if (height <= 0) return;

    // Keep active line optically centered; chrome fades over the bottom mask.
    // No vertical "lift" transform — that was fighting scroll and felt jerky.
    const pad = height / 2;
    centerYOffset = pad;
    viewportEl.style.setProperty('--lyrics-pad-block', `${pad}px`);
  }

  function easeOutCubic(t: number): number {
    return 1 - (1 - t) ** 3;
  }

  function cancelScrollAnim() {
    if (scrollRaf) {
      cancelAnimationFrame(scrollRaf);
      scrollRaf = 0;
    }
    scrollToken += 1;
  }

  function scrollLineToCenter(lineEl: HTMLElement, smooth: boolean) {
    if (!viewportEl) return;

    const lineCenter = lineEl.offsetTop + lineEl.offsetHeight / 2;
    const targetScroll = Math.max(0, lineCenter - centerYOffset);
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
      viewportEl.scrollTop = startScroll + delta * easeOutCubic(t);

      if (t < 1) {
        scrollRaf = requestAnimationFrame(tick);
      } else {
        scrollRaf = 0;
      }
    };

    scrollRaf = requestAnimationFrame(tick);
  }

  function scrollActiveLine(smooth: boolean) {
    if (!viewportEl || lines.length === 0 || syncType === 'none') return;
    if (activeLineIndex < 0) return;

    const lineEl = viewportEl.querySelector<HTMLElement>(`[data-line="${activeLineIndex}"]`);
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
      return line.parts;
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

  $effect(() => {
    if (!viewportEl) return;

    updateViewportMetrics();

    const observer = new ResizeObserver(() => {
      updateViewportMetrics();
      scrollActiveLine(false);
    });
    observer.observe(viewportEl);

    return () => {
      observer.disconnect();
      cancelScrollAnim();
    };
  });

  // chromeVisible kept in props for API stability; no layout lift.
  $effect(() => {
    void chromeVisible;
    if (!viewportEl) return;
    updateViewportMetrics();
  });

  $effect(() => {
    if (!viewportEl || lines.length === 0 || syncType === 'none') return;
    if (activeLineIndex < 0) return;
    if (activeLineIndex === lastScrolledIndex) return;

    lastScrolledIndex = activeLineIndex;
    requestAnimationFrame(() => scrollActiveLine(isPlaying));
  });

  $effect(() => {
    lines;
    lastScrolledIndex = -1;
    updateViewportMetrics();
    scrollActiveLineAfterLayout(false);
  });
</script>

<div class="fs-lyrics-panel">
  {#if loading}
    <div class="fs-lyrics-status">Loading lyrics…</div>
  {:else if error}
    <div class="fs-lyrics-status">{error}</div>
  {:else if lines.length === 0}
    <div class="fs-lyrics-status">No synced lyrics for this track</div>
  {:else}
    <div
      class="fs-lyrics-viewport"
      bind:this={viewportEl}
    >
      <div class="fs-lyrics-container" data-sync={syncType}>
        {#each lines as line, lineIndex (line.startTimeMs + ':' + lineIndex)}
          {@const lineActive = isLineActive(line, lineIndex, lines, currentTime)}
          {@const linePast = isLinePast(line, lineIndex, lines, currentTime)}
          {@const parts = displayParts(line)}

          <div
            class="fs-lyrics-line"
            class:is-active={lineIndex === activeLineIndex}
            class:is-past={linePast && lineIndex !== activeLineIndex}
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
                  {@const partSung = isPartSung(part, partIndex, parts, line, lineIndex, lines, currentTime)
                    || (linePast && !lineActive)}
                  {@const partAnimating = partActive && !partSung}
                  <span class="fs-lyrics-word-wrap" class:is-sung={partSung || partAnimating}>
                    <span
                      class="fs-lyrics-word"
                      class:is-background={part.isBackground}
                      class:is-sung={partSung}
                      class:is-animating={partAnimating}
                      onclick={(e) => {
                        e.stopPropagation();
                        seekToPart(part);
                      }}
                      role="presentation"
                    >{part.words}</span>
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
    </div>
  {/if}
</div>

<style>
  @import './FullscreenLyrics.css';
</style>
