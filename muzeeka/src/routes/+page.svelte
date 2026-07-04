<script lang="ts">
  import '../app.css';
  import PlaylistSidebar from '$lib/components/PlaylistSidebar.svelte';
  import TrackList from '$lib/components/TrackList.svelte';
  import TransportBar from '$lib/components/TransportBar.svelte';
  import DragDropHandler from '$lib/components/DragDropHandler.svelte';
  import WindowControls from '$lib/components/WindowControls.svelte';
  import { getPlayerStore } from '$lib/stores/player.svelte';

  const player = getPlayerStore();
  let searchQuery = $state('');
  let lastSearchPlaylistId = $state<string | null>(null);

  $effect(() => {
    const playlistId = player.activePlaylistId;
    if (playlistId !== lastSearchPlaylistId) {
      searchQuery = '';
      lastSearchPlaylistId = playlistId;
    }
  });

  function isTypingTarget(target: EventTarget | null): boolean {
    return target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement;
  }

  function handleMouseDown(e: MouseEvent) {
    if (e.button !== 3 && e.button !== 4) return;
    if (isTypingTarget(e.target)) return;

    e.preventDefault();

    if (e.button === 3) {
      void player.prevTrack();
    } else {
      void player.nextTrack();
    }
  }

  $effect(() => {
    function handleWheel(e: WheelEvent) {
      if (!e.altKey) return;

      e.preventDefault();

      const step = 0.05;
      const delta = e.deltaY < 0 ? step : -step;
      void player.setVolume(Math.max(0, Math.min(1, player.volume + delta)));
    }

    window.addEventListener('wheel', handleWheel, { passive: false });
    return () => window.removeEventListener('wheel', handleWheel);
  });

  // Keyboard shortcuts
  function handleKeydown(e: KeyboardEvent) {
    if (isTypingTarget(e.target)) return;

    switch (e.code) {
      case 'Space':
        e.preventDefault();
        player.togglePlayPause();
        break;
      case 'ArrowRight':
        if (e.shiftKey) {
          player.nextTrack();
        } else {
          player.seek(Math.min(player.position + 5, player.duration));
        }
        break;
      case 'ArrowLeft':
        if (e.shiftKey) {
          player.prevTrack();
        } else {
          player.seek(Math.max(player.position - 5, 0));
        }
        break;
      case 'ArrowUp':
        e.preventDefault();
        player.setVolume(Math.min(player.volume + 0.05, 1));
        break;
      case 'ArrowDown':
        e.preventDefault();
        player.setVolume(Math.max(player.volume - 0.05, 0));
        break;
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} onmousedown={handleMouseDown} />

<div class="app-layout">
  <header class="app-header glass">
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

    <div class="app-header-spacer" data-tauri-drag-region></div>
    <WindowControls />
  </header>

  <div class="app-body">
    <PlaylistSidebar />
    <TrackList bind:searchQuery />
  </div>

  <TransportBar />
  <DragDropHandler />
</div>

<style>
  @import './+page.css';
</style>
