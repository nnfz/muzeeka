<script lang="ts">
  import {
    getCoverSrc,
    preferFullCoverPath,
    resolveCoverSrc,
    warmImageSrc,
  } from '$lib/coverCache';
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

  /** Small list cover — always show first (fast). */
  let thumbPath = $derived(player.currentTrack?.cover_path?.trim() || null);
  /** Sharper fullscreen cover (capped on disk at ~720px). */
  let fullPath = $derived(
    resolvedFullCoverPath
      ?? preferFullCoverPath(
        player.currentTrack?.cover_path,
        player.currentTrack?.cover_path_full,
      )
  );

  /** Kawarp background URL. */
  let bgCoverSrc = $state<string | null>(null);

  /** Front cover src — thumb first, then full when decoded. */
  let artSrc = $state(COVER_PLACEHOLDER_SRC);
  let artFile = $state<string | null>(null);
  let placeholderFailed = $state(false);
  let artLoadToken = 0;

  function clearArt() {
    artLoadToken += 1;
    artSrc = COVER_PLACEHOLDER_SRC;
    artFile = null;
    placeholderFailed = false;
  }

  function setArtSrc(next: string | null, file: string | null) {
    const target = next ?? COVER_PLACEHOLDER_SRC;

    // Keep real art if path briefly empty while full-res resolve is in flight.
    if (
      !next
      && file
      && artFile === file
      && artSrc !== COVER_PLACEHOLDER_SRC
    ) {
      return;
    }

    artSrc = target;
    artFile = file;
    if (target !== COVER_PLACEHOLDER_SRC) placeholderFailed = false;
  }

  /**
   * Apply src only after the browser has decoded it — avoids a long blank
   * while a large cover streams in. Callers may paint a smaller stand-in first.
   */
  async function setArtSrcWhenReady(next: string, file: string) {
    const token = ++artLoadToken;
    const ok = await warmImageSrc(next);
    if (token !== artLoadToken) return;
    if (untrack(() => player.currentFile) !== file) return;
    if (!ok) return;
    setArtSrc(next, file);
  }

  let lyricsState = $state<LyricsResult | null>(null);
  /** File path for which fetch finished (hit or miss). Not set while in-flight. */
  let lyricsSettledForFile = $state<string | null>(null);
  let lyricsVisible = $state(true);
  /**
   * Keep FullscreenLyrics mounted through the hide transition so opacity + fly-right
   * can finish (unmounting on lyricsVisible=false killed the text mid-frame).
   */
  let lyricsMounted = $state(true);
  let lyricsUnmountTimer: ReturnType<typeof setTimeout> | null = null;
  /** Match .fullscreen-lyrics-slot leave transition (hide) */
  const LYRICS_EXIT_MS = 280;
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

  function clearLyricsUnmountTimer() {
    if (lyricsUnmountTimer) {
      clearTimeout(lyricsUnmountTimer);
      lyricsUnmountTimer = null;
    }
  }

  let chromeVisible = $state(true);
  /** Reactive: drives class so chrome cannot hide under the cursor. */
  let pointerOverChrome = $state(false);
  let chromeEl = $state<HTMLDivElement | null>(null);
  let titleRef: HTMLDivElement | null = null;
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
    if (lyricsVisible) {
      // Start CSS leave (opacity → 0, translateX → right); unmount after it finishes.
      lyricsVisible = false;
      clearLyricsUnmountTimer();
      lyricsUnmountTimer = setTimeout(() => {
        lyricsUnmountTimer = null;
        if (!lyricsVisible) lyricsMounted = false;
      }, LYRICS_EXIT_MS);
    } else {
      clearLyricsUnmountTimer();
      lyricsMounted = true;
      lyricsVisible = true;
    }
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

  /** Seed cover before first paint when opening — thumb first (already warm from list). */
  $effect.pre(() => {
    if (!open) return;

    untrack(() => {
      if (artFile && artFile === player.currentFile) return;

      const file = player.currentFile;
      const thumb = player.currentTrack?.cover_path?.trim() || null;
      const full = preferFullCoverPath(
        player.currentTrack?.cover_path,
        player.currentTrack?.cover_path_full,
      );
      // Prefer thumb for first paint — full may still be multi‑MB until re-encoded.
      const path = thumb || full;
      if (file && path) {
        const src = getCoverSrc(path);
        if (src) {
          setArtSrc(src, file);
          bgCoverSrc = src;
          return;
        }
      }
      if (file) setArtSrc(COVER_PLACEHOLDER_SRC, file);
      bgCoverSrc = COVER_PLACEHOLDER_SRC;
    });
  });

  $effect(() => {
    if (!open) {
      clearHideTimer();
      clearLyricsUnmountTimer();
      chromeVisible = true;
      pointerOverChrome = false;
      sawPointerMove = false;
      lyricsVisible = true;
      lyricsMounted = true;
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

  // File change: lyrics reset + resolve/shrink full cover on disk.
  $effect(() => {
    const file = player.currentFile;

    if (!open || !file) {
      return;
    }

    const knownFull = untrack(() =>
      preferFullCoverPath(
        player.currentTrack?.cover_path,
        player.currentTrack?.cover_path_full,
      )
    );
    resolvedFullCoverPath = knownFull;

    lyricsState = null;
    lyricsSettledForFile = null;
    lyricsLayoutActive = false;

    let cancelled = false;

    void invoke<string | null>('library_resolve_full_cover', { path: file })
      .then((fullPath) => {
        if (cancelled || !fullPath) return;
        if (untrack(() => player.currentFile) !== file) return;
        if (untrack(() => resolvedFullCoverPath) !== fullPath) {
          resolvedFullCoverPath = fullPath;
        }
      })
      .catch(() => {});

    return () => {
      cancelled = true;
    };
  });

  /**
   * Paint cover fast:
   * 1) thumb immediately (usually already in browser cache from the list)
   * 2) full after decode (capped ~720px on disk — no multi‑MB waits)
   */
  $effect(() => {
    if (!open) return;

    const file = player.currentFile;
    void player.currentTrack;
    const thumb = thumbPath;
    const full = fullPath;

    if (!file) return;

    if (!thumb && !full) {
      bgCoverSrc = COVER_PLACEHOLDER_SRC;
      setArtSrc(COVER_PLACEHOLDER_SRC, file);
      return;
    }

    let cancelled = false;

    const showFull = async (path: string) => {
      const immediate = getCoverSrc(path);
      const src = immediate ?? (await resolveCoverSrc(path));
      if (cancelled || !src) return;
      if (untrack(() => player.currentFile) !== file) return;
      bgCoverSrc = src;
      await setArtSrcWhenReady(src, file);
    };

    // 1) Instant stand-in (thumb is ~96px WebP, usually already decoded in the list).
    if (thumb) {
      const src = getCoverSrc(thumb);
      if (src) {
        setArtSrc(src, file);
        bgCoverSrc = src;
      }
    } else {
      setArtSrc(COVER_PLACEHOLDER_SRC, file);
      bgCoverSrc = COVER_PLACEHOLDER_SRC;
    }

    // 2) Upgrade to full after decode (on-disk full is capped ~720px / ≤400KB).
    if (full && full !== thumb) {
      void showFull(full);
    } else if (!thumb && full) {
      void showFull(full);
    }

    return () => {
      cancelled = true;
    };
  });

  // After manual TTML import / clear — drop settled flag so lyrics re-fetch from cache.
  $effect(() => {
    const unlisteners: Array<() => void> = [];
    const onLyricsCacheChanged = (payload: string | undefined) => {
      const changedPath = payload?.trim() || '';
      const current = untrack(() => player.currentFile);
      if (!changedPath || !current || changedPath === current) {
        lyricsSettledForFile = null;
        lyricsState = null;
      }
    };

    for (const eventName of ['lyrics:imported', 'lyrics:cleared', 'lyrics:refetched'] as const) {
      void listen<string>(eventName, (event) => {
        onLyricsCacheChanged(event.payload);
      }).then((fn) => {
        unlisteners.push(fn);
      });
    }

    return () => {
      for (const unlisten of unlisteners) unlisten();
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

  $effect(() => {
    const el = titleRef;
    if (!el) return;
    void player.currentFile;
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
      <KawarpBackground
        src={bgCoverSrc}
        active={open}
        paused={player.isPaused}
        switchKey={player.currentFile}
        transitionDuration={700}
      />
      <div class="fullscreen-backdrop-shade"></div>
    </div>

    <div class="fullscreen-layout" class:lyrics-hidden={!showLyricsPanel}>
      <aside class="fullscreen-side">
        <div class="fullscreen-side-scale" class:is-paused={player.isPaused}>
          <div class="fullscreen-art-wrap">
            {#if !placeholderFailed}
              <img
                class="fullscreen-art"
                src={artSrc}
                alt=""
                draggable="false"
                decoding="async"
                onerror={() => {
                  if (artSrc === COVER_PLACEHOLDER_SRC) placeholderFailed = true;
                  else setArtSrc(COVER_PLACEHOLDER_SRC, artFile);
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
            <div class="fullscreen-meta-text">
              <div class="fullscreen-title-wrapper" bind:this={titleRef} class:marquee-active={titleRef && titleRef.scrollWidth > titleRef.clientWidth}>
                <h2 class="fullscreen-title">
                  {player.currentTrack ? trackDisplayTitle(player.currentTrack) : player.currentFileName ?? ''}
                </h2>
              </div>
              {#if player.currentTrack}
                <p class="fullscreen-artist">{trackDisplayArtist(player.currentTrack)}</p>
              {/if}
            </div>
            {#if player.hasTrack && player.currentFile}
              <button
                class="like-btn-fullscreen"
                class:liked={player.isLiked(player.currentFile)}
                onclick={() => { if (player.currentFile) player.toggleLike(player.currentFile); }}
                title={player.isLiked(player.currentFile) ? 'Remove from Liked' : 'Add to Liked'}
                aria-label={player.isLiked(player.currentFile) ? 'Unlike current track' : 'Like current track'}
              >
                <svg width="20" height="20" viewBox="0 0 24 24" fill={player.isLiked(player.currentFile) ? 'currentColor' : 'none'} stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
                </svg>
              </button>
            {/if}
          </div>
        </div>
      </aside>

      <div class="fullscreen-lyrics-slot" aria-hidden={!showLyricsPanel}>
        <!--
          Mount while hidden so open can fade/slide in.
          Keep mounted during hide so opacity + fly-right can finish.
        -->
        {#if hasLyrics && lyricsMounted}
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