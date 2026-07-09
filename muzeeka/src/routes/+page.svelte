<script lang="ts">
  import '../app.css';
  import PlaylistSidebar from '$lib/components/PlaylistSidebar.svelte';
  import TrackList from '$lib/components/TrackList.svelte';
  import TransportBar from '$lib/components/TransportBar.svelte';
  import DragDropHandler from '$lib/components/DragDropHandler.svelte';
  import WindowControls from '$lib/components/WindowControls.svelte';
  import SettingsWindow from '$lib/components/SettingsWindow.svelte';
  import DownloadWindow from '$lib/components/DownloadWindow.svelte';
  import { openDownloadWindow, precreateDownloadWindow } from '$lib/stores/download.svelte';
  import { looksLikeMediaUrl } from '$lib/urlUtils';
  import { invoke } from '@tauri-apps/api/core';
  import { getPlayerStore } from '$lib/stores/player.svelte';
  import { createSettingsStore, setSettingsStore } from '$lib/stores/settings.svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { WebviewWindow } from '@tauri-apps/api/webviewWindow';

  const currentWin = getCurrentWindow();
  const isSettingsWindow = currentWin.label === 'settings';
  const isDownloadWindow = currentWin.label === 'download';
  const isSecondaryWindow = isSettingsWindow || isDownloadWindow;

  let player = $state<ReturnType<typeof getPlayerStore> | null>(null);
  let ensurePlayerReady: () => Promise<void>;

  if (isSettingsWindow) {
    ensurePlayerReady = async () => {
      try {
        await invoke('player_init');
      } catch {
        // Player may already be initialized by main window
      }
    };
  } else if (isDownloadWindow) {
    ensurePlayerReady = async () => {};
  } else {
    player = getPlayerStore();
    ensurePlayerReady = () => player!.ensureInit();
  }

  const settings = createSettingsStore(ensurePlayerReady);
  setSettingsStore(settings);
  let searchQuery = $state('');

  if (!isSecondaryWindow) {
    const precreateSettingsWindow = async () => {
      try {
        const label = 'settings';
        const existing = await WebviewWindow.getByLabel(label);
        if (existing) return;

        const url = import.meta.env.DEV ? 'http://localhost:1420/' : 'index.html';

        new WebviewWindow(label, {
          url,
          title: 'Settings',
          width: 960,
          height: 620,
          minWidth: 760,
          minHeight: 480,
          decorations: false,
          resizable: true,
          visible: false,
          theme: 'dark',
        });
      } catch {
        // non-fatal
      }
    };

    queueMicrotask(precreateSettingsWindow);
    precreateDownloadWindow();
  }

  async function openSettingsWindow() {
    const label = 'settings';
    try {
      let win = await WebviewWindow.getByLabel(label);

      if (win) {
        await win.show();
        await win.setFocus();
        return;
      }

      const url = import.meta.env.DEV ? 'http://localhost:1420/' : 'index.html';

      win = new WebviewWindow(label, {
        url,
        title: 'Settings',
        width: 960,
        height: 620,
        minWidth: 760,
        minHeight: 480,
        decorations: false,
        resizable: true,
        visible: false,
        theme: 'dark',
      });

      win.once('tauri://error', (e: any) => {
        console.error('[settings] creation error:', e?.message || e);
      });

      const showNow = async () => {
        try {
          await win!.show();
          await win!.setFocus();
        } catch {
          setTimeout(async () => {
            try { await win!.show(); await win!.setFocus(); } catch {}
          }, 60);
        }
      };

      win.once('tauri://created', showNow);
      setTimeout(showNow, 120);
    } catch (err: any) {
      console.error('[settings] Failed to open settings window:', err);
    }
  }

  function isTypingTarget(target: EventTarget | null): boolean {
    return target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement;
  }

  function handleMouseDown(e: MouseEvent) {
    if (isSecondaryWindow || !player) return;
    if (e.button !== 3 && e.button !== 4) return;
    if (isTypingTarget(e.target)) return;

    e.preventDefault();

    if (e.button === 3) {
      void player.prevTrack();
    } else {
      void player.nextTrack();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (isSecondaryWindow || !player) return;
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

  $effect(() => {
    if (isSecondaryWindow) return;

    function handleWheel(e: WheelEvent) {
      if (!e.altKey || !player) return;

      e.preventDefault();

      const step = 0.05;
      const delta = e.deltaY < 0 ? step : -step;
      void player.setVolume(Math.max(0, Math.min(1, player.volume + delta)));
    }

    window.addEventListener('wheel', handleWheel, { passive: false });
    return () => window.removeEventListener('wheel', handleWheel);
  });
</script>

<svelte:window onkeydown={handleKeydown} onmousedown={handleMouseDown} />

{#if isSettingsWindow}
  <SettingsWindow />
{:else if isDownloadWindow}
  <DownloadWindow />
{:else}
  <div class="app-layout">
    <header class="app-header glass">
      <div class="search-container">
        <svg class="search-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <circle cx="11" cy="11" r="8"/>
          <line x1="21" y1="21" x2="16.65" y2="16.65"/>
        </svg>
        <input
          type="text"
          class="search-input"
          placeholder="Search tracks or paste URL…"
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

      <button
        type="button"
        class="header-btn"
        onclick={() => openDownloadWindow(looksLikeMediaUrl(searchQuery) ? searchQuery : '')}
        aria-label="Download from URL"
        title="Download from URL"
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
          <polyline points="7 10 12 15 17 10"/>
          <line x1="12" y1="15" x2="12" y2="3"/>
        </svg>
      </button>

      <div class="app-header-spacer" data-tauri-drag-region></div>

      <button
        type="button"
        class="header-btn"
        onclick={() => openSettingsWindow()}
        aria-label="Settings"
        title="Settings"
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <circle cx="12" cy="12" r="3"/>
          <path d="M12 1v2M12 21v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M1 12h2M21 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42"/>
        </svg>
      </button>

      <WindowControls />
    </header>

    <div class="app-body">
      <PlaylistSidebar />
      <TrackList bind:searchQuery />
    </div>

    <TransportBar />
    <DragDropHandler />
  </div>
{/if}

<style>
  @import './+page.css';
</style>