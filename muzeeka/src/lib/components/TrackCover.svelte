<script lang="ts">
  import { getCoverSrc } from '$lib/coverCache';
  import { COVER_PLACEHOLDER_SRC } from '$lib/coverPlaceholder';
  import type { MusicFile } from '$lib/stores/player.svelte';

  interface Props {
    track: MusicFile | null;
  }

  let { track }: Props = $props();

  let failedSrc = $state<string | null>(null);
  let placeholderFailed = $state(false);

  let src = $derived.by(() => {
    const next = getCoverSrc(track?.cover_path);
    return next && next !== failedSrc ? next : null;
  });

  function handleImageError() {
    if (src) failedSrc = src;
  }

  function handlePlaceholderError() {
    placeholderFailed = true;
  }
</script>

<div class="track-cover">
  {#if src}
    <img
      {src}
      alt=""
      loading="lazy"
      decoding="async"
      draggable="false"
      onerror={handleImageError}
      ondragstart={(e) => e.preventDefault()}
    />
  {:else if !placeholderFailed}
    <img
      class="cover-placeholder-img"
      src={COVER_PLACEHOLDER_SRC}
      alt=""
      loading="lazy"
      decoding="async"
      draggable="false"
      onerror={handlePlaceholderError}
      ondragstart={(e) => e.preventDefault()}
    />
  {:else}
    <div class="cover-placeholder" aria-hidden="true">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
        <path d="M12 3v10.55A4 4 0 1 0 14 17V7h4V3h-6z"/>
      </svg>
    </div>
  {/if}
</div>

<style>
  @import './TrackCover.css';
</style>
