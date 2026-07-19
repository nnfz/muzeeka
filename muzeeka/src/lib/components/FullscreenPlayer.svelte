<script lang="ts">
  import { getCoverSrc, resolveCoverSrc } from '$lib/coverCache';
  import { COVER_PLACEHOLDER_SRC } from '$lib/coverPlaceholder';
  import {
    getPlayerStore,
    trackDisplayArtist,
    trackDisplayTitle,
  } from '$lib/stores/player.svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { fetchLyrics } from '$lib/lyrics/fetchLyrics';
  import type { LyricsResult } from '$lib/lyrics/types';
  import FullscreenLyrics from './FullscreenLyrics.svelte';
  import KawarpBackground from './KawarpBackground.svelte';
  import MediaSlider from './MediaSlider.svelte';
  import { untrack } from 'svelte';

  interface Props {
    open?: boolean;
  }

  let { open = $bindable(false) }: Props = $props();

  const player = getPlayerStore();

  let resolvedFullCoverPath = $state<string | null>(null);

  let coverPath = $derived(
    resolvedFullCoverPath
      ?? player.currentTrack?.cover_path
      ?? player.currentTrack?.cover_path_full
      ?? null
  );
  /** Kawarp background URL (may upgrade to full-res independently). */
  let bgCoverSrc = $state<string | null>(null);

  /**
   * Cover art: base stays fully opaque; overlay fades in on top, then promotes.
   * Old cover never fades out — that was the dissolve bug.
   */
  const ART_CROSSFADE_MS = 480;
  let artBaseSrc = $state<string | null>(null);
  let artOverlaySrc = $state<string | null>(null);
  let artOverlayIn = $state(false);
  /** Audio file the current art belongs to (thumb→full skips crossfade). */
  let artFile = $state<string | null>(null);
  let artToken = 0;
  let artPromoteTimer: ReturnType<typeof setTimeout> | null = null;
  let placeholderFailed = $state(false);

  function clearArtPromoteTimer() {
    if (artPromoteTimer) {
      clearTimeout(artPromoteTimer);
      artPromoteTimer = null;
    }
  }

  function preloadImageOk(url: string): Promise<boolean> {
    return new Promise((resolve) => {
      const img = new Image();
      img.onload = () => resolve(true);
      img.onerror = () => resolve(false);
      img.src = url;
    });
  }

  function clearArt() {
    clearArtPromoteTimer();
    artToken += 1;
    artBaseSrc = null;
    artOverlaySrc = null;
    artOverlayIn = false;
    artFile = null;
  }

  /** Instant base cover — used on open. No animation, no async. */
  function setArtImmediate(src: string, file: string | null) {
    artToken += 1;
    clearArtPromoteTimer();
    artBaseSrc = src;
    artOverlaySrc = null;
    artOverlayIn = false;
    artFile = file;
  }

  /**
   * Show / swap front cover.
   * - Open / first image: hard set base (NO animation).
   * - Same track URL upgrade: replace base pixels in place.
   * - Track change only: preload, fade overlay on top (base never fades out).
   */
  async function setArtSrc(next: string | null, file: string | null) {
    if (!next) {
      clearArt();
      return;
    }

    const settled =
      artOverlayIn && artOverlaySrc ? artOverlaySrc : artBaseSrc;

    if (settled === next || artOverlaySrc === next) {
      artFile = file;
      return;
    }

    // Same track, higher-res / other URL: swap pixels, no fade.
    if (file && artFile === file && artBaseSrc) {
      setArtImmediate(next, file);
      return;
    }

    // First cover (open): solid, no animation.
    if (!artBaseSrc) {
      setArtImmediate(next, file);
      return;
    }

    // Track change only: overlay fades in on top of solid base.
    const token = ++artToken;
    const ok = await preloadImageOk(next);
    if (token !== artToken || !ok) return;
    if (untrack(() => !open)) return;

    clearArtPromoteTimer();
    artOverlaySrc = next;
    artOverlayIn = false;
    artFile = file;

    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        if (token !== artToken) return;
        artOverlayIn = true;
        artPromoteTimer = setTimeout(() => {
          if (token !== artToken) return;
          artBaseSrc = next;
          artOverlaySrc = null;
          artOverlayIn = false;
          artPromoteTimer = null;
        }, ART_CROSSFADE_MS);
      });
    });
  }

  let lyricsState = $state<LyricsResult | null>(null);
  /** File path for which fetch finished (hit or miss). Not set while in-flight. */
  let lyricsSettledForFile = $state<string | null>(null);
  let lyricsVisible = $state(true);
  /** After open settle — transitions + Kawarp allowed. */
  let enterDone = $state(false);
  /**
   * Drives cover left + lyrics visible. Set one frame AFTER enterDone so the browser
   * paints the centered state with transitions enabled, then animates to lyrics layout.
   * Same-frame enterDone+show skipped CSS transitions entirely (looked like "nothing changes").
   */
  let lyricsLayoutActive = $state(false);
  let hasLyrics = $derived((lyricsState?.lines.length ?? 0) > 0);
  let showLyricsPanel = $derived(lyricsLayoutActive);
  const CHROME_HIDE_DELAY = 1800;

  let chromeVisible = $state(true);
  /** Reactive: drives class so chrome cannot hide under the cursor. */
  let pointerOverChrome = $state(false);
  let chromeEl = $state<HTMLDivElement | null>(null);
  let hideTimer: ReturnType<typeof setTimeout> | null = null;
  let pointerX = 0;
  let pointerY = 0;
  /** Don't auto-hide until we know where the cursor is (open under cursor has no :hover). */
  let sawPointerMove = false;

  function clearHideTimer() {
    if (hideTimer) {
      clearTimeout(hideTimer);
      hideTimer = null;
    }
  }

  /** Prefer :hover; fall back to last pointer coords (mouseenter is unreliable). */
  function computePointerOverChrome(): boolean {
    if (!chromeEl) return false;
    try {
      if (chromeEl.matches(':hover')) return true;
    } catch {
      /* ignore */
    }
    const r = chromeEl.getBoundingClientRect();
    // Small pad so buttons/volume near the top edge of the strip still count.
    const pad = 8;
    return (
      pointerX >= r.left - pad &&
      pointerX <= r.right + pad &&
      pointerY >= r.top - pad &&
      pointerY <= r.bottom + pad
    );
  }

  function scheduleChromeHide() {
    clearHideTimer();
    if (!sawPointerMove || pointerOverChrome || computePointerOverChrome()) return;
    hideTimer = setTimeout(() => {
      hideTimer = null;
      const over = computePointerOverChrome();
      pointerOverChrome = over;
      if (over) return;
      chromeVisible = false;
    }, CHROME_HIDE_DELAY);
  }

  function showChrome() {
    chromeVisible = true;
    scheduleChromeHide();
  }

  function onPlayerPointerMove(e: PointerEvent) {
    sawPointerMove = true;
    pointerX = e.clientX;
    pointerY = e.clientY;
    const over = computePointerOverChrome();
    pointerOverChrome = over;
    chromeVisible = true;
    if (over) {
      clearHideTimer();
    } else {
      scheduleChromeHide();
    }
  }

  function close() {
    open = false;
  }

  function toggleLyrics() {
    lyricsVisible = !lyricsVisible;
    showChrome();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (!open) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
    }
  }

  $effect(() => {
    if (!open) return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      document.body.style.overflow = prev;
    };
  });

  $effect(() => {
    if (open && !player.hasTrack) {
      open = false;
    }
  });

  $effect(() => {
    if (!open) {
      clearHideTimer();
      clearArtPromoteTimer();
      chromeVisible = true;
      pointerOverChrome = false;
      sawPointerMove = false;
      lyricsVisible = true;
      enterDone = false;
      lyricsLayoutActive = false;
      clearArt();
      bgCoverSrc = null;
      resolvedFullCoverPath = null;
      return;
    }

    chromeVisible = true;
    pointerOverChrome = false;
    sawPointerMove = false;
    scheduleChromeHide();
    enterDone = false;
    lyricsLayoutActive = false;

    // Seed once on open (untracked) — solid cover on first paint, zero animation.
    untrack(() => {
      const file = player.currentFile;
      const path =
        player.currentTrack?.cover_path
        ?? player.currentTrack?.cover_path_full
        ?? null;
      if (file && path) {
        const src = getCoverSrc(path);
        if (src) {
          setArtImmediate(src, file);
          bgCoverSrc = src;
          return;
        }
      }
      // No cover — still feed placeholder into Kawarp
      bgCoverSrc = COVER_PLACEHOLDER_SRC;
    });

    const enterTimer = setTimeout(() => {
      enterDone = true;
    }, 200);

    return () => {
      clearHideTimer();
      clearTimeout(enterTimer);
    };
  });

  // Two rAFs after we can show lyrics: paint "centered + transitions on", then flip layout.
  $effect(() => {
    if (!open || !enterDone || !hasLyrics || !lyricsVisible) {
      lyricsLayoutActive = false;
      return;
    }

    let cancelled = false;
    let raf1 = 0;
    let raf2 = 0;
    raf1 = requestAnimationFrame(() => {
      raf2 = requestAnimationFrame(() => {
        if (!cancelled) lyricsLayoutActive = true;
      });
    });

    return () => {
      cancelled = true;
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
    };
  });

  // File change: reset lyrics + resolve full cover (do not hard-clear art — crossfade handles swap).
  $effect(() => {
    const file = player.currentFile;

    if (!open || !file) {
      return;
    }

    resolvedFullCoverPath = null;
    lyricsState = null;
    lyricsSettledForFile = null;
    lyricsLayoutActive = false;

    let cancelled = false;

    void invoke<string | null>('library_resolve_full_cover', { path: file })
      .then((fullPath) => {
        if (cancelled || !fullPath) return;
        if (untrack(() => player.currentFile) !== file) return;
        resolvedFullCoverPath = fullPath;
      })
      .catch(() => {});

    return () => {
      cancelled = true;
    };
  });

  /**
   * Sync art + bg from cover path.
   * No path → placeholder (clear art). Never blank the base mid-load if path only upgrades.
   */
  $effect(() => {
    if (!open) return;

    const file = player.currentFile;
    void player.currentTrack;
    const path = coverPath;

    if (!file) return;

    if (!path) {
      // No cover — clear front art (placeholder UI) but keep Kawarp on placeholder image.
      bgCoverSrc = COVER_PLACEHOLDER_SRC;
      void setArtSrc(null, file);
      return;
    }

    let cancelled = false;

    const apply = (src: string) => {
      if (cancelled) return;
      if (untrack(() => player.currentFile) !== file) return;
      bgCoverSrc = src;
      void setArtSrc(src, file);
    };

    const immediate = getCoverSrc(path);
    if (immediate) apply(immediate);

    void resolveCoverSrc(path).then((src) => {
      if (src) apply(src);
    });

    return () => {
      cancelled = true;
    };
  });

  // After manual TTML import — drop settled flag so lyrics re-fetch from cache.
  $effect(() => {
    let unlisten: (() => void) | undefined;
    void listen<string>('lyrics:imported', (event) => {
      const importedPath = event.payload?.trim() || '';
      const current = untrack(() => player.currentFile);
      if (!importedPath || !current || importedPath === current) {
        lyricsSettledForFile = null;
        lyricsState = null;
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  });

  // Silent background fetch — panel appears only when lines exist.
  // Depend ONLY on open + file. Reading track/duration/settled reactively was
  // re-running this effect mid-flight, cancelling the request, and leaving no lyrics.
  $effect(() => {
    const file = player.currentFile;
    const isOpen = open;
    // Re-run when settled flag is cleared after import.
    void lyricsSettledForFile;

    if (!isOpen || !file) {
      lyricsState = null;
      lyricsSettledForFile = null;
      return;
    }

    if (untrack(() => lyricsSettledForFile === file)) {
      return;
    }

    let alive = true;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;

    const finish = (result: LyricsResult | null) => {
      if (!alive) return;
      if (untrack(() => player.currentFile) !== file) return;
      lyricsState = result;
      lyricsSettledForFile = file;
    };

    const run = () => {
      if (!alive) return;

      const params = untrack(() => {
        const track = player.currentTrack;
        if (!track) return null;
        return {
          title: trackDisplayTitle(track),
          artist: trackDisplayArtist(track),
          album: track.album,
          durationSecs:
            track.duration_secs
            ?? (player.duration > 0 ? player.duration : null),
        };
      });

      // Track metadata not in the map yet — retry without cancelling a parent fetch.
      if (!params) {
        retryTimer = setTimeout(run, 80);
        return;
      }

      void fetchLyrics(params)
        .then((result) => finish(result))
        .catch((error: unknown) => {
          console.warn('[lyrics] fetch failed', error);
          finish(null);
        });
    };

    run();

    return () => {
      alive = false;
      if (retryTimer) clearTimeout(retryTimer);
    };
  });
</script>

<svelte:window
  onkeydown={handleKeydown}
  onpointermove={open ? onPlayerPointerMove : undefined}
/>

{#if open && player.hasTrack}
  <div
    class="fullscreen-player"
    class:enter-done={enterDone}
    role="dialog"
    aria-modal="true"
    aria-label="Now playing"
  >
    <!-- Persistent Kawarp (no #key) so texture crossfade works between tracks -->
    <div class="fullscreen-backdrop" aria-hidden="true">
      <KawarpBackground src={bgCoverSrc} active={open} transitionDuration={700} />
      <div class="fullscreen-backdrop-shade"></div>
    </div>

    <div class="fullscreen-layout" class:lyrics-hidden={!showLyricsPanel}>
      <aside class="fullscreen-side">
        <div class="fullscreen-art-wrap">
          {#if artBaseSrc}
            <img
              class="fullscreen-art art-base"
              src={artBaseSrc}
              alt=""
              draggable="false"
              decoding="async"
            />
            {#if artOverlaySrc}
              <img
                class="fullscreen-art art-overlay"
                class:is-in={artOverlayIn}
                src={artOverlaySrc}
                alt=""
                draggable="false"
                decoding="async"
              />
            {/if}
          {:else if !placeholderFailed}
            <img
              class="fullscreen-art art-base art-placeholder-img"
              src={COVER_PLACEHOLDER_SRC}
              alt=""
              draggable="false"
              decoding="async"
              onerror={() => {
                placeholderFailed = true;
              }}
            />
          {:else}
            <div class="fullscreen-art-placeholder" aria-hidden="true">
              <svg width="72" height="72" viewBox="0 0 24 24" fill="currentColor">
                <path d="M12 3v10.55A4 4 0 1 0 14 17V7h4V3h-6z"/>
              </svg>
            </div>
          {/if}
        </div>

        <div class="fullscreen-meta">
          <h2 class="fullscreen-title">
            {player.currentTrack ? trackDisplayTitle(player.currentTrack) : player.currentFileName ?? ''}
          </h2>
          {#if player.currentTrack}
            <p class="fullscreen-artist">{trackDisplayArtist(player.currentTrack)}</p>
          {/if}
        </div>
      </aside>

      <div class="fullscreen-lyrics-slot" aria-hidden={!showLyricsPanel}>
        <!-- Mount lyrics while still hidden so the slot can transition from opacity 0 → 1 -->
        {#if hasLyrics && lyricsVisible}
          <FullscreenLyrics
            lines={lyricsState?.lines ?? []}
            syncType={lyricsState?.syncType ?? 'none'}
            currentTime={player.position}
            isPlaying={player.isPlaying}
            chromeVisible={chromeVisible}
            onSeek={(time) => void player.seek(time)}
          />
        {/if}
      </div>
    </div>

    <div class="fullscreen-bottom-chrome" bind:this={chromeEl}>
      <div
        class="fullscreen-bottom-chrome-inner"
        class:chrome-hidden={!chromeVisible && !pointerOverChrome}
      >
        <div class="fullscreen-toolbar">
          <div class="fullscreen-controls">
          <button
            class="fs-control-btn mode-btn"
            class:active={player.shuffleEnabled}
            onclick={() => player.toggleShuffle()}
            disabled={!player.hasPlayingTracks}
            aria-label={player.shuffleEnabled ? 'Disable shuffle' : 'Enable shuffle'}
            title={player.shuffleEnabled ? 'Shuffle on' : 'Shuffle'}
          >
            <span
              class="fs-icon"
              style:--fs-icon={player.shuffleEnabled
                ? "url('/icons/shuffle.svg')"
                : "url('/icons/noshuffle.svg')"}
              aria-hidden="true"
            ></span>
          </button>

          <button
            class="fs-control-btn"
            onclick={() => player.prevTrack()}
            disabled={!player.hasTrack}
            aria-label="Previous track"
          >
            <span class="fs-icon" style:--fs-icon={"url('/icons/playbackward.svg')"} aria-hidden="true"></span>
          </button>

          <button
            class="fs-control-btn play-btn"
            class:playing={player.isPlaying}
            onclick={() => player.togglePlayPause()}
            disabled={!player.hasPlayingTracks && !player.hasTrack}
            aria-label={player.isPlaying ? 'Pause' : player.isPaused ? 'Resume' : 'Play'}
          >
            <span
              class="fs-icon fs-icon-play"
              style:--fs-icon={player.isPlaying
                ? "url('/icons/pause.svg')"
                : "url('/icons/play.svg')"}
              aria-hidden="true"
            ></span>
          </button>

          <button
            class="fs-control-btn"
            onclick={() => player.nextTrack()}
            disabled={!player.hasNext}
            aria-label="Next track"
          >
            <span class="fs-icon" style:--fs-icon={"url('/icons/playforward.svg')"} aria-hidden="true"></span>
          </button>

          <button
            class="fs-control-btn mode-btn"
            class:active={player.repeatMode !== 'off'}
            onclick={() => player.toggleRepeat()}
            disabled={!player.hasPlayingTracks}
            aria-label={
              player.repeatMode === 'one'
                ? 'Disable repeat'
                : player.repeatMode === 'all'
                  ? 'Repeat one'
                  : 'Repeat all'
            }
          >
            <span
              class="fs-icon"
              style:--fs-icon={
                player.repeatMode === 'one'
                  ? "url('/icons/repeat.svg')"
                  : player.repeatMode === 'all'
                    ? "url('/icons/repeatplaylist.svg')"
                    : "url('/icons/norepeat.svg')"
              }
              aria-hidden="true"
            ></span>
          </button>
          </div>

          <div class="fullscreen-volume">
            <button
              type="button"
              class="lyrics-toggle-btn"
              class:active={lyricsVisible}
              onclick={toggleLyrics}
              aria-label={lyricsVisible ? 'Hide lyrics' : 'Show lyrics'}
              title={lyricsVisible ? 'Hide lyrics' : 'Show lyrics'}
            >
              <span
                class="fs-icon fs-icon-sm"
                style:--fs-icon={lyricsVisible
                  ? "url('/icons/text.svg')"
                  : "url('/icons/textclose.svg')"}
                aria-hidden="true"
              ></span>
            </button>
            <MediaSlider variant="volume" useStaticIcons />
          </div>
        </div>

        <div class="fullscreen-progress">
          <MediaSlider variant="progress" />
        </div>
      </div>
    </div>
  </div>
{/if}

<style>
  @import './FullscreenPlayer.css';
</style>