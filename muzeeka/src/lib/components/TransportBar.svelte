<script lang="ts">
  import { getPlayerStore, trackDisplayArtist } from '$lib/stores/player.svelte';
  import { exportAudioPathForTrack } from '$lib/trackPaths';
  import { startFileDrag } from '$lib/fileDrag';
  import FullscreenPlayer from './FullscreenPlayer.svelte';
  import MediaSlider from './MediaSlider.svelte';
  import TrackCover from './TrackCover.svelte';

  const player = getPlayerStore();

  const DRAG_THRESHOLD = 6;

  let fullscreenOpen = $state(false);

  let fileDragSession = $state<{
    x: number;
    y: number;
    path: string;
    iconPath: string | null;
    started: boolean;
    openFullscreenOnClick?: boolean;
  } | null>(null);
  let fileDragCaptureEl = $state<HTMLElement | null>(null);
  let fileDragPointerId = $state<number | null>(null);

  function cleanupFileDragSession() {
    window.removeEventListener('pointermove', onPlayerPointerMove);
    window.removeEventListener('pointerup', onPlayerPointerUp);
    window.removeEventListener('pointercancel', onPlayerPointerUp);
    window.removeEventListener('blur', onPlayerPointerCancel);
    document.removeEventListener('visibilitychange', onPlayerFileDragVisibility);

    if (fileDragCaptureEl && fileDragPointerId !== null) {
      try {
        if (fileDragCaptureEl.hasPointerCapture(fileDragPointerId)) {
          fileDragCaptureEl.releasePointerCapture(fileDragPointerId);
        }
      } catch {
        /* pointer may already be released */
      }
    }

    fileDragCaptureEl = null;
    fileDragPointerId = null;
    fileDragSession = null;
  }

  function onPlayerPointerCancel() {
    cleanupFileDragSession();
  }

  function onPlayerFileDragVisibility() {
    if (document.visibilityState === 'hidden') {
      onPlayerPointerCancel();
    }
  }

  function beginFileDragSession(
    e: PointerEvent,
    options?: { openFullscreenOnClick?: boolean }
  ) {
    if (e.button !== 0) return;
    if (!player.currentFile || !player.currentTrack) return;
    if ((e.target as HTMLElement).closest('.like-btn-transport')) return;

    const path = exportAudioPathForTrack(player.currentTrack, player.currentFile);
    if (!path) return;

    cleanupFileDragSession();

    fileDragSession = {
      x: e.clientX,
      y: e.clientY,
      path,
      iconPath: player.currentTrack.cover_path ?? null,
      started: false,
      openFullscreenOnClick: options?.openFullscreenOnClick,
    };
    fileDragCaptureEl = e.currentTarget as HTMLElement;
    fileDragPointerId = e.pointerId;
    fileDragCaptureEl.setPointerCapture(e.pointerId);

    window.addEventListener('pointermove', onPlayerPointerMove);
    window.addEventListener('pointerup', onPlayerPointerUp);
    window.addEventListener('pointercancel', onPlayerPointerUp);
    window.addEventListener('blur', onPlayerPointerCancel);
    document.addEventListener('visibilitychange', onPlayerFileDragVisibility);
  }

  function onCoverPointerDown(e: PointerEvent) {
    beginFileDragSession(e, { openFullscreenOnClick: true });
  }

  function onTextPointerDown(e: PointerEvent) {
    beginFileDragSession(e);
  }

  function onPlayerPointerMove(e: PointerEvent) {
    const session = fileDragSession;
    if (!session || session.started) return;

    const dx = e.clientX - session.x;
    const dy = e.clientY - session.y;
    if (Math.hypot(dx, dy) < DRAG_THRESHOLD) return;

    const { path, iconPath } = session;
    cleanupFileDragSession();
    void startFileDrag([path], { iconPath }).catch((err) => {
      console.error('Failed to start file drag:', err);
    });
  }

  function onPlayerPointerUp() {
    const session = fileDragSession;
    const shouldOpenFullscreen = !!session?.openFullscreenOnClick && !session.started;
    cleanupFileDragSession();
    if (shouldOpenFullscreen) {
      fullscreenOpen = true;
    }
  }

  $effect(() => {
    document.documentElement.classList.toggle('fullscreen-active', fullscreenOpen);
    return () => {
      document.documentElement.classList.remove('fullscreen-active');
    };
  });
