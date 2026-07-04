<script lang="ts">
  import { getCoverSrc } from '$lib/coverCache';
  import type { MusicFile } from '$lib/stores/player.svelte';


  interface Props {
    track: MusicFile | null;
  }

  let { track }: Props = $props();

  let src = $derived(getCoverSrc(track?.cover_path));
</script>

<div class="track-cover">
  {#if src}
    <img {src} alt="" loading="lazy" decoding="async" />
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