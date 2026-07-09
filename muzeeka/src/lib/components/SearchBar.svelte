<script lang="ts">
  import { getDownloadStore } from '$lib/stores/download.svelte';
  import { looksLikeMediaUrl } from '$lib/urlUtils';

  interface Props {
    searchQuery?: string;
  }

  let { searchQuery = $bindable('') }: Props = $props();

  const download = getDownloadStore();

  let successMsg = $state<string | null>(null);
  let probeTimer: ReturnType<typeof setTimeout> | null = null;
  let successTimer: ReturnType<typeof setTimeout> | null = null;

  let isUrlSearch = $derived(looksLikeMediaUrl(searchQuery));

  let downloadLabel = $derived.by(() => {
    if (successMsg) return successMsg;
    if (download.isDownloading) {
      return `${download.downloadPercent ?? 0}%`;
    }
    if (download.isProbing) return 'Checking…';
    return 'Download';
  });

  let downloadDisabled = $derived(
    download.isProbing
    || download.isDownloading
    || !isUrlSearch
    || download.ytdlpReady === false
  );

  function formatDuration(secs: number | null | undefined): string {
    if (secs == null || !Number.isFinite(secs) || secs <= 0) return '';
    const mins = Math.floor(secs / 60);
    const s = Math.floor(secs % 60);
    return `${mins}:${s.toString().padStart(2, '0')}`;
  }

  function clearSearch() {
    searchQuery = '';
    successMsg = null;
    download.clearProbeState();
    if (successTimer) {
      clearTimeout(successTimer);
      successTimer = null;
    }
  }

  async function handleDownload() {
    if (!isUrlSearch || download.isDownloading) return;

    successMsg = null;
    const added = await download.download(searchQuery);
    if (added > 0) {
      successMsg = added === 1 ? 'Added' : `+${added}`;
      if (successTimer) clearTimeout(successTimer);
      successTimer = setTimeout(() => {
        clearSearch();
      }, 1800);
    }
  }

  $effect(() => {
    if (!isUrlSearch) {
      download.clearProbeState();
      successMsg = null;
      return;
    }

    const query = searchQuery;
    if (probeTimer) clearTimeout(probeTimer);
    probeTimer = setTimeout(() => {
      void download.probeUrl(query);
    }, 320);

    return () => {
      if (probeTimer) {
        clearTimeout(probeTimer);
        probeTimer = null;
      }
    };
  });
</script>

<div class="search-area" class:url-active={isUrlSearch}>
  <div class="search-row">
    <svg class="search-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <circle cx="11" cy="11" r="8"/>
      <line x1="21" y1="21" x2="16.65" y2="16.65"/>
    </svg>
    <input
      type="text"
      class="search-input"
      placeholder="Search tracks or paste URL…"
      bind:value={searchQuery}
      disabled={download.isDownloading}
    />
    {#if searchQuery}
      <button class="search-clear" onclick={clearSearch} aria-label="Clear search">
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <line x1="18" y1="6" x2="6" y2="18"/>
          <line x1="6" y1="6" x2="18" y2="18"/>
        </svg>
      </button>
    {/if}
  </div>

  {#if isUrlSearch}
    <div class="search-url-panel">
      <div class="search-url-content">
        {#if download.probe}
          {#if download.probe.thumbnail}
            <img class="search-thumb" src={download.probe.thumbnail} alt="" />
          {:else}
            <div class="search-thumb search-thumb-placeholder" aria-hidden="true">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M9 18V5l12-2v13"/>
                <circle cx="6" cy="18" r="3"/>
                <circle cx="18" cy="16" r="3"/>
              </svg>
            </div>
          {/if}

          <div class="search-probe-info">
            <div class="search-probe-title" title={download.probe.title}>{download.probe.title}</div>
            <div class="search-probe-meta">
              {#if download.probe.uploader}
                <span>{download.probe.uploader}</span>
              {/if}
              {#if download.probe.is_playlist && download.probe.entry_count}
                {#if download.probe.uploader}<span class="meta-sep">·</span>{/if}
                <span>Playlist · {download.probe.entry_count} tracks</span>
              {:else if download.probe.duration_secs}
                {#if download.probe.uploader}<span class="meta-sep">·</span>{/if}
                <span>{formatDuration(download.probe.duration_secs)}</span>
              {/if}
            </div>
            {#if download.isDownloading && download.progress?.status}
              <div class="search-probe-status">{download.progress.status}</div>
            {/if}
            {#if download.error}
              <div class="search-probe-error">{download.error}</div>
            {/if}
          </div>
        {:else if download.isProbing}
          <div class="search-url-placeholder">
            <span class="search-url-spinner" aria-hidden="true"></span>
            <span>Recognizing link…</span>
          </div>
        {:else if download.error}
          <div class="search-url-placeholder error">
            <span>{download.error}</span>
          </div>
        {:else}
          <div class="search-url-placeholder">
            <span>Media link detected</span>
          </div>
        {/if}
      </div>

      <div class="search-url-actions">
        {#if download.isDownloading}
          <button
            type="button"
            class="search-action-btn secondary"
            onclick={() => download.cancel()}
          >
            Cancel
          </button>
        {/if}
        <button
          type="button"
          class="search-action-btn primary"
          class:success={successMsg != null}
          onclick={handleDownload}
          disabled={downloadDisabled}
          title={download.error ?? undefined}
        >
          {#if !successMsg}
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
              <polyline points="7 10 12 15 17 10"/>
              <line x1="12" y1="15" x2="12" y2="3"/>
            </svg>
          {/if}
          {downloadLabel}
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  @import './SearchBar.css';
</style>