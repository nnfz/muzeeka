<script lang="ts">
  import { getCoverSrc } from '$lib/coverCache';
  import {
    getPlayerStore,
    trackDisplayArtist,
    trackDisplayTitle,
  } from '$lib/stores/player.svelte';
  import { invoke } from '@tauri-apps/api/core';
  import MediaSlider from './MediaSlider.svelte';

  interface Props {
    open?: boolean;
  }

  let { open = $bindable(false) }: Props = $props();

  const player = getPlayerStore();

  let resolvedFullCoverPath = $state<string | null>(null);

  let coverPath = $derived(
    resolvedFullCoverPath
      ?? player.currentTrack?.cover_path_full
      ?? player.currentTrack?.cover_path
      ?? null
  );
  let coverSrc = $derived(getCoverSrc(coverPath));
  let album = $derived(player.currentTrack?.album?.trim() || null);

  const CHROME_HIDE_DELAY = 3200;

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
      return;
    }

    chromeVisible = true;
    scheduleChromeHide();

    return () => {
      clearHideTimer();
    };
  });

  $effect(() => {
    const file = player.currentFile;
    const track = player.currentTrack;

    if (!open || !file) {
      resolvedFullCoverPath = null;
      return;
    }

    if (track?.cover_path_full) {
      resolvedFullCoverPath = track.cover_path_full;
      return;
    }

    let cancelled = false;
    resolvedFullCoverPath = null;

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
</script>

<svelte:window onkeydown={handleKeydown} />

{#if open && player.hasTrack}
  <div class="fullscreen-player" role="dialog" aria-modal="true" aria-label="Now playing">
    <div class="fullscreen-backdrop" aria-hidden="true">
      {#if coverSrc}
        <img class="fullscreen-backdrop-img" src={coverSrc} alt="" />
      {/if}
      <div class="fullscreen-backdrop-shade"></div>
    </div>

    <div class="fullscreen-center">
      <div class="fullscreen-art-wrap">
        {#if coverSrc}
          <img class="fullscreen-art" src={coverSrc} alt="" draggable="false" />
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
        {#if album}
          <p class="fullscreen-album">{album}</p>
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
        <div class="fullscreen-progress">
          <MediaSlider variant="progress" />
        </div>
      </div>
    </div>
  </div>

  <button
    type="button"
    class="fullscreen-close-overlay"
    onclick={close}
    aria-label="Close fullscreen player"
    title="Close"
  >
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <polyline points="6 9 12 15 18 9"/>
    </svg>
  </button>
{/if}

<style>
  @import './FullscreenPlayer.css';
</style>