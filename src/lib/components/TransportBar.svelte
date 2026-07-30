<script lang="ts">
  import { getCoverSrc, resolveCoverSrc } from '$lib/coverCache';
  import { setAccentFromCoverSrc } from '$lib/coverAccent';
  import { COVER_PLACEHOLDER_SRC } from '$lib/coverPlaceholder';
  import { getPlayerStore, trackDisplayArtist } from '$lib/stores/player.svelte';
  import { exportAudioPathForTrack } from '$lib/trackPaths';
  import { startFileDrag } from '$lib/fileDrag';
  import FullscreenPlayer from './FullscreenPlayer.svelte';
  import MediaSlider from './MediaSlider.svelte';
  import TrackCover from './TrackCover.svelte';

  interface Props {
    fullscreenOpen?: boolean;
  }

  let { fullscreenOpen = $bindable(false) }: Props = $props();

  const player = getPlayerStore();

  const DRAG_THRESHOLD = 6;

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

  // Accent color from the juiciest hue on the current cover (or placeholder).
  $effect(() => {
    const track = player.currentTrack;
    const hasTrack = player.hasTrack;
    const path = track?.cover_path ?? track?.cover_path_full ?? null;

    if (!hasTrack) {
      void setAccentFromCoverSrc(null);
      return;
    }

    let cancelled = false;

    const apply = (src: string) => {
      if (!cancelled) void setAccentFromCoverSrc(src);
    };

    if (!path) {
      apply(COVER_PLACEHOLDER_SRC);
      return () => {
        cancelled = true;
      };
    }

    const immediate = getCoverSrc(path);
    if (immediate) apply(immediate);

    void resolveCoverSrc(path).then((src) => {
      if (src) apply(src);
      else if (!cancelled) apply(COVER_PLACEHOLDER_SRC);
    });

    return () => {
      cancelled = true;
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
            <span class="control-icon like-icon" style:--control-icon={"url('/icons/heart.svg')"} aria-hidden="true"></span>
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
      {#if player.shuffleEnabled}
        <span class="control-icon" style:--control-icon={"url('/icons/shuffle.svg')"} aria-hidden="true"></span>
      {:else}
        <span class="control-icon" style:--control-icon={"url('/icons/noshuffle.svg')"} aria-hidden="true"></span>
      {/if}
      </button>

      <button
        class="control-btn"
        onclick={() => player.prevTrack()}
        disabled={!player.hasTrack}
        aria-label="Previous track"
      >
        <span class="control-icon" style:--control-icon={"url('/icons/playbackward.svg')"} aria-hidden="true"></span>
      </button>

      <button
        class="control-btn play-btn"
        class:playing={player.isPlaying}
        onclick={() => player.togglePlayPause()}
        disabled={!player.hasPlayingTracks && !player.hasTrack}
        aria-label={player.isPlaying ? 'Pause' : player.isPaused ? 'Resume' : 'Play'}
      >
        {#if player.isPlaying}
          <span class="control-icon play-icon" style:--control-icon={"url('/icons/pause.svg')"} aria-hidden="true"></span>
        {:else}
          <span class="control-icon play-icon" style:--control-icon={"url('/icons/play.svg')"} aria-hidden="true"></span>
        {/if}
      </button>

      <button
        class="control-btn"
        onclick={() => player.nextTrack()}
        disabled={!player.hasNext}
        aria-label="Next track"
      >
        <span class="control-icon" style:--control-icon={"url('/icons/playforward.svg')"} aria-hidden="true"></span>
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
        <!-- <span class="control-icon" style:--control-icon={"url('/icons/repeat.svg')"} aria-hidden="true"></span> -->
        {#if player.repeatMode === 'one'}
          <span class="control-icon" style:--control-icon={"url('/icons/repeat.svg')"} aria-hidden="true"></span>
        {:else if player.repeatMode === 'all'}
          <span class="control-icon" style:--control-icon={"url('/icons/repeatplaylist.svg')"} aria-hidden="true"></span>
        {:else}
          <span class="control-icon" style:--control-icon={"url('/icons/norepeat.svg')"} aria-hidden="true"></span>
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

{#if fullscreenOpen}
  <FullscreenPlayer bind:open={fullscreenOpen} />
{/if}

<style>
  @import './TransportBar.css';
</style>