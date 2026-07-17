<script lang="ts">
  import '../../app.css';
  import '../../routes/+page.css';
  import WindowControls from './WindowControls.svelte';
  import SettingsSidebar from './SettingsSidebar.svelte';
  import Equalizer from './Equalizer.svelte';
  import { getSettingsStore } from '$lib/stores/settings.svelte';
  type Section = 'general' | 'audio' | 'about';
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

  interface CoverRebuildStats {
    cleared_files: number;
    track_covers: number;
    unique_images: number;
    playlist_covers: number;
    errors: number;
  }

  // Prevent white flash when the window becomes visible
  if (typeof document !== 'undefined') {
    document.documentElement.style.setProperty('background-color', '#0a0a0f', 'important');
    if (document.body) {
      document.body.style.setProperty('background-color', '#0a0a0f', 'important');
    }
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

  onMount(() => {
    let unlisten: UnlistenFn | null = null;

    void (async () => {
      try {
        appVersion = await getVersion();
        appName = await getName();
      } catch {
        // fallback already set
      }

      try {
        const data = await invoke<{ playlists: { id: string; name: string }[] }>('playlists_load');
        playlists = (data.playlists ?? []).map((p) => ({ id: p.id, name: p.name }));
      } catch {
        playlists = [];
      }

      await refreshVkAuth();

      try {
        unlisten = await listen<VkAuthStatus>('vk:auth-changed', (event) => {
          vkAuth = event.payload;
          vkAuthError = null;
        });
      } catch {
        // non-fatal
      }
    })();

    return () => {
      unlisten?.();
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
</script>

<div class="settings-window" style="background-color: #0a0a0f;">
  <header class="app-header glass">
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
          <p class="section-desc">
            Application behavior and preferences. Most settings are saved automatically.
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
            <div class="card-row">
              <div>
                <div class="card-label">Playlists &amp; library</div>
                <div class="card-value">Stored locally in app data</div>
              </div>
              <div class="card-badge">Auto-saved</div>
            </div>
            <div class="card-row">
              <div>
                <div class="card-label">Volume level</div>
                <div class="card-value">Persisted across restarts</div>
              </div>
              <div class="card-badge">Auto-saved</div>
            </div>
            <div class="card-row">
              <div>
                <div class="card-label">Playback speed / rate</div>
                <div class="card-value">Persisted in Audio settings</div>
              </div>
              <div class="card-badge">Auto-saved</div>
            </div>
            <div class="card-row">
              <div>
                <div class="card-label">Discord Rich Presence</div>
                <div class="card-value">Show the current track in Discord</div>
              </div>
              <label class="discord-toggle">
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
                <div class="card-label">Cover art cache</div>
                <div class="card-value">
                  Rebuild as WebP, dedupe identical album art (one file per unique image)
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

          <h2 class="section-title section-title-spaced">VK Music</h2>
          <p class="section-desc">
            Log in to download tracks and playlists from vk.com / vk.ru. Session is stored only on this device.
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

          <div class="settings-info">
            Keyboard shortcuts and mouse controls are available in the main window.
            Use Alt + scroll to adjust volume.
          </div>

        </div>
      {:else if activeSection === 'audio'}
        <div class="settings-section">
          <h2 class="section-title">Audio</h2>
          <p class="section-desc">
            15-band 1/3-octave graphic EQ with 32-bit floating-point DSP processing.
          </p>
          <Equalizer />

          <!-- Playback Rate -->
          <div class="settings-card rate-card">
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
                <span class="rate-value-big">{settings.playbackRate.toFixed(2)}×</span>
              </div>
            </div>

            <div class="rate-slider-row">
              <input
                type="range"
                min="0.25"
                max="2"
                step="0.01"
                value={settings.playbackRate}
                oninput={(e) => settings.setPlaybackRate(parseFloat((e.target as HTMLInputElement).value))}
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
                  onclick={() => void settings.setPlaybackRate(r, { immediate: true })}
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

  .section-title-spaced {
    margin-top: 22px;
  }

  .card-value-error {
    color: #f07178;
    margin-top: 4px;
  }

  .card-value-ok {
    color: #7fd99a;
    margin-top: 4px;
  }

  .action-btn-primary {
    background: var(--accent-soft);
    color: var(--accent);
    border-color: transparent;
  }

  .action-btn-primary:disabled {
    opacity: 0.6;
  }

  .card-row-stack {
    flex-wrap: wrap;
    align-items: flex-start;
  }

  .card-value-path {
    word-break: break-all;
    max-width: 42ch;
  }

  .card-actions {
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }

  .playlist-select {
    min-width: 180px;
    height: 32px;
    padding: 0 10px;
    font-size: 12px;
    color: var(--text-primary);
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    outline: none;
  }

  .playlist-select:focus {
    border-color: var(--border-accent);
  }
</style>