</script>

<div class="transport-bar glass">
  <div class="transport-content">
    <div class="transport-info">
      {#if player.hasTrack}
        <div class="np-drag-handle">
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="np-cover-hit"
            onpointerdown={onCoverPointerDown}
            title="Open fullscreen · drag to share"
          >
            <TrackCover track={player.currentTrack} />
          </div>
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="now-playing-text"
            onpointerdown={onTextPointerDown}
            title="Drag file to share"
          >
            <span class="np-title">{player.currentFileName ?? ''}</span>
            {#if player.currentTrack}
              <span class="np-artist">{trackDisplayArtist(player.currentTrack)}</span>
            {/if}
          </div>
        </div>

        {#if player.hasTrack && player.currentFile}
          <button
            class="like-btn-transport"
            class:liked={player.isLiked(player.currentFile)}
            onclick={() => { if (player.currentFile) player.toggleLike(player.currentFile); }}
            title={player.isLiked(player.currentFile) ? 'Remove from Liked' : 'Add to Liked'}
            aria-label={player.isLiked(player.currentFile) ? 'Unlike current track' : 'Like current track'}
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill={player.isLiked(player.currentFile) ? 'currentColor' : 'none'} stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
            </svg>
          </button>
        {/if}
      {/if}
    </div>

    <div class="transport-controls">
      <button
        class="control-btn mode-btn"
        class:active={player.shuffleEnabled}
        onclick={() => player.toggleShuffle()}
        disabled={!player.hasPlayingTracks}
        aria-label={player.shuffleEnabled ? 'Disable shuffle' : 'Enable shuffle'}
        title={player.shuffleEnabled ? 'Shuffle on' : 'Shuffle'}
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <path d="M10.59 9.17 5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41-1.41 1.41 3.13 3.13L14.5 20H20v-5.51l-2.04 2.04-3.13-3.12z"/>
        </svg>
      </button>

      <button
        class="control-btn"
        onclick={() => player.prevTrack()}
        disabled={!player.hasTrack}
        aria-label="Previous track"
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
          <path d="M6 6h2v12H6zm3.5 6 8.5 6V6z"/>
        </svg>
      </button>

      <button
        class="control-btn play-btn"
        class:playing={player.isPlaying}
        onclick={() => player.togglePlayPause()}
        disabled={!player.hasPlayingTracks && !player.hasTrack}
        aria-label={player.isPlaying ? 'Pause' : player.isPaused ? 'Resume' : 'Play'}
      >
        {#if player.isPlaying}
          <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor">
            <rect x="6" y="4" width="4" height="16" rx="1"/>
            <rect x="14" y="4" width="4" height="16" rx="1"/>
          </svg>
        {:else}
          <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor">
            <path d="M8 5v14l11-7z"/>
          </svg>
        {/if}
      </button>

      <button
        class="control-btn"
        onclick={() => player.nextTrack()}
        disabled={!player.hasNext}
        aria-label="Next track"
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
          <path d="M6 18l8.5-6L6 6v12zM16 6v12h2V6h-2z"/>
        </svg>
      </button>

      <button
        class="control-btn mode-btn"
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
        title={
          player.repeatMode === 'one'
            ? 'Repeat one'
            : player.repeatMode === 'all'
              ? 'Repeat all'
              : 'Repeat'
        }
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <path d="M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4z"/>
        </svg>
        {#if player.repeatMode === 'one'}
          <span class="repeat-one-badge" aria-hidden="true">1</span>
        {/if}
      </button>

    </div>

    <div class="transport-right">
      <MediaSlider variant="volume" />
    </div>
  </div>
  <div class="transport-progress">
    <MediaSlider variant="progress" />
  </div>

</div>

<FullscreenPlayer bind:open={fullscreenOpen} />

<style>
  @import './TransportBar.css';
</style>