<script lang="ts">
  import { onDestroy } from 'svelte';
  import { getPlayerStore } from '$lib/stores/player.svelte';

  interface Props {
    variant: 'progress' | 'volume';
  }

  let { variant }: Props = $props();

  const player = getPlayerStore();
  const SEEK_STEP_SEC = 5;
  const VOLUME_WHEEL_STEP = 0.04;

  let isDragging = $state(false);
  let dragValue = $state(0);
  let pendingValue = $state<number | null>(null);
  let trackEl = $state<HTMLDivElement | null>(null);
  let previousVolume = $state(0.8);
  let liveVolume = $state(player.volume);

  let showPct = $state(false);
  let pctKey = $state(0);
  let hovered = $state(false);

  let isMuted = $derived(liveVolume === 0);
  let showMed = $derived(!isMuted && liveVolume > 0.33);
  let showMax = $derived(!isMuted && liveVolume > 0.66);

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
      ? formatTime(activeRatio * player.displayDuration)
      : player.formattedPosition
  );

  let fillRatio = $derived(activeRatio);
  let volumePercent = $derived(Math.round(liveVolume * 100));

  function setVolumeUi(next: number) {
    const v = Math.max(0, Math.min(1, next));
    liveVolume = v;
    player.setVolume(v);
  }

  function flashPct() {
    if (variant !== 'volume') return;
    showPct = true;
    pctKey += 1;
  }

  function onPctAnimationEnd(forKey: number, e: AnimationEvent) {
    if (e.target !== e.currentTarget) return;
    if (forKey !== pctKey) return;
    if (isDragging) return;
    showPct = false;
  }

  $effect(() => {
    if (variant !== 'volume' || isDragging) return;
    const remote = player.volume;
    if (Math.abs(remote - liveVolume) > 0.0005) {
      liveVolume = remote;
      flashPct();
    }
  });

  $effect(() => {
    if (pendingValue === null) return;
    const actual = variant === 'progress' ? player.progress : player.volume;
    if (Math.abs(actual - pendingValue) < 0.01) {
      pendingValue = null;
    }
  });

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
      void player.seek(dragValue * player.displayDuration);
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
    if (!player.hasTrack || player.displayDuration <= 0) return;
    const step = e.deltaY > 0 ? -SEEK_STEP_SEC : SEEK_STEP_SEC;
    const next = Math.max(
      0,
      Math.min(player.displayDuration, player.displayPosition + step)
    );
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
      <span class="volume-icon-stack" aria-hidden="true">
        <span class="volume-icon-base" style:--control-icon={"url('/icons/volmin.svg')"}></span>
        <span
          class="volume-icon-append"
          class:visible={isMuted}
          style:--control-icon={"url('/icons/mute.svg')"}
        ></span>
        <span
          class="volume-icon-append slide"
          class:visible={showMed}
          style:--control-icon={"url('/icons/volmed.svg')"}
        ></span>
        <span
          class="volume-icon-append volume-icon-append--second slide"
          class:visible={showMax}
          style:--control-icon={"url('/icons/volmax.svg')"}
        ></span>
      </span>
    </button>

    <div class="volume-track-wrap">
      {#if showPct}
        {#key pctKey}
          <span
            class="volume-pct"
            class:sticky={isDragging}
            style="--fill: {liveVolume}"
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
        style="--fill: {liveVolume}"
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
        <div class="slider-fill"></div>
        <div class="slider-thumb-rail">
          <div class="slider-thumb" class:active={isDragging}></div>
        </div>
      </div>
    </div>
  {:else if player.currentIsStream}
    <!-- Radio / web streams have no finite length: no scrubbing, just a live marker. -->
    <span class="slider-time current">{player.formattedPosition}</span>

    <div class="slider-track is-live" aria-label="Live stream" role="img">
      <div class="slider-fill"></div>
    </div>

    <span class="slider-time duration live-badge">LIVE</span>
  {:else}
    <span class="slider-time current">{displayTime}</span>

    <div
      class="slider-track"
      class:is-dragging={isDragging}
      class:is-pending={pendingValue !== null}
      style="--fill: {fillRatio}"
      bind:this={trackEl}
      onpointerdown={handleTrackPointerDown}
      onpointermove={handleTrackPointerMove}
      onpointerup={handleTrackPointerUp}
      onpointercancel={handleTrackPointerCancel}
      role="slider"
      tabindex="0"
      aria-label="Seek position"
      aria-valuemin={0}
      aria-valuemax={player.displayDuration}
      aria-valuenow={player.displayPosition}
    >
      <div class="slider-fill"></div>
      <div class="slider-thumb-rail">
        <div class="slider-thumb" class:active={isDragging}></div>
      </div>
    </div>

    <span class="slider-time duration">{player.formattedDuration}</span>
  {/if}
</div>

<style>
  @import './MediaSlider.css';
</style>