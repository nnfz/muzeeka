<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import type { MusicFile } from '$lib/stores/player.svelte';


  interface Props {
    track: MusicFile | null;
  }

  let { track }: Props = $props();

  const coverCache = new Map<string, string>();

  let src = $state<string | null>(null);

  async function resolveCover(path: string): Promise<string | null> {
    const cached = coverCache.get(path);
    if (cached) return cached;

    const url = await invoke<string | null>('library_cover_data_url', { path });
    if (url) coverCache.set(path, url);
    return url;
  }

  $effect(() => {
    const path = track?.cover_path?.trim();
    if (!path) {
      src = null;
      return;
    }

    const cached = coverCache.get(path);
    if (cached) {
      src = cached;
      return;
    }

    let cancelled = false;
    src = null;

    resolveCover(path)
      .then((url) => {
        if (!cancelled) src = url;
      })
      .catch(() => {
        if (!cancelled) src = null;
      });

    return () => {
      cancelled = true;
    };
  });
</script>

<div class="track-cover">
  {#if src}
    <img {src} alt="" loading="lazy" />
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
