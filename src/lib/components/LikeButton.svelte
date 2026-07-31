<script lang="ts">
  import { getPlayerStore } from '$lib/stores/player.svelte';

  // Замените `any` на ваш тип файла, если он у вас экспортируется, например: import type { TrackFile } from '$lib/types';
  interface Props {
    file: any; 
    class?: string;
  }

  let { file, class: className = '' }: Props = $props();

  const player = getPlayerStore();
</script>

<button
  class="like-btn {className}"
  onclick={() => player.toggleLike(file)}
  title={player.isLiked(file) ? 'Remove from Liked' : 'Add to Liked'}
  aria-label={player.isLiked(file) ? 'Unlike track' : 'Like track'}
>
  <span class="like-icon-stack" aria-hidden="true">
    <span
      class="control-icon like-icon like-icon-layer"
      class:visible={!player.isLiked(file)}
      style:--control-icon={"url('/icons/heart.svg')"}
    ></span>
    <span
      class="control-icon like-icon like-icon-layer"
      class:visible={player.isLiked(file)}
      style:--control-icon={"url('/icons/heartfilled.svg')"}
    ></span>
  </span>
</button>

<style>
    @import './LikeButton.css';
</style>