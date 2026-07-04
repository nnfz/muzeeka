<script lang="ts">
  import {
    getPlayerStore,
    trackDisplayArtist,
    trackDisplayTitle,
    trackSearchText,
    type MusicFile,
  } from '$lib/stores/player.svelte';
  import { open } from '@tauri-apps/plugin-dialog';
  import TrackCover from './TrackCover.svelte';
  import WindowControls from './WindowControls.svelte';


  const player = getPlayerStore();

  let searchQuery = $state('');

  let filteredTracks = $derived(
    searchQuery.trim()
      ? player.tracks.filter((t) =>
          trackSearchText(t).includes(searchQuery.toLowerCase())
        )
      : player.tracks
  );

  async function addTracksFromFolder() {
    const selected = await open({ directory: true });
    if (selected) {
      await player.addFolderToActivePlaylist(selected as string);
    }
  }

  function handleTrackClick(track: MusicFile) {
    player.play(track.path);
  }

  function formatDuration(seconds: number | null | undefined): string {
    if (seconds == null || !Number.isFinite(seconds) || seconds <= 0) return '—';
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, '0')}`;
  }
</script>

<section class="track-panel">
  <div class="panel-toolbar">
    {#if player.activePlaylist && player.hasTracks}
      <div class="search-container">
        <svg class="search-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <circle cx="11" cy="11" r="8"/>
          <line x1="21" y1="21" x2="16.65" y2="16.65"/>
        </svg>
        <input
          type="text"
          class="search-input"
          placeholder="Search tracks..."
          bind:value={searchQuery}
        />
        {#if searchQuery}
          <button class="search-clear" onclick={() => searchQuery = ''} aria-label="Clear search">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <line x1="18" y1="6" x2="6" y2="18"/>
              <line x1="6" y1="6" x2="18" y2="18"/>
            </svg>
          </button>
        {/if}
      </div>
    {/if}

    <div class="toolbar-spacer" data-tauri-drag-region></div>
    <WindowControls />
  </div>

  <div class="track-list">
    {#if !player.activePlaylist}
      <div class="empty-state" data-tauri-drag-region>
        <p class="empty-title">Select a playlist</p>
        <p class="empty-hint">Choose a playlist or drop music files here</p>
      </div>
    {:else if !player.hasTracks}
      <div class="empty-state" data-tauri-drag-region>
        <p class="empty-title">Playlist is empty</p>
        <p class="empty-hint">Drop files or folders here</p>
        <button class="empty-btn" onclick={addTracksFromFolder}>
          Add Tracks
        </button>
      </div>
    {:else if filteredTracks.length === 0}
      <div class="empty-state" data-tauri-drag-region>
        <p class="empty-title">No matches</p>
        <p class="empty-hint">Try a different search term</p>
      </div>
    {:else}
      {#each filteredTracks as track, i}
        {@const isActive = track.path === player.currentFile}
        <button
          class="track-item"
          class:active={isActive}
          class:playing={isActive && player.isPlaying}
          onclick={() => handleTrackClick(track)}
          title={`${trackDisplayTitle(track)} — ${trackDisplayArtist(track)}`}
        >
          <div class="track-index">
            {#if isActive && player.isPlaying}
              <div class="mini-eq">
                <span></span><span></span><span></span>
              </div>
            {:else}
              <span class="track-num">{i + 1}</span>
            {/if}
          </div>
          <TrackCover track={track} />
          <div class="track-details">
            <span class="track-name">{trackDisplayTitle(track)}</span>
            <div class="track-meta-row">
              <span class="track-artist">{trackDisplayArtist(track)}</span>
              {#if track.album}
                <span class="track-album">{track.album}</span>
              {/if}
            </div>
          </div>
          <span class="track-duration">{formatDuration(track.duration_secs)}</span>
        </button>
      {/each}
    {/if}
  </div>
</section>

<style>
  @import './TrackList.css';
</style>
