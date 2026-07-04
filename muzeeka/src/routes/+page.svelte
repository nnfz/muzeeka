<script lang="ts">
  import '../app.css';
  import PlaylistSidebar from '$lib/components/PlaylistSidebar.svelte';
  import TrackList from '$lib/components/TrackList.svelte';
  import TransportBar from '$lib/components/TransportBar.svelte';
  import DragDropHandler from '$lib/components/DragDropHandler.svelte';
  import { getPlayerStore } from '$lib/stores/player.svelte';

  const player = getPlayerStore();

  // Keyboard shortcuts
  function handleKeydown(e: KeyboardEvent) {
    // Ignore if user is typing in an input
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;

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

<svelte:window onkeydown={handleKeydown} />

<div class="app-layout">
  <PlaylistSidebar />
  <TrackList />
  <TransportBar />
  <DragDropHandler />
</div>

<style>
  @import './+page.css';
</style>
