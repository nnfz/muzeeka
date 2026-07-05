<script lang="ts">
  import { getPlayerStore, trackDisplayArtist } from '$lib/stores/player.svelte';
  import MediaSlider from './MediaSlider.svelte';
  import TrackCover from './TrackCover.svelte';


  const player = getPlayerStore();
</script>

<div class="transport-bar glass">
  <div class="transport-content">
    <div class="transport-info">
      {#if player.hasTrack}
        <TrackCover track={player.currentTrack} />
        <div class="now-playing-text">
          <span class="np-title">{player.currentFileName ?? ''}</span>
          {#if player.currentTrack}
            <span class="np-artist">{trackDisplayArtist(player.currentTrack)}</span>
          {/if}
        </div>
      {/if}
    </div>

    <div class="transport-controls">
      <button
        class="control-btn mode-btn"
        class:active={player.shuffleEnabled}
        onclick={() => player.toggleShuffle()}
        disabled={!player.hasTracks}
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
        disabled={!player.hasTracks}
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
        disabled={!player.hasTracks}
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

<style>
  @import './TransportBar.css';
</style>

