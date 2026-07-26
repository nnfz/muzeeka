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
    type MusicFile,
  } from '$lib/stores/player.svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import type { LyricsResult } from '$lib/lyrics/types';
  import {
    invalidateLyricsCache,
    loadLyricsForPath,
    peekLyricsCache,
    prefetchLyricsForPath,
    setLyricsCache,
  } from '$lib/lyrics/lyricsCache';
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
  /** Cover/lyrics column travel — match FullscreenPlayer.css transitions. */
  const LAYOUT_CLOSE_MS = 400;
  /** After open settle — transitions + Kawarp / lyrics layout allowed. */
  let enterDone = $state(false);
  /**
   * Open surface animation:
   * - `from`: mounted oversized + blurred + faded (no CSS transition yet)
   * - `active`: animating to scale 1 / sharp / opaque
   * - `done`: settled (same as enterDone for lyrics timing)
   */
  /** Component only mounts while open — start on the oversized first frame. */
  let enterPhase = $state<'idle' | 'from' | 'active' | 'done'>('from');
  /** Full open zoom/blur duration — keep in sync with FullscreenPlayer.css. */
  const FS_ENTER_MS = 480;
  /**
   * Cover left + lyrics column.
   * On track change: always close fully, then open again if the next track has lyrics.
   */
  let lyricsLayoutActive = $state(false);
  let hasLyrics = $derived((lyricsState?.lines.length ?? 0) > 0);
  let showLyricsPanel = $derived(lyricsLayoutActive && lyricsVisible);
  const CHROME_HIDE_DELAY = 1800;
  const LYRICS_PREFETCH_COUNT = 2;
  /** Bumps on every track switch so stale close/open timers cannot reopen wrong layout. */
  let lyricsSwitchGen = 0;
  let lyricsReopenTimer: ReturnType<typeof setTimeout> | null = null;
  /** When layout was closed for a track switch (0 = no pending close wait). */
  let layoutClosedAt = 0;

  function lyricsParamsForTrack(
    track: MusicFile | null | undefined,
    durationFallback?: number | null,
  ) {
    if (!track) return null;
    return {
      title: trackDisplayTitle(track),
      artist: trackDisplayArtist(track),
      album: track.album,
      durationSecs:
        track.duration_secs
        ?? (durationFallback != null && durationFallback > 0 ? durationFallback : null),
    };
  }

  function clearLyricsReopenTimer() {
    if (lyricsReopenTimer) {
      clearTimeout(lyricsReopenTimer);
      lyricsReopenTimer = null;
    }
  }

  /** After layout has closed, open again if this generation still has lyrics. */
  function scheduleLyricsLayoutOpen(gen: number, delayMs: number) {
    clearLyricsReopenTimer();
    const openNow = () => {
      if (gen !== lyricsSwitchGen) return;
      if (!untrack(() => open) || !untrack(() => enterDone) || !untrack(() => lyricsVisible)) {
        return;
      }
      if (!untrack(() => hasLyrics)) return;
      // Two rAFs so the browser paints the closed state before opening (CSS transition runs).
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          if (gen !== lyricsSwitchGen) return;
          if (!untrack(() => hasLyrics) || !untrack(() => lyricsVisible) || !untrack(() => enterDone)) {
            return;
          }
          lyricsLayoutActive = true;
          layoutClosedAt = 0;
        });
      });
    };
    if (delayMs <= 0) {
      openNow();
      return;
    }
    lyricsReopenTimer = setTimeout(() => {
      lyricsReopenTimer = null;
      openNow();
    }, delayMs);
  }

  function applyLyricsForFile(file: string, result: LyricsResult | null, gen: number) {
    setLyricsCache(file, result);
    if (gen !== lyricsSwitchGen) return;
    if (untrack(() => player.currentFile) !== file) return;

    lyricsState = result;
    lyricsSettledForFile = file;

    const nextHas = (result?.lines.length ?? 0) > 0;
    if (!nextHas) {
      lyricsLayoutActive = false;
      clearLyricsReopenTimer();
      return;
    }

    // Finish the close animation, then open cleanly (no half-state thrash).
    const closedAt = layoutClosedAt;
    const delay = closedAt > 0
      ? Math.max(0, LAYOUT_CLOSE_MS - (Date.now() - closedAt))
      : 0;
    scheduleLyricsLayoutOpen(gen, delay);
  }

  function prefetchUpcomingLyrics() {
    if (!open) return;
    const current = untrack(() => player.currentFile);
    if (!current) return;
    const upcoming = player.getUpcomingTracks(current, LYRICS_PREFETCH_COUNT);
    for (const track of upcoming) {
      const params = lyricsParamsForTrack(track);
      if (!params) continue;
      prefetchLyricsForPath(track.path, params);
    }
  }

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
      clearLyricsReopenTimer();
      chromeVisible = true;
      pointerOverChrome = false;
      sawPointerMove = false;
      lyricsVisible = true;
      lyricsMounted = true;
      enterDone = false;
      enterPhase = 'idle';
      lyricsLayoutActive = false;
      layoutClosedAt = 0;
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
    layoutClosedAt = 0;

    // Paint first frame oversized/blurred without transition, then animate in.
    enterPhase = 'from';
    let raf1 = 0;
    let raf2 = 0;
    let enterTimer: ReturnType<typeof setTimeout> | null = null;

    raf1 = requestAnimationFrame(() => {
      raf2 = requestAnimationFrame(() => {
        enterPhase = 'active';
        enterTimer = setTimeout(() => {
          enterPhase = 'done';
          enterDone = true;
        }, FS_ENTER_MS);
      });
    });

    return () => {
      clearHideTimer();
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
      if (enterTimer) clearTimeout(enterTimer);
    };
  });

  // First open / user re-enabled lyrics: open layout when we already have lines.
  $effect(() => {
    if (!open || !enterDone || !lyricsVisible) return;
    if (!hasLyrics || lyricsLayoutActive) return;
    // Track-switch path schedules its own reopen; only handle cold open here.
    if (layoutClosedAt > 0) return;

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

  // Track change: close layout fully, then load lyrics and reopen if present.
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

    // Always animate closed on switch, then open again when ready.
    const gen = ++lyricsSwitchGen;
    const wasOpen = untrack(() => lyricsLayoutActive);
    clearLyricsReopenTimer();
    lyricsLayoutActive = false;
    lyricsState = null;
    lyricsSettledForFile = null;
    layoutClosedAt = wasOpen ? Date.now() : 0;

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

  // After manual TTML import / clear — invalidate memory cache + re-fetch.
  $effect(() => {
    const unlisteners: Array<() => void> = [];
    const onLyricsCacheChanged = (payload: string | undefined) => {
      const changedPath = payload?.trim() || '';
      const current = untrack(() => player.currentFile);
      if (changedPath) {
        invalidateLyricsCache(changedPath);
      } else {
        invalidateLyricsCache();
      }
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

  // Fetch lyrics for current track; reopen layout after close completes when lines exist.
  $effect(() => {
    const file = player.currentFile;
    const isOpen = open;
    // Track generation so this fetch is tied to the latest switch.
    const gen = lyricsSwitchGen;
    void lyricsSettledForFile;

    if (!isOpen || !file) {
      if (!isOpen) {
        lyricsState = null;
        lyricsSettledForFile = null;
      }
      return;
    }

    if (untrack(() => lyricsSettledForFile === file)) {
      prefetchUpcomingLyrics();
      return;
    }

    let alive = true;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;

    const run = () => {
      if (!alive) return;

      const cached = peekLyricsCache(file);
      if (cached !== undefined) {
        applyLyricsForFile(file, cached, gen);
        prefetchUpcomingLyrics();
        return;
      }

      const params = untrack(() => lyricsParamsForTrack(player.currentTrack, player.duration));
      if (!params) {
        retryTimer = setTimeout(run, 80);
        return;
      }

      void loadLyricsForPath(file, params).then((result) => {
        if (!alive) return;
        applyLyricsForFile(file, result, gen);
        prefetchUpcomingLyrics();
      });
    };

    run();

    return () => {
      alive = false;
      if (retryTimer) clearTimeout(retryTimer);
    };
  });

  // Warm next tracks while open.
  $effect(() => {
    if (!open) return;
    void player.currentFile;
    void player.playingTracks;
    void player.shuffleEnabled;
    void player.repeatMode;
    prefetchUpcomingLyrics();
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
    class:fs-enter-from={enterPhase === 'from'}
    class:fs-enter-active={enterPhase === 'active' || enterPhase === 'done'}
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

    <div
      class="fullscreen-layout"
      class:lyrics-hidden={!showLyricsPanel}
    >
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
              <h2 class="fullscreen-title">
                {player.currentTrack ? trackDisplayTitle(player.currentTrack) : player.currentFileName ?? ''}
              </h2>
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
        <!-- Keep mounted while layout open so hide CSS can finish. -->
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