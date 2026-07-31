<script lang="ts">
  import '../../app.css';
  import '../../routes/+page.css';
  import WindowControls from './WindowControls.svelte';
  import SettingsSidebar from './SettingsSidebar.svelte';
  import Equalizer from './Equalizer.svelte';
  import Dropdown from '$lib/components/Dropdown.svelte';
  import { getSettingsStore, type RemoteStatus } from '$lib/stores/settings.svelte';
  import {
    applyAccentPalette,
    hydrateAccentFromStorage,
    type AccentPalette,
  } from '$lib/coverAccent';
  type Section = 'general' | 'downloads' | 'remote' | 'audio' | 'about';
  import { getVersion, getName } from '@tauri-apps/api/app';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { open } from '@tauri-apps/plugin-dialog';
  import { onMount } from 'svelte';

  interface VkAuthStatus {
    logged_in: boolean;
    user_id: number | null;
    user_name: string | null;
  }

  const settings = getSettingsStore();

  let clearAllBusy = $state(false);
  let clearAllConfirm = $state(false);
  let clearAllError = $state<string | null>(null);

  let activeSection = $state<Section>('general');
  let appVersion = $state('0.1.0');
  let appName = $state('muzeeka');
  let playlists = $state<{ id: string; name: string }[]>([]);
  let vkAuth = $state<VkAuthStatus>({ logged_in: false, user_id: null, user_name: null });
  let vkAuthBusy = $state(false);
  let vkAuthError = $state<string | null>(null);

  let coverRebuildBusy = $state(false);
  let coverRebuildMsg = $state<string | null>(null);
  let coverRebuildError = $state<string | null>(null);

  let remoteStatus = $state<RemoteStatus | null>(null);
  let portDraft = $state(String(settings.remotePort));

  /** Visual playback rate (animated on presets; instant on drag). */
  let displayRate = $state(settings.playbackRate);
  let rateUserTouched = $state(false);
  let rateAnimRaf = 0;

  interface CoverRebuildStats {
    cleared_files: number;
    track_covers: number;
    unique_images: number;
    playlist_covers: number;
    errors: number;
  }

  // Prevent white flash when the window becomes visible
  if (typeof document !== 'undefined') {
    document.documentElement.style.setProperty('background-color', '#0a0a0a', 'important');
    if (document.body) {
      document.body.style.setProperty('background-color', '#0a0a0a', 'important');
    }
    // Match main window cover accent (shared localStorage origin).
    hydrateAccentFromStorage();
  }

  async function refreshVkAuth() {
    try {
      vkAuth = await invoke<VkAuthStatus>('vk_auth_status');
      vkAuthError = null;
    } catch (e) {
      vkAuth = { logged_in: false, user_id: null, user_name: null };
      vkAuthError = typeof e === 'string' ? e : String(e);
    }
  }

  async function refreshRemoteStatus() {
    remoteStatus = await settings.fetchRemoteStatus();
    if (remoteStatus) {
      portDraft = String(remoteStatus.port);
    }
  }

  onMount(() => {
    let unlistenVk: UnlistenFn | null = null;
    let unlistenAccent: UnlistenFn | null = null;
    let remotePoll: ReturnType<typeof setInterval> | null = null;

    hydrateAccentFromStorage();

    void (async () => {
      try {
        appVersion = await getVersion();
        appName = await getName();
      } catch {
        // fallback already set
      }

      try {
        // Meta-only list: no full library, no per-track Path::exists prune.
        // Full playlists_load was freezing the app on open for large / network libs.
        playlists = await invoke<{ id: string; name: string }[]>('playlists_list_meta');
      } catch {
        playlists = [];
      }

      await refreshVkAuth();
      await refreshRemoteStatus();
      portDraft = String(settings.remotePort);

      try {
        unlistenVk = await listen<VkAuthStatus>('vk:auth-changed', (event) => {
          vkAuth = event.payload;
          vkAuthError = null;
        });
      } catch {
        // non-fatal
      }

      try {
        unlistenAccent = await listen<AccentPalette>('muzeeka:accent', (event) => {
          applyAccentPalette(event.payload, { persist: false });
        });
      } catch {
        // non-fatal
      }

      remotePoll = setInterval(() => {
        if (activeSection === 'remote') {
          void refreshRemoteStatus();
        }
      }, 2000);
    })();

    return () => {
      unlistenVk?.();
      unlistenAccent?.();
      if (remotePoll) clearInterval(remotePoll);
      cancelRateAnim();
    };
  });

  async function pickDownloadFolder() {
    const selected = await open({ directory: true });
    if (selected) {
      settings.setDownloadFolder(selected as string);
    }
  }

  function clearDownloadFolder() {
    settings.setDownloadFolder(null);
  }

  async function vkLogin() {
    if (vkAuthBusy) return;
    vkAuthBusy = true;
    vkAuthError = null;
    try {
      vkAuth = await invoke<VkAuthStatus>('vk_login');
    } catch (e) {
      vkAuthError = typeof e === 'string' ? e : String(e);
      await refreshVkAuth();
    } finally {
      vkAuthBusy = false;
    }
  }

  async function vkLogout() {
    if (vkAuthBusy) return;
    vkAuthBusy = true;
    vkAuthError = null;
    try {
      vkAuth = await invoke<VkAuthStatus>('vk_logout');
    } catch (e) {
      vkAuthError = typeof e === 'string' ? e : String(e);
      await refreshVkAuth();
    } finally {
      vkAuthBusy = false;
    }
  }

  function vkStatusLabel(status: VkAuthStatus): string {
    if (!status.logged_in) return 'Not logged in';
    if (status.user_name && status.user_id) {
      return `${status.user_name} (id${status.user_id})`;
    }
    if (status.user_name) return status.user_name;
    if (status.user_id) return `id${status.user_id}`;
    return 'Logged in';
  }

  async function clearAll() {
    if (!clearAllConfirm) {
      clearAllConfirm = true;
      clearAllError = null;
      setTimeout(() => { clearAllConfirm = false; }, 3000);
      return;
    }
    clearAllBusy = true;
    clearAllConfirm = false;
    clearAllError = null;
    try {
      await invoke('library_clear_all');
    } catch (e) {
      clearAllError = typeof e === 'string' ? e : String(e);
    } finally {
      clearAllBusy = false;
    }
  }

  async function rebuildCovers() {
    if (coverRebuildBusy) return;
    coverRebuildBusy = true;
    coverRebuildMsg = null;
    coverRebuildError = null;
    try {
      const stats = await invoke<CoverRebuildStats>('library_rebuild_covers');
      const parts = [
        `cleared ${stats.cleared_files}`,
        `tracks ${stats.track_covers}`,
        `unique images ${stats.unique_images}`,
        `playlists ${stats.playlist_covers}`,
      ];
      if (stats.errors > 0) parts.push(`errors ${stats.errors}`);
      coverRebuildMsg = `Done — ${parts.join(' · ')}. Same album art is stored once.`;
    } catch (e) {
      coverRebuildError = typeof e === 'string' ? e : String(e);
    } finally {
      coverRebuildBusy = false;
    }
  }

  function onRemoteEnabledChange(checked: boolean) {
    settings.setRemoteEnabled(checked);
    // Refresh after save debounce + server restart
    setTimeout(() => void refreshRemoteStatus(), 450);
  }

  function commitRemotePort() {
    const parsed = parseInt(portDraft, 10);
    if (!Number.isFinite(parsed)) {
      portDraft = String(settings.remotePort);
      return;
    }
    settings.setRemotePort(parsed);
    portDraft = String(settings.remotePort);
    setTimeout(() => void refreshRemoteStatus(), 450);
  }

  function remoteStatusBadge(status: RemoteStatus | null): { text: string; kind: 'ok' | 'warn' | 'muted' } {
    if (!status) return { text: '…', kind: 'muted' };
    if (!status.enabled) return { text: 'Off', kind: 'muted' };
    if (status.running) return { text: 'Running', kind: 'ok' };
    if (status.last_error) return { text: 'Error', kind: 'warn' };
    return { text: 'Starting…', kind: 'warn' };
  }

  let remoteBadge = $derived(remoteStatusBadge(remoteStatus));

  function rateFillPct(rate: number): number {
    return Math.max(0, Math.min(100, ((rate - 0.25) / (2 - 0.25)) * 100));
  }

  function cancelRateAnim() {
    if (rateAnimRaf) {
      cancelAnimationFrame(rateAnimRaf);
      rateAnimRaf = 0;
    }
  }

  function animateRate(target: number, duration = 280) {
    cancelRateAnim();
    const from = displayRate;
    const start = performance.now();
    const tick = (now: number) => {
      const t = Math.min(1, (now - start) / duration);
      const e = 1 - Math.pow(1 - t, 3);
      displayRate = from + (target - from) * e;
      if (t < 1) {
        rateAnimRaf = requestAnimationFrame(tick);
      } else {
        rateAnimRaf = 0;
        displayRate = target;
      }
    };
    rateAnimRaf = requestAnimationFrame(tick);
  }

  $effect(() => {
    const r = settings.playbackRate;
    if (rateUserTouched || rateAnimRaf) return;
    displayRate = r;
  });

  function onRateInput(e: Event) {
    rateUserTouched = true;
    cancelRateAnim();
    const v = parseFloat((e.target as HTMLInputElement).value);
    displayRate = v;
    void settings.setPlaybackRate(v);
  }

  function onRatePreset(r: number) {
    rateUserTouched = true;
    void settings.setPlaybackRate(r, { immediate: true });
    animateRate(r);
  }
