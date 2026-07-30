<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import { getCoverSrc } from '$lib/coverCache';
  import { COVER_PLACEHOLDER_SRC } from '$lib/coverPlaceholder';

  interface DragFloatPayload {
    title: string;
    artist: string;
    coverPath?: string | null;
    cover_path?: string | null;
    count: number;
    isCopy?: boolean;
    is_copy?: boolean;
    rotate?: number;
  }

  let title = $state('Track');
  let artist = $state('');
  let count = $state(1);
  let isCopy = $state(false);
  let rotate = $state(-2.5);
  let coverSrc = $state(COVER_PLACEHOLDER_SRC);
  let coverFailed = $state(false);

  function applyPayload(p: DragFloatPayload) {
    title = p.title || 'Track';
    artist = p.artist || '';
    count = Math.max(1, Number(p.count) || 1);
    isCopy = !!(p.isCopy ?? p.is_copy);
    rotate = typeof p.rotate === 'number' ? p.rotate : -2.5;
    const path = p.coverPath ?? p.cover_path ?? null;
    coverFailed = false;
    const resolved = getCoverSrc(path);
    coverSrc = resolved || COVER_PLACEHOLDER_SRC;
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;

    void (async () => {
      try {
        const last = await invoke<DragFloatPayload | null>('drag_float_get_payload');
        if (last) applyPayload(last);
      } catch {
        /* window may open before command is hot */
      }

      unlisten = await listen<DragFloatPayload>('drag-float:update', (event) => {
        applyPayload(event.payload);
      });
    })();

    return () => {
      unlisten?.();
    };
  });
</script>

<div class="df-root" style="transform: rotate({rotate.toFixed(2)}deg)">
  <div class="df-card" class:is-copy={isCopy}>
    <div class="df-cover">
      <img
        src={coverFailed ? COVER_PLACEHOLDER_SRC : coverSrc}
        alt=""
        draggable="false"
        onerror={() => {
          coverFailed = true;
        }}
      />
      {#if count > 1}
        <span class="df-badge">{count}</span>
      {/if}
    </div>
    <div class="df-meta">
      <span class="df-title">{title}</span>
      {#if artist}
        <span class="df-artist">{artist}</span>
      {/if}
      {#if isCopy}
        <span class="df-mode">Copy</span>
      {/if}
    </div>
  </div>
</div>

<style>
  :global(html),
  :global(body) {
    margin: 0 !important;
    padding: 0 !important;
    background: transparent !important;
    overflow: hidden !important;
    width: 100%;
    height: 100%;
  }

  .df-root {
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: flex-start;
    padding: 6px;
    box-sizing: border-box;
    background: transparent;
    pointer-events: none;
    transform-origin: 20% 80%;
    font-family: var(--font-family, system-ui, sans-serif);
  }

  .df-card {
    display: flex;
    align-items: center;
    gap: 10px;
    max-width: 260px;
    padding: 8px 12px 8px 8px;
    border-radius: 12px;
    background: rgba(22, 22, 30, 0.92);
    border: 1px solid rgba(255, 255, 255, 0.1);
    box-shadow:
      0 12px 40px rgba(0, 0, 0, 0.5),
      0 0 0 1px rgba(255, 255, 255, 0.04) inset;
    backdrop-filter: blur(14px);
    -webkit-backdrop-filter: blur(14px);
  }

  .df-card.is-copy {
    border-color: color-mix(in srgb, #7c6af0 45%, rgba(255, 255, 255, 0.1));
  }

  .df-cover {
    position: relative;
    width: 44px;
    height: 44px;
    flex-shrink: 0;
    border-radius: 8px;
    overflow: hidden;
    background: rgba(255, 255, 255, 0.04);
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.35);
  }

  .df-cover img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .df-badge {
    position: absolute;
    right: -6px;
    top: -6px;
    min-width: 20px;
    height: 20px;
    padding: 0 6px;
    border-radius: 999px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 700;
    color: #fff;
    background: #7c6af0;
    box-shadow: 0 2px 8px rgba(124, 106, 240, 0.5);
  }

  .df-meta {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .df-title {
    font-size: 12.5px;
    font-weight: 600;
    color: #f2f2f5;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 170px;
  }

  .df-artist {
    font-size: 11px;
    color: rgba(242, 242, 245, 0.55);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 170px;
  }

  .df-mode {
    margin-top: 2px;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: #7c6af0;
  }
</style>
