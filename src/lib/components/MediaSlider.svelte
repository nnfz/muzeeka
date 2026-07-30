<script lang="ts">
  import { onDestroy } from 'svelte';
  import { getPlayerStore } from '$lib/stores/player.svelte';

  interface Props {
    variant: 'progress' | 'volume';
    /** Fullscreen: use /static/icons volume set instead of inline SVG. */
    useStaticIcons?: boolean;
  }

  let { variant, useStaticIcons = false }: Props = $props();

  const player = getPlayerStore();
  const SEEK_STEP_SEC = 5;
  const VOLUME_WHEEL_STEP = 0.04;

  let isDragging = $state(false);
  let dragValue = $state(0);
  let pendingValue = $state<number | null>(null);
  let trackEl = $state<HTMLDivElement | null>(null);
  let previousVolume = $state(0.8);
  let liveVolume = $state(player.volume);

  /** % bubble: shown only while this is true; hide via CSS animation end (no sticky timers). */
  let showPct = $state(false);
  /** Bump to remount bubble and restart fade-out animation. */
  let pctKey = $state(0);
  /** True while pointer is over this volume control (for window wheel). */
  let hovered = $state(false);

  let isMuted = $derived(liveVolume === 0);
  let volumeIcon = $derived(
    isMuted
      ? 'muted'
      : liveVolume > 0.66
        ? 'high'
        : liveVolume > 0.33
          ? 'med'
          : liveVolume > 0
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
          : liveVolume
  );

  let displayTime = $derived(
    variant === 'progress'
      ? formatTime(activeRatio * player.duration)
      : player.formattedPosition
  );

  let fillRatio = $derived(activeRatio);
  let volumePercent = $derived(Math.round(liveVolume * 100));

  function setVolumeUi(next: number) {
    const v = Math.max(0, Math.min(1, next));
    liveVolume = v;
    player.setVolume(v);
  }

  /**
   * Show % and restart CSS auto-hide (unless sticky drag).
   * Remount via pctKey so animation always restarts cleanly.
   */
  function flashPct() {
    if (variant !== 'volume') return;
    showPct = true;
    pctKey += 1;
  }

  /** Ignore animationend from a replaced (stale) bubble after pctKey bump. */
  function onPctAnimationEnd(forKey: number, e: AnimationEvent) {
    if (e.target !== e.currentTarget) return;
    if (forKey !== pctKey) return;
    if (isDragging) return;
    showPct = false;
  }

  // Sync from store (keyboard / remote) — never opens %.
  $effect(() => {
    if (variant !== 'volume' || isDragging) return;
    const remote = player.volume;
    if (Math.abs(remote - liveVolume) > 0.0005) {
      liveVolume = remote;
    }
  });

  $effect(() => {
    if (pendingValue === null) return;
    const actual = variant === 'progress' ? player.progress : player.volume;
    if (Math.abs(actual - pendingValue) < 0.01) {
      pendingValue = null;
    }
  });

  // Window wheel while hovered — works in fullscreen where element wheel is unreliable.
  $effect(() => {
    if (variant !== 'volume') return;

    const onWheel = (e: WheelEvent) => {
      if (!hovered && !isDragging) return;
      e.preventDefault();
      e.stopPropagation();

      const dir = e.deltaY > 0 ? -1 : 1;
      const intensity = Math.min(3, Math.max(1, Math.round(Math.abs(e.deltaY) / 40)));
      setVolumeUi(liveVolume + dir * VOLUME_WHEEL_STEP * intensity);
      flashPct();
    };

    window.addEventListener('wheel', onWheel, { passive: false, capture: true });
    return () => {
      window.removeEventListener('wheel', onWheel, { capture: true } as EventListenerOptions);
    };
  });

  function endDragFromWindow() {
    if (!isDragging || variant !== 'volume') return;
    finishVolumeDrag();
  }

  function finishVolumeDrag() {
    if (!isDragging) return;
    isDragging = false;
    window.removeEventListener('pointerup', endDragFromWindow);
    window.removeEventListener('pointercancel', endDragFromWindow);
    setVolumeUi(dragValue);
    // Remount bubble with fade-out animation
    flashPct();
  }

  onDestroy(() => {
    window.removeEventListener('pointerup', endDragFromWindow);
    window.removeEventListener('pointercancel', endDragFromWindow);
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

  function handleTrackPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;

    isDragging = true;
    updateDragValue(e.clientX);

    if (variant === 'volume') {
      setVolumeUi(dragValue);
      // Sticky while dragging: show without relying on fade-out yet
      showPct = true;
      pctKey += 1;
      window.addEventListener('pointerup', endDragFromWindow);
      window.addEventListener('pointercancel', endDragFromWindow);
    }

    trackEl?.setPointerCapture(e.pointerId);
    e.preventDefault();
  }

  function handleTrackPointerMove(e: PointerEvent) {
    if (!isDragging) return;
    updateDragValue(e.clientX);
    if (variant === 'volume') {
      setVolumeUi(dragValue);
      showPct = true;
    }
  }

  function handleTrackPointerUp(e: PointerEvent) {
    if (!isDragging) return;

    updateDragValue(e.clientX);

    if (variant === 'progress') {
      pendingValue = dragValue;
      void player.seek(dragValue * player.duration);
      isDragging = false;
    } else {
      finishVolumeDrag();
    }

    if (trackEl?.hasPointerCapture(e.pointerId)) {
      trackEl.releasePointerCapture(e.pointerId);
    }
  }

  function handleTrackPointerCancel(e: PointerEvent) {
    if (!isDragging) return;
    if (variant === 'volume') {
      finishVolumeDrag();
    } else {
      isDragging = false;
    }
    if (trackEl?.hasPointerCapture(e.pointerId)) {
      trackEl.releasePointerCapture(e.pointerId);
    }
  }

  function toggleMute() {
    if (isMuted) {
      setVolumeUi(previousVolume || 0.8);
    } else {
      previousVolume = liveVolume || player.volume;
      setVolumeUi(0);
    }
    flashPct();
  }

  function handleProgressWheel(e: WheelEvent) {
    if (variant !== 'progress') return;
    e.preventDefault();
    e.stopPropagation();
    if (!player.hasTrack || player.duration <= 0) return;
    const step = e.deltaY > 0 ? -SEEK_STEP_SEC : SEEK_STEP_SEC;
    const next = Math.max(0, Math.min(player.duration, player.position + step));
    void player.seek(next);
  }

  function progressWheelAction(node: HTMLElement) {
    if (variant !== 'progress') return {};
    const onWheel = (e: WheelEvent) => handleProgressWheel(e);
    node.addEventListener('wheel', onWheel, { passive: false });
    return {
      destroy() {
        node.removeEventListener('wheel', onWheel);
      },
    };
  }

  function onVolumeEnter() {
    hovered = true;
  }

  function onVolumeLeave() {
    hovered = false;
    // If not dragging, let current fade finish / force hide if stuck solid
    if (!isDragging && showPct) {
      flashPct();
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="media-slider {variant}"
  use:progressWheelAction
  onpointerenter={variant === 'volume' ? onVolumeEnter : undefined}
  onpointerleave={variant === 'volume' ? onVolumeLeave : undefined}
>
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

    <div class="volume-track-wrap">
      {#if showPct}
        {#key pctKey}
          <span
            class="volume-pct"
            class:sticky={isDragging}
            style="left: {liveVolume * 100}%"
            onanimationend={(e) => onPctAnimationEnd(pctKey, e)}
            aria-hidden="true"
          >
            {volumePercent}%
          </span>
        {/key}
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
        aria-label="Volume"
        aria-valuemin={0}
        aria-valuemax={1}
        aria-valuenow={liveVolume}
      >
        <div class="slider-fill" style="width: {liveVolume * 100}%">
          <div class="slider-thumb" class:active={isDragging}></div>
        </div>
      </div>
    </div>
  {:else}
    <span class="slider-time current">{displayTime}</span>

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
      aria-label="Seek position"
      aria-valuemin={0}
      aria-valuemax={player.duration}
      aria-valuenow={player.position}
    >
      <div class="slider-fill" style="width: {fillRatio * 100}%">
        <div class="slider-thumb" class:active={isDragging}></div>
      </div>
    </div>

    <span class="slider-time duration">{player.formattedDuration}</span>
  {/if}
</div>

<style>
  @import './MediaSlider.css';
</style>
