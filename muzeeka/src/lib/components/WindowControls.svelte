<script lang="ts">
  import { getCurrentWindow } from '@tauri-apps/api/window';


  const appWindow = getCurrentWindow();
  let isMaximized = $state(false);

  $effect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    appWindow.isMaximized().then((value) => {
      if (!disposed) isMaximized = value;
    });

    appWindow.onResized(async () => {
      if (!disposed) isMaximized = await appWindow.isMaximized();
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  });

  async function minimize() {
    await appWindow.minimize();
  }

  async function toggleMaximize() {
    await appWindow.toggleMaximize();
    isMaximized = await appWindow.isMaximized();
  }

  async function close() {
    await appWindow.close();
  }
</script>

<div class="window-controls">
  <button class="win-btn" onclick={minimize} aria-label="Minimize" title="Minimize">
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
      <rect x="1" y="4.5" width="8" height="1" fill="currentColor"/>
    </svg>
  </button>
  <button class="win-btn" onclick={toggleMaximize} aria-label={isMaximized ? 'Restore' : 'Maximize'} title={isMaximized ? 'Restore' : 'Maximize'}>
    {#if isMaximized}
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
        <rect x="2.5" y="0.5" width="6" height="6" fill="none" stroke="currentColor" stroke-width="1"/>
        <rect x="0.5" y="2.5" width="6" height="6" fill="var(--bg-deep)" stroke="currentColor" stroke-width="1"/>
      </svg>
    {:else}
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
        <rect x="1.5" y="1.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1"/>
      </svg>
    {/if}
  </button>
  <button class="win-btn close" onclick={close} aria-label="Close" title="Close">
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
      <path d="M1.5 1.5 8.5 8.5M8.5 1.5 1.5 8.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
    </svg>
  </button>
</div>

<style>
  @import './WindowControls.css';
</style>
