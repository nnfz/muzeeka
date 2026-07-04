<script lang="ts">
  import { getPlayerStore } from '$lib/stores/player.svelte';

  interface Props {
    variant: 'progress' | 'volume';
  }

  let { variant }: Props = $props();

  const player = getPlayerStore();
  const SEEK_STEP_SEC = 5;

  let isDragging = $state(false);
  let dragValue = $state(0);
  let trackEl = $state<HTMLDivElement | null>(null);
  let previousVolume = $state(0.8);

  let isMuted = $derived(player.volume === 0);
  let volumeIcon = $derived(
    isMuted ? 'muted' : player.volume > 0.5 ? 'high' : player.volume > 0 ? 'low' : 'muted'
  );

  let displayTime = $derived(
    isDragging && variant === 'progress'
      ? formatTime(dragValue * player.duration)
      : player.formattedPosition
  );

  let fillRatio = $derived(
    variant === 'progress'
      ? isDragging
        ? dragValue
        : player.progress
      : player.volume
  );

  function formatTime(seconds: number): string {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, '0')}`;
  }

  function updateDragValue(e: MouseEvent) {
    if (!trackEl) return;
    const rect = trackEl.getBoundingClientRect();
    dragValue = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));

    if (variant === 'volume') {
      player.setVolume(dragValue);
    }
  }

  function handleTrackMouseDown(e: MouseEvent) {
    isDragging = true;
    updateDragValue(e);
    window.addEventListener('mousemove', handleTrackMouseMove);
    window.addEventListener('mouseup', handleTrackMouseUp);
  }

  function handleTrackMouseMove(e: MouseEvent) {
    if (isDragging) {
      updateDragValue(e);
    }
  }

  function handleTrackMouseUp() {
    if (isDragging && variant === 'progress') {
      player.seek(dragValue * player.duration);
    }
    isDragging = false;
    window.removeEventListener('mousemove', handleTrackMouseMove);
    window.removeEventListener('mouseup', handleTrackMouseUp);
  }

  function toggleMute() {
    if (isMuted) {
      player.setVolume(previousVolume || 0.8);
    } else {
      previousVolume = player.volume;
      player.setVolume(0);
    }
  }

  function handleWheel(e: WheelEvent) {
    e.preventDefault();
    e.stopPropagation();

    if (variant === 'progress') {
      if (!player.hasTrack || player.duration <= 0) return;

      const step = e.deltaY > 0 ? -SEEK_STEP_SEC : SEEK_STEP_SEC;
      const next = Math.max(0, Math.min(player.duration, player.position + step));
      void player.seek(next);
      return;
    }

    const step = e.deltaY > 0 ? -0.04 : 0.04;
    const next = Math.max(0, Math.min(1, player.volume + step));
    player.setVolume(next);
  }

  function wheelAction(node: HTMLElement) {
    node.addEventListener('wheel', handleWheel, { passive: false });
    return {
      destroy() {
        node.removeEventListener('wheel', handleWheel);
      },
    };
  }
</script>

<div class="media-slider {variant}" use:wheelAction>
  {#if variant === 'volume'}
    <button
      class="volume-btn"
      onclick={toggleMute}
      aria-label={isMuted ? 'Unmute' : 'Mute'}
    >
      {#if volumeIcon === 'muted'}
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/>
          <line x1="23" y1="9" x2="17" y2="15"/>
          <line x1="17" y1="9" x2="23" y2="15"/>
        </svg>
      {:else if volumeIcon === 'low'}
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/>
          <path d="M15.54 8.46a5 5 0 0 1 0 7.07"/>
        </svg>
      {:else}
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/>
          <path d="M15.54 8.46a5 5 0 0 1 0 7.07"/>
          <path d="M19.07 4.93a10 10 0 0 1 0 14.14"/>
        </svg>
      {/if}
    </button>
  {:else}
    <span class="slider-time current">{displayTime}</span>
  {/if}

  <div
    class="slider-track"
    bind:this={trackEl}
    onmousedown={handleTrackMouseDown}
    role="slider"
    tabindex="0"
    aria-label={variant === 'progress' ? 'Seek position' : 'Volume'}
    aria-valuemin={0}
    aria-valuemax={variant === 'progress' ? player.duration : 1}
    aria-valuenow={variant === 'progress' ? player.position : player.volume}
  >
    <div class="slider-fill" style="width: {fillRatio * 100}%">
      <div class="slider-thumb" class:active={isDragging}></div>
    </div>
  </div>

  {#if variant === 'progress'}
    <span class="slider-time duration">{player.formattedDuration}</span>
  {/if}
</div>

<style>
  @import './MediaSlider.css';
</style>