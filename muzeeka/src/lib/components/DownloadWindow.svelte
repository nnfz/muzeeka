<script lang="ts">
  import '../../app.css';
  import '../../routes/+page.css';
  import WindowControls from './WindowControls.svelte';
  import { getDownloadStore } from '$lib/stores/download.svelte';
  import { looksLikeMediaUrl } from '$lib/urlUtils';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';

  const download = getDownloadStore();

  let successMsg = $state<string | null>(null);

  let footerStatus = $derived.by(() => {
    if (successMsg) return { text: successMsg, tone: 'success' as const };
    if (download.progress?.status) return { text: download.progress.status, tone: 'neutral' as const };
    if (download.isProbing) return { text: 'Checking…', tone: 'neutral' as const };
    return null;
  });

  let footerPercent = $derived(
    download.progress?.percent != null ? Math.round(download.progress.percent) : null
  );

  if (typeof document !== 'undefined') {
    document.documentElement.style.setProperty('background-color', '#0a0a0f', 'important');
    if (document.body) {
      document.body.style.setProperty('background-color', '#0a0a0f', 'important');
    }
  }

  function formatDuration(secs: number | null | undefined): string {
    if (secs == null || !Number.isFinite(secs) || secs <= 0) return '';
    const mins = Math.floor(secs / 60);
    const s = Math.floor(secs % 60);
    return `${mins}:${s.toString().padStart(2, '0')}`;
  }

  async function handleDownload() {
    successMsg = null;
    const added = await download.download();
    if (added > 0) {
      successMsg = `Added ${added} track${added === 1 ? '' : 's'} to library`;
    }
  }

  function startDrag(e: PointerEvent) {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest('button, input, a, select, textarea')) return;
    void getCurrentWindow().startDragging();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && !download.isDownloading) {
      void download.closeWindow();
    }
    if (e.key === 'Enter' && !download.isDownloading) {
      if (!download.probe && looksLikeMediaUrl(download.url)) {
        void download.probeUrl();
      } else if (download.probe || looksLikeMediaUrl(download.url)) {
        void handleDownload();
      }
    }
  }

  onMount(() => {
    let unlistenOpen: (() => void) | undefined;
    let unlistenClose: (() => void) | undefined;

    void listen<{ url?: string }>('download:open', (event) => {
      successMsg = null;
      download.resetForOpen(event.payload.url ?? '');
    }).then((fn) => {
      unlistenOpen = fn;
    });

    void getCurrentWindow().onCloseRequested((event) => {
      if (download.isDownloading) {
        event.preventDefault();
      }
    }).then((fn) => {
      unlistenClose = fn;
    });

    return () => {
      unlistenOpen?.();
      unlistenClose?.();
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="download-window" style="background-color: #0a0a0f;">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <header
    class="app-header glass download-header"
    data-tauri-drag-region
    onpointerdown={startDrag}
  >
    <div class="download-win-title" data-tauri-drag-region>Download</div>
    <div class="app-header-spacer" data-tauri-drag-region></div>
    <WindowControls showMinimize={false} showMaximize={false} />
  </header>

  <div class="download-content">
    {#if download.ytdlpReady === false}
      <div class="warning-banner">
        yt-dlp not found. Place <code>yt-dlp.exe</code> in <code>src-tauri/bin/</code>.
      </div>
    {:else if download.ffmpegReady === false}
      <div class="warning-banner">
        ffmpeg not found. Place <code>ffmpeg.exe</code> in <code>src-tauri/bin/</code> (same folder as yt-dlp).
      </div>
    {/if}

    <div class="url-row">
      <input
        type="url"
        class="url-input"
        placeholder="Paste YouTube, SoundCloud, or other link…"
        value={download.url}
        oninput={(e) => download.setUrl((e.target as HTMLInputElement).value)}
        disabled={download.isDownloading}
      />
      <button
        type="button"
        class="probe-btn"
        onclick={() => download.probeUrl()}
        disabled={download.isDownloading || download.isProbing || !looksLikeMediaUrl(download.url)}
      >
        {download.isProbing ? 'Checking…' : 'Check'}
      </button>
    </div>

    {#if download.error}
      <div class="error-msg">{download.error}</div>
    {/if}

    {#if download.probe}
      <div class="probe-card">
        {#if download.probe.thumbnail}
          <img class="probe-thumb" src={download.probe.thumbnail} alt="" />
        {/if}
        <div class="probe-info">
          <div class="probe-title">{download.probe.title}</div>
          {#if download.probe.uploader}
            <div class="probe-meta">{download.probe.uploader}</div>
          {/if}
          {#if download.probe.is_playlist && download.probe.entry_count}
            <div class="probe-meta">Playlist · {download.probe.entry_count} tracks</div>
          {:else if download.probe.duration_secs}
            <div class="probe-meta">{formatDuration(download.probe.duration_secs)}</div>
          {/if}
        </div>
      </div>
    {/if}

  </div>

  <footer class="download-footer">
    <div class="footer-status">
      {#if footerStatus}
        <span
          class="status-text"
          class:tone-success={footerStatus.tone === 'success'}
        >
          {footerStatus.text}
        </span>
      {/if}

      {#if footerPercent != null}
        <div class="progress-bar">
          <div class="progress-fill" style="width: {footerPercent}%"></div>
        </div>
        <span class="progress-pct">{footerPercent}%</span>
      {/if}
    </div>

    <div class="footer-actions">
      {#if download.isDownloading}
        <button type="button" class="action-btn secondary" onclick={() => download.cancel()}>
          Cancel
        </button>
      {:else}
        <button type="button" class="action-btn secondary" onclick={() => download.closeWindow()}>
          Close
        </button>
        <button
          type="button"
          class="action-btn primary"
          onclick={handleDownload}
          disabled={!download.probe && !looksLikeMediaUrl(download.url)}
        >
          Download
        </button>
      {/if}
    </div>
  </footer>
</div>

<style>
  @import './DownloadWindow.css';
</style>