</script>

<div class="settings-window">
  <header class="app-header settings-header">
    <div class="settings-win-title" data-tauri-drag-region>Settings</div>
    <div class="app-header-spacer" data-tauri-drag-region></div>
    <WindowControls showMinimize={false} showMaximize={false} />
  </header>

  <div class="settings-layout">
    <SettingsSidebar bind:activeSection />

    <div class="settings-content">
      {#if activeSection === 'general'}
        <div class="settings-section">
          <h2 class="section-title">General</h2>
          <p class="section-desc">App behavior and integrations.</p>

          <div class="settings-card">
            <div class="card-row">
              <div>
                <div class="card-label">Discord Rich Presence</div>
                <div class="card-value">Show the current track in Discord</div>
              </div>
              <label class="settings-toggle">
                <input
                  type="checkbox"
                  checked={settings.discordRpcEnabled}
                  onchange={(e) =>
                    settings.setDiscordRpcEnabled((e.target as HTMLInputElement).checked)}
                />
                <span>Enabled</span>
              </label>
            </div>
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Shuffle mode</div>
                <div class="card-value">
                  {#if settings.shuffleMode === 'smart'}
                    Smart: remembers tracks already played in this playlist and won’t
                    repeat them until every track has had a turn
                  {:else}
                    Normal: classic random order; tracks may come up again sooner when
                    the order reshuffles
                  {/if}
                </div>
              </div>
              <div class="mode-switch" role="group" aria-label="Shuffle mode">
                <button
                  type="button"
                  class="mode-btn"
                  class:active={settings.shuffleMode === 'smart'}
                  onclick={() => settings.setShuffleMode('smart')}
                >
                  Smart
                </button>
                <button
                  type="button"
                  class="mode-btn"
                  class:active={settings.shuffleMode === 'normal'}
                  onclick={() => settings.setShuffleMode('normal')}
                >
                  Normal
                </button>
              </div>
            </div>
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Cover art cache</div>
                <div class="card-value">
                  Rebuild as WebP and dedupe identical album art
                </div>
                {#if coverRebuildMsg}
                  <div class="card-value card-value-ok">{coverRebuildMsg}</div>
                {/if}
                {#if coverRebuildError}
                  <div class="card-value card-value-error">{coverRebuildError}</div>
                {/if}
              </div>
              <div class="card-actions">
                <button
                  type="button"
                  class="action-btn"
                  disabled={coverRebuildBusy}
                  onclick={() => void rebuildCovers()}
                >
                  {coverRebuildBusy ? 'Rebuilding…' : 'Rebuild covers'}
                </button>
              </div>
            </div>
          </div>

          <div class="settings-card">
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Library</div>
                <div class="card-value">Remove all playlists and tracks from the library</div>
                {#if clearAllError}
                  <div class="card-value card-value-error">{clearAllError}</div>
                {/if}
              </div>
              <div class="card-actions">
                <button
                  type="button"
                  class="action-btn action-btn-danger"
                  disabled={clearAllBusy}
                  onclick={() => void clearAll()}
                >
                  {clearAllBusy ? 'Clearing…' : clearAllConfirm ? 'Are you sure?' : 'Delete all'}
                </button>
              </div>
            </div>
          </div>

          <div class="settings-info">
            Keyboard shortcuts and mouse controls are available in the main window.
            Use Alt + scroll to adjust volume.
          </div>
        </div>
      {:else if activeSection === 'downloads'}
        <div class="settings-section">
          <h2 class="section-title">Downloads</h2>
          <p class="section-desc">
            Where downloaded tracks are saved and which playlist receives them.
          </p>

          <div class="settings-card">
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Download folder</div>
                <div class="card-value card-value-path">
                  {settings.downloadFolder ?? (settings.effectiveDownloadFolder || 'App data / downloads')}
                </div>
              </div>
              <div class="card-actions">
                <button type="button" class="action-btn" onclick={pickDownloadFolder}>
                  Choose…
                </button>
                {#if settings.downloadFolder}
                  <button type="button" class="action-btn" onclick={clearDownloadFolder}>
                    Reset
                  </button>
                {/if}
              </div>
            </div>
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Download playlist</div>
                <div class="card-value">Tracks are added here after download</div>
              </div>
              <select
                class="playlist-select"
                value={settings.downloadPlaylistId ?? ''}
                onchange={(e) => {
                  const val = (e.target as HTMLSelectElement).value;
                  settings.setDownloadPlaylistId(val || null);
                }}
              >
                <option value="">Downloads (auto-create)</option>
                {#each playlists as pl (pl.id)}
                  <option value={pl.id}>{pl.name}</option>
                {/each}
              </select>
            </div>
          </div>

          <h2 class="section-title section-title-spaced">VK Music</h2>
          <p class="section-desc">
            Log in to download tracks and playlists from vk.com / vk.ru. Session stays on this device.
          </p>

          <div class="settings-card">
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Account</div>
                <div class="card-value">
                  {vkStatusLabel(vkAuth)}
                </div>
                {#if vkAuthError}
                  <div class="card-value card-value-error">{vkAuthError}</div>
                {/if}
              </div>
              <div class="card-actions">
                {#if vkAuth.logged_in}
                  <div class="card-badge">Connected</div>
                  <button
                    type="button"
                    class="action-btn"
                    disabled={vkAuthBusy}
                    onclick={() => void vkLogout()}
                  >
                    {vkAuthBusy ? 'Working…' : 'Log out'}
                  </button>
                {:else}
                  <button
                    type="button"
                    class="action-btn action-btn-primary"
                    disabled={vkAuthBusy}
                    onclick={() => void vkLogin()}
                  >
                    {vkAuthBusy ? 'Waiting…' : 'Log in with VK'}
                  </button>
                {/if}
              </div>
            </div>
          </div>
        </div>
      {:else if activeSection === 'remote'}
        <div class="settings-section">
          <h2 class="section-title">Remote control</h2>
          <p class="section-desc">
            Control playback from a phone or browser on the same network.
          </p>

          <div class="settings-card">
            <div class="card-row">
              <div>
                <div class="card-label">Remote server</div>
                <div class="card-value">HTTP control panel on your local network</div>
              </div>
              <div class="card-actions">
                <div class="card-badge {remoteBadge.kind}">{remoteBadge.text}</div>
                <label class="settings-toggle">
                  <input
                    type="checkbox"
                    checked={settings.remoteEnabled}
                    onchange={(e) =>
                      onRemoteEnabledChange((e.target as HTMLInputElement).checked)}
                  />
                  <span>Enabled</span>
                </label>
              </div>
            </div>

            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Computer IP</div>
                <div class="card-value card-value-mono">
                  {remoteStatus?.local_ip ?? '—'}
                </div>
                {#if remoteStatus?.local_ips?.length}
                  <div class="card-value">
                    Also:
                    {#each remoteStatus.local_ips as ip, i (ip)}
                      <span class="card-value-mono">{ip}</span>{i < remoteStatus.local_ips.length - 1 ? ', ' : ''}
                    {/each}
                  </div>
                {/if}
              </div>
            </div>

            <div class="card-row">
              <div>
                <div class="card-label">Port</div>
                <div class="card-value">1024–65535 (default 8765)</div>
              </div>
              <input
                class="port-input"
                type="number"
                min="1024"
                max="65535"
                step="1"
                disabled={!settings.remoteEnabled}
                bind:value={portDraft}
                onchange={commitRemotePort}
                onkeydown={(e) => {
                  if (e.key === 'Enter') {
                    (e.target as HTMLInputElement).blur();
                    commitRemotePort();
                  }
                }}
              />
            </div>

            {#if settings.remoteEnabled && remoteStatus?.urls?.length}
              <div class="card-row card-row-stack">
                <div>
                  <div class="card-label">Open on phone</div>
                  <div class="card-value">Same Wi‑Fi as this PC</div>
                  {#each remoteStatus.urls as url (url)}
                    <a class="remote-url" href={url} target="_blank" rel="noreferrer">{url}</a>
                  {/each}
                </div>
              </div>
            {/if}

            {#if remoteStatus?.last_error}
              <div class="card-row">
                <div class="card-value card-value-error">{remoteStatus.last_error}</div>
              </div>
            {/if}
          </div>

          <div class="settings-info">
            Open the URL on your phone. If it doesn’t load, check firewall rules for the chosen port.
          </div>
        </div>
      {:else if activeSection === 'audio'}
        <div class="settings-section">
          <h2 class="section-title">Audio</h2>
          <p class="section-desc">15-band graphic equalizer and playback speed.</p>
          <Equalizer />

          <!-- Playback Rate -->
          <div class="rate-card">
            <div class="card-header">
              <div>
                <div class="card-label">Playback speed</div>
                <div class="card-value">
                  {#if settings.pitchEnabled}
                    Speed changes shift pitch (vinyl-style)
                  {:else}
                    Original pitch preserved while changing speed
                  {/if}
                </div>
              </div>
              <div class="rate-display">
                <span class="rate-value-big">{displayRate.toFixed(2)}×</span>
              </div>
            </div>

            <div class="rate-slider-row">
              <input
                type="range"
                class="rate-slider"
                min="0.25"
                max="2"
                step="0.01"
                value={displayRate}
                style={`--fill: ${rateFillPct(displayRate)}%`}
                oninput={onRateInput}
              />
              <div class="rate-bounds">
                <span>0.25×</span>
                <span>2.00×</span>
              </div>
            </div>

            <div class="rate-presets">
              {#each [0.75, 0.85, 1.0, 1.25, 1.5] as r}
                <button
                  type="button"
                  class="preset-btn"
                  class:active={Math.abs(settings.playbackRate - r) < 0.01}
                  onclick={() => onRatePreset(r)}
                >
                  {r.toFixed(r === 1 ? 1 : 2)}×
                </button>
              {/each}
              <button
                type="button"
                class="preset-btn pitch-btn"
                class:active={settings.pitchEnabled}
                onclick={() => void settings.setPitchEnabled(!settings.pitchEnabled)}
                title={settings.pitchEnabled
                  ? 'Pitch shifts with speed — click to preserve pitch'
                  : 'Pitch preserved — click to couple pitch with speed'}
              >
                Pitch
              </button>
            </div>
          </div>
        </div>
      {:else if activeSection === 'about'}
        <div class="settings-section about-section">
          <div class="about-header">
            <div class="about-logo">
              <img src="/app-logo.png" alt="" width="52" height="52" />
            </div>
            <div>
              <div class="about-name">{appName}</div>
              <div class="about-version">Version {appVersion}</div>
            </div>
          </div>

          <p class="about-desc">
            A lightweight, high-quality desktop music player.<br />
            Built for clean playback and fast browsing.
          </p>

          <div class="about-meta">
            <div class="meta-item">
              <span class="meta-key">Built with</span>
              <span class="meta-val">Tauri 2 • Svelte 5 • Rust</span>
            </div>
            <div class="meta-item">
              <span class="meta-key">Audio engine</span>
              <span class="meta-val">BASS by Un4seen Developments</span>
            </div>
            <div class="meta-item">
              <span class="meta-key">Metadata</span>
              <span class="meta-val">Lofty</span>
            </div>
          </div>

          <div class="about-footer">
            Settings and user data are stored in your system app data directory.
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  @import './SettingsWindow.css';
</style>
