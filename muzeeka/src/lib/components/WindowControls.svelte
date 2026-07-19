<script lang="ts">
  import { getCurrentWindow } from '@tauri-apps/api/window';

  const appWindow = getCurrentWindow();

  // Props to control which buttons are shown
  let {
    showMinimize = true,
    showMaximize = true,
    showClose = true,
  }: {
    showMinimize?: boolean;
    showMaximize?: boolean;
    showClose?: boolean;
  } = $props();

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
  {#if showMinimize}
    <button class="win-btn" onclick={minimize} aria-label="Minimize" title="Minimize">
      <span
        class="win-icon"
        style:--win-icon={"url('/icons/minimize.svg')"}
        aria-hidden="true"
      ></span>
    </button>
  {/if}
  {#if showMaximize}
    <button class="win-btn" onclick={toggleMaximize} aria-label={isMaximized ? 'Restore' : 'Maximize'} title={isMaximized ? 'Restore' : 'Maximize'}>
      <span
        class="win-icon"
        style:--win-icon={isMaximized
          ? "url('/icons/revertmaximize.svg')"
          : "url('/icons/maximize.svg')"}
        aria-hidden="true"
      ></span>
    </button>
  {/if}
  {#if showClose}
    <button class="win-btn close" onclick={close} aria-label="Close" title="Close">
      <span
        class="win-icon"
        style:--win-icon={"url('/icons/close.svg')"}
        aria-hidden="true"
      ></span>
    </button>
  {/if}
</div>

<style>
  @import './WindowControls.css';
</style>
