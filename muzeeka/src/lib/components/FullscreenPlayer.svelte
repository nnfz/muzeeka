<script lang="ts">
  import { getCoverSrc, resolveCoverSrc } from '$lib/coverCache';
  import {
    getPlayerStore,
    trackDisplayArtist,
    trackDisplayTitle,
  } from '$lib/stores/player.svelte';
  import { invoke } from '@tauri-apps/api/core';
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
  /** Background may upgrade to full-res; art img stays locked (no mid-open src swap). */
  let bgCoverSrc = $state<string | null>(null);
  let artCoverSrc = $state<string | null>(null);

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
  const CHROME_HIDE_DELAY = 3600;

  let chromeVisible = $state(true);
  let hideTimer: ReturnType<typeof setTimeout> | null = null;

  function clearHideTimer() {
    if (hideTimer) {
      clearTimeout(hideTimer);
      hideTimer = null;
    }
  }

  function scheduleChromeHide() {
    clearHideTimer();
    hideTimer = setTimeout(() => {
      chromeVisible = false;
      hideTimer = null;
    }, CHROME_HIDE_DELAY);
  }

  function showChrome() {
    chromeVisible = true;
    scheduleChromeHide();
  }

  function onChromeEnter() {
    showChrome();
  }

  function onChromeLeave() {
    scheduleChromeHide();
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
      chromeVisible = true;
      lyricsVisible = true;
      enterDone = false;
      lyricsLayoutActive = false;
      artCoverSrc = null;
      bgCoverSrc = null;
      resolvedFullCoverPath = null;
      return;
    }

    chromeVisible = true;
    scheduleChromeHide();
    enterDone = false;
    lyricsLayoutActive = false;
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

  $effect(() => {
    const file = player.currentFile;

    if (!open || !file) {
      return;
    }

    let cancelled = false;

    void invoke<string | null>('library_resolve_full_cover', { path: file })
      .then((path) => {
        if (!cancelled && path) {
          resolvedFullCoverPath = path;
        }
      })
      .catch(() => {});

    return () => {
      cancelled = true;
    };
  });

  // Art cover: lock first bitmap for this open — never swap src (thumb→full was the jerk).
  $effect(() => {
    if (!open) return;
    if (artCoverSrc) return;

    const path =
      player.currentTrack?.cover_path
      ?? player.currentTrack?.cover_path_full
      ?? null;
    if (!path) return;

    const immediate = getCoverSrc(path);
    if (immediate) {
      artCoverSrc = immediate;
      bgCoverSrc = immediate;
    }
  });

  // Background only may upgrade to full-res after open (Kawarp), art stays locked.
  $effect(() => {
    if (!open || !enterDone) return;
    const path = coverPath;
    if (!path) return;

    let cancelled = false;
    void resolveCoverSrc(path).then((src) => {
      if (!cancelled && src) {
        bgCoverSrc = src;
      }
    });

    return () => {
      cancelled = true;
    };
  });

  // Silent background fetch — panel appears only when lines exist.
  // Depend ONLY on open + file. Reading track/duration/settled reactively was
  // re-running this effect mid-flight, cancelling the request, and leaving no lyrics.
  $effect(() => {
    const file = player.currentFile;
    const isOpen = open;

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

<svelte:window onkeydown={handleKeydown} />

{#if open && player.hasTrack}
  <div
    class="fullscreen-player"
    class:enter-done={enterDone}
    role="dialog"
    aria-modal="true"
    aria-label="Now playing"
  >
    <!-- Only the backdrop fades; cover is never in an opacity/transform open chain -->
    <div class="fullscreen-backdrop" aria-hidden="true">
      <KawarpBackground src={bgCoverSrc} active={open && enterDone} />
      <div class="fullscreen-backdrop-shade"></div>
    </div>

    <div class="fullscreen-layout" class:lyrics-hidden={!showLyricsPanel}>
      <aside class="fullscreen-side">
        <div class="fullscreen-art-wrap">
          {#if artCoverSrc}
            <img
              class="fullscreen-art"
              src={artCoverSrc}
              alt=""
              draggable="false"
              decoding="async"
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

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="fullscreen-bottom-chrome"
      onmouseenter={onChromeEnter}
      onmouseleave={onChromeLeave}
    >
      <div
        class="fullscreen-bottom-chrome-inner"
        class:chrome-hidden={!chromeVisible}
        onpointerdown={showChrome}
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
            <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <path d="M10.59 9.17 5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41-1.41 1.41 3.13 3.13L14.5 20H20v-5.51l-2.04 2.04-3.13-3.12z"/>
            </svg>
          </button>

          <button
            class="fs-control-btn"
            onclick={() => player.prevTrack()}
            disabled={!player.hasTrack}
            aria-label="Previous track"
          >
            <svg width="24" height="24" viewBox="0 0 24 24" fill="currentColor">
              <path d="M6 6h2v12H6zm3.5 6 8.5 6V6z"/>
            </svg>
          </button>

          <button
            class="fs-control-btn play-btn"
            class:playing={player.isPlaying}
            onclick={() => player.togglePlayPause()}
            disabled={!player.hasPlayingTracks && !player.hasTrack}
            aria-label={player.isPlaying ? 'Pause' : player.isPaused ? 'Resume' : 'Play'}
          >
            {#if player.isPlaying}
              <svg width="30" height="30" viewBox="0 0 24 24" fill="currentColor">
                <rect x="6" y="4" width="4" height="16" rx="1"/>
                <rect x="14" y="4" width="4" height="16" rx="1"/>
              </svg>
            {:else}
              <svg width="30" height="30" viewBox="0 0 24 24" fill="currentColor">
                <path d="M8 5v14l11-7z"/>
              </svg>
            {/if}
          </button>

          <button
            class="fs-control-btn"
            onclick={() => player.nextTrack()}
            disabled={!player.hasNext}
            aria-label="Next track"
          >
            <svg width="24" height="24" viewBox="0 0 24 24" fill="currentColor">
              <path d="M6 18l8.5-6L6 6v12zM16 6v12h2V6h-2z"/>
            </svg>
          </button>

          <button
            class="fs-control-btn mode-btn"
            class:active={player.repeatMode !== 'off'}
            class:repeat-one={player.repeatMode === 'one'}
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
            <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <path d="M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4z"/>
            </svg>
            {#if player.repeatMode === 'one'}
              <span class="repeat-one-badge" aria-hidden="true">1</span>
            {/if}
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
              <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                <path d="M4 6h16v2H4V6zm0 5h12v2H4v-2zm0 5h14v2H4v-2z"/>
              </svg>
            </button>
            <MediaSlider variant="volume" />
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