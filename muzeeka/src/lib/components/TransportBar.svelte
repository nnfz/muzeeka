<script lang="ts">
  import { getPlayerStore, trackDisplayArtist } from '$lib/stores/player.svelte';
  import MediaSlider from './MediaSlider.svelte';
  import TrackCover from './TrackCover.svelte';


  const player = getPlayerStore();

  let currentTrack = $derived(
    player.tracks.find((t) => t.path === player.currentFile) ?? null
  );
</script>

<div class="transport-bar glass">
  <div class="transport-content">
    <div class="transport-info">
      {#if player.hasTrack}
        <TrackCover track={currentTrack} />
        <div class="now-playing-text">
          <span class="np-title">{player.currentFileName ?? ''}</span>
          {#if currentTrack}
            <span class="np-artist">{trackDisplayArtist(currentTrack)}</span>
          {/if}
        </div>
      {/if}
    </div>

    <div class="transport-controls">
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
        disabled={!player.hasTracks}
        aria-label={player.isPlaying ? 'Pause' : 'Play'}
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

    </div>

    <div class="transport-right">
      <MediaSlider variant="volume" />
    </div>
  </div>
  <div class="transport-progress">
    <MediaSlider variant="progress" />
  </div>

</div>

<style>
  @import './TransportBar.css';
</style>

