<script lang="ts">
  import {
    getPlayerStore,
    trackDisplayArtist,
    trackDisplayTitle,
  } from '$lib/stores/player.svelte';
  import {
    describeSearchQuery,
    isTrackSearch,
    searchTracks,
  } from '$lib/searchUtils';
  import TrackCover from './TrackCover.svelte';

  interface Props {
    query?: string;
  }

  let { query = '' }: Props = $props();

  const player = getPlayerStore();

  let results = $derived(
    isTrackSearch(query) ? searchTracks(player.playlists, query) : []
  );

  let description = $derived(describeSearchQuery(query, player.playlists));

  function formatDuration(seconds: number | null | undefined): string {
    if (seconds == null || !Number.isFinite(seconds) || seconds <= 0) return '—';
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, '0')}`;
  }

  function playTrack(path: string) {
    void player.play(path);
  }
</script>

{#if isTrackSearch(query)}
  <section class="search-dropdown-panel search-results-dropdown" aria-label="Search results">
    <div class="search-dropdown-header">
      <span class="search-dropdown-title">
        {results.length} result{results.length !== 1 ? 's' : ''}
      </span>
      <span class="search-dropdown-meta">{description}</span>
      <span class="search-dropdown-hint">@artist · @title · @p=playlist</span>
    </div>

    {#if results.length === 0}
      <div class="search-dropdown-empty">No matches — try @artist, @title or @p=playlist</div>
    {:else}
      <div class="search-dropdown-list">
        {#each results as item (item.track.path + item.playlistId)}
          {@const track = item.track}
          {@const isActive = track.path === player.currentFile}
          <button
            type="button"
            class="search-dropdown-row"
            class:active={isActive}
            onclick={() => playTrack(track.path)}
            title={`${trackDisplayTitle(track)} — ${trackDisplayArtist(track)} (${item.playlistName})`}
          >
            <TrackCover track={track} />
            <span class="search-dropdown-info">
              <span class="search-dropdown-primary">{trackDisplayTitle(track)}</span>
              <span class="search-dropdown-secondary">
                {trackDisplayArtist(track)} · {item.playlistName}
              </span>
            </span>
            <span class="search-dropdown-aside">{formatDuration(track.duration_secs)}</span>
          </button>
        {/each}
      </div>
    {/if}
  </section>
{/if}

<style>
  @import './searchDropdown.css';
  @import './SearchResults.css';
</style>