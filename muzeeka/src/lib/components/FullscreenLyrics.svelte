<script lang="ts">
  import type { LyricLine, LyricPart, SyncType } from '$lib/lyrics/types';
  import {
    animationDelay,
    findActiveLineIndex,
    highlightAmounts,
    isLineActive,
    isLinePast,
    isPartActive,
    isPartSung,
    lineHighlightProgress,
    lineStartSec,
    partDurationSec,
    partHighlightProgress,
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

  /** Lift when bottom chrome is visible — keep subtle; layout already has bottom inset */
  const CHROME_RESERVE_PX = 44;
  const LIFT_SHOW_MS = 1100;
  const LIFT_HIDE_MS = 950;
  const LIFT_SHOW_DELAY_MS = 140;
  const LIFT_EASING = 'cubic-bezier(0.16, 1, 0.3, 1)';

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
  let containerEl = $state<HTMLDivElement | undefined>();
  let lastScrolledIndex = -1;
  let centerYOffset = $state(0);
  let liftAnimation: Animation | null = null;

  let activeLineIndex = $derived(findActiveLineIndex(lines, currentTime));

  function chromeLiftPx(): number {
    return CHROME_RESERVE_PX / 2;
  }

  function readTranslateY(el: HTMLElement): number {
    const transform = getComputedStyle(el).transform;
    if (transform === 'none') return 0;
    return new DOMMatrix(transform).m42;
  }

  function setContainerLift(px: number) {
    if (!containerEl) return;
    containerEl.style.transform = px === 0 ? '' : `translateY(${-px}px)`;
  }

  function animateChromeLift(visible: boolean, instant = false) {
    if (!containerEl) return;

    liftAnimation?.cancel();
    liftAnimation = null;

    const targetLift = visible ? chromeLiftPx() : 0;
    const currentLift = Math.max(0, -readTranslateY(containerEl));

    if (instant || currentLift === targetLift) {
      setContainerLift(targetLift);
      return;
    }

    liftAnimation = containerEl.animate(
      [
        { transform: `translateY(${-currentLift}px)` },
        { transform: `translateY(${-targetLift}px)` },
      ],
      {
        duration: visible ? LIFT_SHOW_MS : LIFT_HIDE_MS,
        delay: visible ? LIFT_SHOW_DELAY_MS : 0,
        easing: LIFT_EASING,
        fill: 'forwards',
      },
    );

    liftAnimation.onfinish = () => {
      setContainerLift(targetLift);
      liftAnimation = null;
    };
    liftAnimation.oncancel = () => {
      liftAnimation = null;
    };
  }

  function updateViewportMetrics() {
    if (!viewportEl) return;

    const height = viewportEl.clientHeight;
    if (height <= 0) return;

    const pad = height / 2;
    centerYOffset = chromeVisible
      ? (height - CHROME_RESERVE_PX) / 2
      : pad;

    viewportEl.style.setProperty('--lyrics-pad-block', `${pad}px`);
  }

  function scrollLineToCenter(lineEl: HTMLElement, smooth: boolean) {
    if (!viewportEl) return;

    const lineCenter = lineEl.offsetTop + lineEl.offsetHeight / 2;
    const targetScroll = lineCenter - centerYOffset;

    viewportEl.scrollTo({
      top: Math.max(0, targetScroll),
      behavior: smooth ? 'smooth' : 'instant',
    });
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

  let prevChromeVisible: boolean | undefined = undefined;
  let boundContainerEl: HTMLDivElement | undefined;

  $effect(() => {
    if (!viewportEl) return;

    updateViewportMetrics();

    const observer = new ResizeObserver(() => {
      updateViewportMetrics();
      if (containerEl) {
        animateChromeLift(chromeVisible, true);
      }
      scrollActiveLine(false);
    });
    observer.observe(viewportEl);

    return () => observer.disconnect();
  });

  $effect(() => {
    const visible = chromeVisible;
    if (!viewportEl) return;
    updateViewportMetrics();
    if (!containerEl) {
      boundContainerEl = undefined;
      return;
    }

    const containerChanged = containerEl !== boundContainerEl;
    boundContainerEl = containerEl;

    const chromeChanged = prevChromeVisible !== undefined && prevChromeVisible !== visible;
    animateChromeLift(visible, containerChanged || !chromeChanged);
    prevChromeVisible = visible;
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
      <div class="fs-lyrics-container" data-sync={syncType} bind:this={containerEl}>
        {#each lines as line, lineIndex (line.startTimeMs + ':' + lineIndex)}
          {@const lineActive = isLineActive(line, lineIndex, lines, currentTime)}
          {@const linePast = isLinePast(line, lineIndex, lines, currentTime)}
          {@const lineSyncedReveal = syncType === 'synced' && (linePast || lineActive)}
          {@const lineRichAnimating = lineActive && syncType === 'richsync'}
          {@const lineAnimating = lineActive && syncType === 'richsync'}
          {@const parts = displayParts(line)}

          <div
            class="fs-lyrics-line"
            class:is-active={lineIndex === activeLineIndex}
            class:is-past={linePast}
            class:is-animating={lineAnimating}
            class:is-paused={lineAnimating && !isPlaying}
            data-line={lineIndex}
            data-agent={line.agent}
            style:--blyrics-anim-delay={lineAnimating ? animationDelay(currentTime, lineStartSec(line)) : undefined}
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
            {:else if line.parts && line.parts.length > 0}
              {#each parts as part, partIndex (part.startTimeMs + ':' + partIndex)}
                {#if part.words}
                  {@const partActive = isPartActive(part, partIndex, parts, line, lineIndex, lines, currentTime)}
                  {@const partSung = lineSyncedReveal
                    || isPartSung(part, partIndex, parts, line, lineIndex, lines, currentTime)}
                  {@const partAnimating = partActive && syncType === 'richsync'}
                  {@const partFill = partSung || partAnimating
                    ? highlightAmounts(
                      partAnimating
                        ? partHighlightProgress(part, partIndex, parts, line, lineIndex, lines, currentTime)
                        : 1,
                    )
                    : null}
                  {@const partDurationMs = Math.round(
                    partDurationSec(part, partIndex, parts, line, lineIndex, lines) * 1000,
                  )}
                  <span class="fs-lyrics-word-wrap" class:is-sung={partSung}>
                    <span
                      class="fs-lyrics-word"
                      class:is-background={part.isBackground}
                      class:is-sung={partSung}
                      class:is-pre-animating={partAnimating && !partSung && !partActive}
                      class:is-animating={partAnimating}
                      class:is-paused={partAnimating && !isPlaying}
                      data-content={part.words}
                      style:--blyrics-duration="{Math.max(partDurationMs, 300)}ms"
                      style:--lyric-highlight-start={partFill?.start}
                      style:--lyric-highlight-end={partFill?.end}
                      style:--blyrics-anim-delay={partAnimating
                        ? animationDelay(currentTime, part.startTimeMs / 1000)
                        : undefined}
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
              {@const lineFill = lineSyncedReveal
                ? highlightAmounts(1)
                : lineRichAnimating
                  ? highlightAmounts(lineHighlightProgress(line, lineIndex, lines, currentTime))
                  : null}
              <span
                class="fs-lyrics-word-wrap"
                class:is-sung={lineSyncedReveal}
              >
                <span
                  class="fs-lyrics-word"
                  class:is-sung={lineSyncedReveal}
                  class:is-pre-animating={lineRichAnimating && !linePast}
                  class:is-animating={lineRichAnimating}
                  class:is-paused={lineRichAnimating && !isPlaying}
                  data-content={line.words}
                  style:--blyrics-duration="{Math.max(line.durationMs, 800)}ms"
                  style:--lyric-highlight-start={lineFill?.start}
                  style:--lyric-highlight-end={lineFill?.end}
                  style:--blyrics-anim-delay={lineRichAnimating
                    ? animationDelay(currentTime, lineStartSec(line))
                    : undefined}
                >{line.words}</span>
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