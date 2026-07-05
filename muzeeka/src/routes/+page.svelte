<script lang="ts">
  import '../app.css';
  import PlaylistSidebar from '$lib/components/PlaylistSidebar.svelte';
  import TrackList from '$lib/components/TrackList.svelte';
  import TransportBar from '$lib/components/TransportBar.svelte';
  import DragDropHandler from '$lib/components/DragDropHandler.svelte';
  import WindowControls from '$lib/components/WindowControls.svelte';
  import SettingsWindow from '$lib/components/SettingsWindow.svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getPlayerStore } from '$lib/stores/player.svelte';
  import { createSettingsStore, setSettingsStore } from '$lib/stores/settings.svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
  // Detect which window we are in. This is stable per webview instance.
  const currentWin = getCurrentWindow();
  const isSettingsWindow = currentWin.label === 'settings';

  // Avoid full player bootstrap (playlists, listeners, heavy init) in the settings window.
  // Settings window only needs to be able to apply EQ settings.
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
  } else {
    player = getPlayerStore();
    ensurePlayerReady = () => player!.ensureInit();
  }

  const settings = createSettingsStore(ensurePlayerReady);
  setSettingsStore(settings);
  let searchQuery = $state('');

  async function openSettingsWindow() {
    const label = 'settings';
    try {
      console.log('[settings] openSettingsWindow called');

      // Try to reuse if still open
      const existing = await WebviewWindow.getByLabel(label);
      if (existing) {
        try {
          await existing.show();
          await existing.setFocus();
          console.log('[settings] reused existing window');
          return;
        } catch (e: any) {
          const msg = String(e?.message || e).toLowerCase();
          if (!msg.includes('not found')) {
            console.log('[settings] could not reuse existing:', e);
          }
          // otherwise it was closed -> fallthrough to create fresh
        }
      }

      // Build correct URL for dev vs prod so the webview actually loads the app
      const url = import.meta.env.DEV ? 'http://localhost:1420/' : 'index.html';

      const win = new WebviewWindow(label, {
        url,
        title: 'Settings',
        width: 960,
        height: 620,
        minWidth: 760,
        minHeight: 480,
        decorations: false,
        resizable: true,
      });

      // Log creation errors (permission, etc.)
      win.once('tauri://error', (e: any) => {
        console.error('[settings] tauri://error:', e?.message || e, e);
      });

      // Fast path: if Tauri emits created event, show immediately
      win.once('tauri://created', async () => {
        try {
          await win.show();
          await win.setFocus();
          console.log('[settings] settings window shown via created event');
        } catch (e) {
          // fall back to retry below
        }
      });

      // The WebviewWindow handle is returned immediately, but the actual
      // window creation in the backend is async. Calling show() too early
      // often throws "window not found". We use a short delay + retry so
      // we don't spam the console with benign errors (the window appears anyway).
      const showWithRetry = async (attempt = 0) => {
        try {
          await win.show();
          await win.setFocus();
          if (attempt === 0) {
            console.log('[settings] new settings window shown and focused');
          } else {
            console.log('[settings] settings window shown after retry');
          }
        } catch (e: any) {
          const msg = String(e?.message || e).toLowerCase();
          if (attempt < 5 && msg.includes('not found')) {
            setTimeout(() => showWithRetry(attempt + 1), 35);
          } else if (attempt < 3) {
            setTimeout(() => showWithRetry(attempt + 1), 50);
          } else {
            // After several retries we only warn (window is usually already visible)
            console.warn('[settings] show/focus still failing after retries:', e);
          }
        }
      };

      setTimeout(() => showWithRetry(), 25);
    } catch (err: any) {
      const msg = String(err?.message || err);
      console.error('[settings] Failed to open settings window:', err);

      // Don't spam alert for transient "not found" (window usually appears anyway)
      if (typeof alert === 'function' && !msg.toLowerCase().includes('not found')) {
        alert('Не удалось открыть окно настроек.\n' + msg);
      }
    }
  }

  function isTypingTarget(target: EventTarget | null): boolean {
    return target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement;
  }

  function handleMouseDown(e: MouseEvent) {
    if (isSettingsWindow || !player) return;
    if (e.button !== 3 && e.button !== 4) return;
    if (isTypingTarget(e.target)) return;

    e.preventDefault();

    if (e.button === 3) {
      void player.prevTrack();
    } else {
      void player.nextTrack();
    }
  }

  // Keyboard shortcuts (main window only)
  function handleKeydown(e: KeyboardEvent) {
    if (isSettingsWindow || !player) return;
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
    if (isSettingsWindow) return;

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
{:else}
  <div class="app-layout">
    <header class="app-header glass">
      {#if player!.hasAnyTracks}
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
