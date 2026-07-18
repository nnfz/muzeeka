<script lang="ts">
  import { getPlayerStore } from '$lib/stores/player.svelte';

  interface Props {
    variant: 'progress' | 'volume';
    /** Fullscreen: use /static/icons volume set instead of inline SVG. */
    useStaticIcons?: boolean;
  }

  let { variant, useStaticIcons = false }: Props = $props();

  const player = getPlayerStore();
  const SEEK_STEP_SEC = 5;

  let isDragging = $state(false);
  let dragValue = $state(0);
  let pendingValue = $state<number | null>(null);
  let trackEl = $state<HTMLDivElement | null>(null);
  let previousVolume = $state(0.8);

  let isMuted = $derived(player.volume === 0);
  let volumeIcon = $derived(
    isMuted
      ? 'muted'
      : player.volume > 0.66
        ? 'high'
        : player.volume > 0.33
          ? 'med'
          : player.volume > 0
            ? 'low'
            : 'muted'
  );

  let staticVolumeIconUrl = $derived(
    volumeIcon === 'muted'
      ? '/icons/mute.svg'
      : volumeIcon === 'high'
        ? '/icons/volmax.svg'
        : volumeIcon === 'med'
          ? '/icons/volmed.svg'
          : '/icons/volmin.svg'
  );

  let activeRatio = $derived(
    isDragging
      ? dragValue
      : pendingValue !== null
        ? pendingValue
        : variant === 'progress'
          ? player.progress
          : player.volume
  );

  let displayTime = $derived(
    variant === 'progress'
      ? formatTime(activeRatio * player.duration)
      : player.formattedPosition
  );

  let fillRatio = $derived(activeRatio);

  $effect(() => {
    if (pendingValue === null) return;

    const actual = variant === 'progress' ? player.progress : player.volume;
    if (Math.abs(actual - pendingValue) < 0.01) {
      pendingValue = null;
    }
  });

  function formatTime(seconds: number): string {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, '0')}`;
  }

  function updateDragValue(clientX: number) {
    if (!trackEl) return;
    const rect = trackEl.getBoundingClientRect();
    if (rect.width <= 0) return;
    dragValue = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
  }

  function applyVolumeDrag() {
    player.setVolume(dragValue);
  }

  function commitDragValue() {
    if (variant === 'progress') {
      pendingValue = dragValue;
      void player.seek(dragValue * player.duration);
    } else {
      applyVolumeDrag();
    }
  }

  function handleTrackPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;

    isDragging = true;
    updateDragValue(e.clientX);
    if (variant === 'volume') applyVolumeDrag();
    trackEl?.setPointerCapture(e.pointerId);
    e.preventDefault();
  }

  function handleTrackPointerMove(e: PointerEvent) {
    if (!isDragging) return;
    updateDragValue(e.clientX);
    if (variant === 'volume') applyVolumeDrag();
  }

  function handleTrackPointerUp(e: PointerEvent) {
    if (!isDragging) return;

    updateDragValue(e.clientX);
    commitDragValue();
    isDragging = false;

    if (trackEl?.hasPointerCapture(e.pointerId)) {
      trackEl.releasePointerCapture(e.pointerId);
    }
  }

  function handleTrackPointerCancel(e: PointerEvent) {
    if (!isDragging) return;
    isDragging = false;

    if (trackEl?.hasPointerCapture(e.pointerId)) {
      trackEl.releasePointerCapture(e.pointerId);
    }
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
      {#if useStaticIcons}
        <span
          class="volume-static-icon"
          style:--vol-icon={"url('" + staticVolumeIconUrl + "')"}
          aria-hidden="true"
        ></span>
      {:else if volumeIcon === 'muted'}
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/>
          <line x1="23" y1="9" x2="17" y2="15"/>
          <line x1="17" y1="9" x2="23" y2="15"/>
        </svg>
      {:else if volumeIcon === 'low' || volumeIcon === 'med'}
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
    class:is-dragging={isDragging}
    class:is-pending={pendingValue !== null}
    bind:this={trackEl}
    onpointerdown={handleTrackPointerDown}
    onpointermove={handleTrackPointerMove}
    onpointerup={handleTrackPointerUp}
    onpointercancel={handleTrackPointerCancel}
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