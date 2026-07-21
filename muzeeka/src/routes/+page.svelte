<script lang="ts">
  import '../app.css';
  import PlaylistSidebar from '$lib/components/PlaylistSidebar.svelte';
  import TrackList from '$lib/components/TrackList.svelte';
  import TransportBar from '$lib/components/TransportBar.svelte';
  import DragDropHandler from '$lib/components/DragDropHandler.svelte';
  import WindowControls from '$lib/components/WindowControls.svelte';
  import SettingsWindow from '$lib/components/SettingsWindow.svelte';
  import DownloadWindow from '$lib/components/DownloadWindow.svelte';
  import SearchBar from '$lib/components/SearchBar.svelte';
  import ImportProgressBar from '$lib/components/ImportProgressBar.svelte';
  import { precreateDownloadWindow } from '$lib/stores/download.svelte';
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
  let fullscreenOpen = $state(false);

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

  /** Reclaim document focus after Windows/WebView Alt menu mode steals it. */
  function ensureWebviewFocus() {
    if (typeof document === 'undefined') return;
    if (document.hasFocus()) return;
    try {
      window.focus();
    } catch {
      /* ignore */
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

    // Capture: bare Alt activates Windows/WebView menu accelerators and can
    // leave the document unfocused (animations + hotkeys die) until a click.
    function handleAltKeydown(e: KeyboardEvent) {
      if (e.key !== 'Alt' || e.ctrlKey || e.metaKey || e.shiftKey) return;
      e.preventDefault();
    }

    function handleWheel(e: WheelEvent) {
      if (!e.altKey || !player) return;

      e.preventDefault();

      const step = 0.01;
      const delta = e.deltaY < 0 ? step : -step;
      void player.setVolume(Math.max(0, Math.min(1, player.volume + delta)));

      // Alt+wheel can still drop document focus mid-gesture on WebView2.
      ensureWebviewFocus();
    }

    function handleKeyup(e: KeyboardEvent) {
      if (e.key !== 'Alt' && e.code !== 'AltLeft' && e.code !== 'AltRight') return;
      ensureWebviewFocus();
    }

    window.addEventListener('keydown', handleAltKeydown, { capture: true });
    window.addEventListener('wheel', handleWheel, { passive: false });
    window.addEventListener('keyup', handleKeyup);
    return () => {
      window.removeEventListener('keydown', handleAltKeydown, { capture: true } as EventListenerOptions);
      window.removeEventListener('wheel', handleWheel);
      window.removeEventListener('keyup', handleKeyup);
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} onmousedown={handleMouseDown} />

{#if isSettingsWindow}
  <SettingsWindow />
{:else if isDownloadWindow}
  <DownloadWindow />
{:else}
  <div class="app-layout">
    <ImportProgressBar />
    <header class="app-header glass">
      <SearchBar bind:searchQuery />

      {#if fullscreenOpen}
        <button
          type="button"
          class="header-btn"
          onclick={() => { fullscreenOpen = false; }}
          aria-label="Close fullscreen player"
          title="Close fullscreen"
        >
          <span
            class="header-static-icon"
            style:--header-icon={"url('/icons/closefullscreen.svg')"}
            aria-hidden="true"
          ></span>
        </button>
      {/if}

      <div class="app-header-spacer" data-tauri-drag-region></div>

      <button
        type="button"
        class="header-btn"
        onclick={() => openSettingsWindow()}
        aria-label="Settings"
        title="Settings"
      >
        <span
          class="header-static-icon"
          style:--header-icon={"url('/icons/options.svg')"}
          aria-hidden="true"
        ></span>
      </button>

      <WindowControls />
    </header>

    <div class="app-body">
      <PlaylistSidebar />
      <TrackList />
    </div>

    <TransportBar bind:fullscreenOpen />
    <DragDropHandler />
  </div>
{/if}

<style>
  @import './+page.css';
</style>