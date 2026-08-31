<script lang="ts">
  import "../../app.css";
  import "../../routes/+page.css";
  import WindowControls from "./WindowControls.svelte";
  import SettingsSidebar from "./SettingsSidebar.svelte";
  import DspChain from "./DspChain.svelte";
  import Dropdown from "$lib/components/Dropdown.svelte";
  import { getSettingsStore } from "$lib/stores/settings.svelte";
  import {
    applyAccentPalette,
    hydrateAccentFromStorage,
    type AccentPalette,
  } from "$lib/coverAccent";
  import {
    createPluginsStore,
    type HttpStatus,
  } from "$lib/stores/plugins.svelte";
  import { getVersion, getName } from "@tauri-apps/api/app";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import { onMount } from "svelte";

  type Section = "general" | "downloads" | "plugins" | "audio" | "developer" | "about";

  interface DevLogLine {
    ts: number;
    level: string;
    source: string;
    message: string;
  }

  interface VkAuthStatus {
    logged_in: boolean;
    user_id: number | null;
    user_name: string | null;
  }

  interface YoutubeAuthStatus {
    logged_in: boolean;
  }

  const settings = getSettingsStore();
  const plugins = createPluginsStore();

  let clearAllBusy = $state(false);
  let clearAllConfirm = $state(false);
  let clearAllError = $state<string | null>(null);

  let activeSection = $state<Section>("general");
  let appVersion = $state("0.1.0");
  let appName = $state("muzeeka");
  let playlists = $state<{ id: string; name: string }[]>([]);
  let downloadPlaylistOpen = $state(false);
  let vkAuth = $state<VkAuthStatus>({
    logged_in: false,
    user_id: null,
    user_name: null,
  });
  let vkAuthBusy = $state(false);
  let vkAuthError = $state<string | null>(null);
  let youtubeAuth = $state<YoutubeAuthStatus>({ logged_in: false });
  let youtubeAuthBusy = $state(false);
  let youtubeAuthError = $state<string | null>(null);

  let coverRebuildBusy = $state(false);
  let coverRebuildMsg = $state<string | null>(null);
  let coverRebuildError = $state<string | null>(null);

  let pluginError = $state<string | null>(null);
  let logLines = $state<DevLogLine[]>([]);
  let consoleEl = $state<HTMLDivElement | null>(null);
  let consoleStick = $state(true);
  let pluginBusyId = $state<string | null>(null);
  let settingDrafts = $state<Record<string, string>>({});

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
  if (typeof document !== "undefined") {
    document.documentElement.style.setProperty(
      "background-color",
      "#0a0a0a",
      "important",
    );
    if (document.body) {
      document.body.style.setProperty(
        "background-color",
        "#0a0a0a",
        "important",
      );
    }
    // Match main window cover accent (shared localStorage origin).
    hydrateAccentFromStorage();
  }

  async function refreshVkAuth() {
    try {
      vkAuth = await invoke<VkAuthStatus>("vk_auth_status");
      vkAuthError = null;
    } catch (e) {
      vkAuth = { logged_in: false, user_id: null, user_name: null };
      vkAuthError = typeof e === "string" ? e : String(e);
    }
  }

  async function refreshYoutubeAuth() {
    try {
      youtubeAuth = await invoke<YoutubeAuthStatus>("ytdlp_youtube_auth_status");
      youtubeAuthError = null;
    } catch (e) {
      youtubeAuth = { logged_in: false };
      youtubeAuthError = typeof e === "string" ? e : String(e);
    }
  }

  onMount(() => {
    let unlistenVk: UnlistenFn | null = null;
    let unlistenYoutube: UnlistenFn | null = null;
    let unlistenAccent: UnlistenFn | null = null;
    let unlistenLog: UnlistenFn | null = null;
    let pluginPoll: ReturnType<typeof setInterval> | null = null;

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
        playlists = await invoke<{ id: string; name: string }[]>(
          "playlists_list_meta",
        );
      } catch {
        playlists = [];
      }

      await refreshVkAuth();
      await refreshYoutubeAuth();
      await plugins.refresh();

      try {
        logLines = await invoke<DevLogLine[]>("dev_log_lines");
      } catch {
        logLines = [];
      }

      try {
        unlistenLog = await listen<DevLogLine>("dev:log", (event) => {
          logLines = [...logLines, event.payload].slice(-500);
        });
      } catch {
        // non-fatal
      }

      try {
        unlistenVk = await listen<VkAuthStatus>("vk:auth-changed", (event) => {
          vkAuth = event.payload;
          vkAuthError = null;
        });
      } catch {
        // non-fatal
      }

      try {
        unlistenYoutube = await listen<YoutubeAuthStatus>(
          "youtube:auth-changed",
          (event) => {
            youtubeAuth = event.payload;
            youtubeAuthError = null;
          },
        );
      } catch {
        // non-fatal
      }

      try {
        unlistenAccent = await listen<AccentPalette>(
          "muzeeka:accent",
          (event) => {
            applyAccentPalette(event.payload, { persist: false });
          },
        );
      } catch {
        // non-fatal
      }

      pluginPoll = setInterval(() => {
        if (activeSection === "plugins") {
          void plugins.refresh();
        }
      }, 2000);
    })();

    return () => {
      unlistenVk?.();
      unlistenYoutube?.();
      unlistenAccent?.();
      unlistenLog?.();
      if (pluginPoll) clearInterval(pluginPoll);
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

  async function youtubeLogin() {
    if (youtubeAuthBusy) return;
    youtubeAuthBusy = true;
    youtubeAuthError = null;
    try {
      youtubeAuth = await invoke<YoutubeAuthStatus>("ytdlp_youtube_login");
    } catch (e) {
      youtubeAuthError = typeof e === "string" ? e : String(e);
      await refreshYoutubeAuth();
    } finally {
      youtubeAuthBusy = false;
    }
  }

  async function youtubeLogout() {
    if (youtubeAuthBusy) return;
    youtubeAuthBusy = true;
    youtubeAuthError = null;
    try {
      youtubeAuth = await invoke<YoutubeAuthStatus>("ytdlp_youtube_logout");
    } catch (e) {
      youtubeAuthError = typeof e === "string" ? e : String(e);
      await refreshYoutubeAuth();
    } finally {
      youtubeAuthBusy = false;
    }
  }

  async function vkLogin() {
    if (vkAuthBusy) return;
    vkAuthBusy = true;
    vkAuthError = null;
    try {
      vkAuth = await invoke<VkAuthStatus>("vk_login");
    } catch (e) {
      vkAuthError = typeof e === "string" ? e : String(e);
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
      vkAuth = await invoke<VkAuthStatus>("vk_logout");
    } catch (e) {
      vkAuthError = typeof e === "string" ? e : String(e);
      await refreshVkAuth();
    } finally {
      vkAuthBusy = false;
    }
  }

  function vkStatusLabel(status: VkAuthStatus): string {
    if (!status.logged_in) return "Not logged in";
    if (status.user_name && status.user_id) {
      return `${status.user_name} (id${status.user_id})`;
    }
    if (status.user_name) return status.user_name;
    if (status.user_id) return `id${status.user_id}`;
    return "Logged in";
  }

  async function clearAll() {
    if (!clearAllConfirm) {
      clearAllConfirm = true;
      clearAllError = null;
      setTimeout(() => {
        clearAllConfirm = false;
      }, 3000);
      return;
    }
    clearAllBusy = true;
    clearAllConfirm = false;
    clearAllError = null;
    try {
      await invoke("library_clear_all");
    } catch (e) {
      clearAllError = typeof e === "string" ? e : String(e);
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
      const stats = await invoke<CoverRebuildStats>("library_rebuild_covers");
      const parts = [
        `cleared ${stats.cleared_files}`,
        `tracks ${stats.track_covers}`,
        `unique images ${stats.unique_images}`,
        `playlists ${stats.playlist_covers}`,
      ];
      if (stats.errors > 0) parts.push(`errors ${stats.errors}`);
      coverRebuildMsg = `Done — ${parts.join(" · ")}. Same album art is stored once.`;
    } catch (e) {
      coverRebuildError = typeof e === "string" ? e : String(e);
    } finally {
      coverRebuildBusy = false;
    }
  }

  async function togglePlugin(id: string, enabled: boolean) {
    pluginError = null;
    pluginBusyId = id;
    try {
      await plugins.setEnabled(id, enabled);
    } catch (e) {
      pluginError = typeof e === "string" ? e : String(e);
    } finally {
      pluginBusyId = null;
    }
  }

  function settingDraftKey(pluginId: string, key: string) {
    return `${pluginId}:${key}`;
  }

  function settingText(pluginId: string, key: string, fallback: unknown) {
    const draftKey = settingDraftKey(pluginId, key);
    if (Object.prototype.hasOwnProperty.call(settingDrafts, draftKey)) {
      return settingDrafts[draftKey];
    }
    return fallback == null ? "" : String(fallback);
  }

  async function commitPluginSetting(
    pluginId: string,
    spec: { key: string; type: string; min?: number | null; max?: number | null },
  ) {
    pluginError = null;
    const draftKey = settingDraftKey(pluginId, spec.key);
    const plugin = plugins.list.find((p) => p.id === pluginId);
    const raw = settingDrafts[draftKey] ?? String(plugin?.config?.[spec.key] ?? "");
    let value: unknown = raw;
    if (spec.type === "number") {
      let n = Number(raw);
      if (!Number.isFinite(n)) {
        const next = { ...settingDrafts };
        delete next[draftKey];
        settingDrafts = next;
        return;
      }
      if (typeof spec.min === "number") n = Math.max(spec.min, n);
      if (typeof spec.max === "number") n = Math.min(spec.max, n);
      const integer =
        (spec.min == null || Number.isInteger(spec.min)) &&
        (spec.max == null || Number.isInteger(spec.max));
      if (integer) n = Math.round(n);
      value = n;
    }
    try {
      await plugins.setConfig(pluginId, { [spec.key]: value });
      const next = { ...settingDrafts };
      delete next[draftKey];
      settingDrafts = next;
      await plugins.refresh();
    } catch (e) {
      pluginError = typeof e === "string" ? e : String(e);
    }
  }

  async function setPluginBool(pluginId: string, key: string, value: boolean) {
    pluginError = null;
    try {
      await plugins.setConfig(pluginId, { [key]: value });
    } catch (e) {
      pluginError = typeof e === "string" ? e : String(e);
    }
  }

  async function openPluginsFolder() {
    if (!plugins.dir) return;
    try {
      await revealItemInDir(plugins.dir);
    } catch (e) {
      pluginError = typeof e === "string" ? e : String(e);
    }
  }

  $effect(() => {
    if (!settings.developerMode && activeSection === "developer") {
      activeSection = "about";
    }
  });

  $effect(() => {
    logLines;
    if (!consoleStick || !consoleEl) return;
    queueMicrotask(() => {
      if (consoleEl) consoleEl.scrollTop = consoleEl.scrollHeight;
    });
  });

  function onConsoleScroll() {
    if (!consoleEl) return;
    consoleStick =
      consoleEl.scrollHeight - consoleEl.scrollTop - consoleEl.clientHeight < 48;
  }

  function formatLogTime(ts: number): string {
    const d = new Date(ts);
    const pad = (n: number, w = 2) => String(n).padStart(w, "0");
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
  }

  async function clearDevLog() {
    try {
      await invoke("dev_log_clear");
    } catch {
      // still clear the view
    }
    logLines = [];
  }

  function httpBadge(status: HttpStatus): { text: string; kind: "ok" | "warn" | "muted" } | null {
    if (status.running) return { text: "Running", kind: "ok" };
    if (status.last_error) return { text: "Error", kind: "warn" };
    if (status.enabled) return { text: "Starting…", kind: "warn" };
    return null;
  }

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
    <SettingsSidebar
      bind:activeSection
      showDeveloper={settings.developerMode}
    />

    <div class="settings-content">
      {#if activeSection === "general"}
        <div class="settings-section">
          <div class="settings-section-header">
            <h2 class="section-title">General</h2>
            <p class="section-desc">App behavior and integrations.</p>
          </div>

          <div class="settings-card">
            <div class="card-row">
              <div>
                <div class="card-label">Discord Rich Presence</div>
                <div class="card-value">Show the current track in Discord</div>
              </div>
              <label class="settings-toggle">
                <input
                  type="checkbox"
                  role="switch"
                  checked={settings.discordRpcEnabled}
                  aria-label="Discord Rich Presence"
                  onchange={(e) =>
                    settings.setDiscordRpcEnabled(
                      (e.target as HTMLInputElement).checked,
                    )}
                />
                <span class="settings-switch" aria-hidden="true"></span>
              </label>
            </div>
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Shuffle mode</div>
                <div class="card-value">
                  {#if settings.shuffleMode === "smart"}
                    Smart: remembers tracks already played in this playlist and
                    won’t repeat them until every track has had a turn
                  {:else}
                    Normal: classic random order; tracks may come up again
                    sooner when the order reshuffles
                  {/if}
                </div>
              </div>
              <div class="mode-switch" role="group" aria-label="Shuffle mode">
                <button
                  type="button"
                  class="mode-btn"
                  class:active={settings.shuffleMode === "smart"}
                  onclick={() => settings.setShuffleMode("smart")}
                >
                  Smart
                </button>
                <button
                  type="button"
                  class="mode-btn"
                  class:active={settings.shuffleMode === "normal"}
                  onclick={() => settings.setShuffleMode("normal")}
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
                  <div class="card-value card-value-error">
                    {coverRebuildError}
                  </div>
                {/if}
              </div>
              <div class="card-actions">
                <button
                  type="button"
                  class="action-btn"
                  disabled={coverRebuildBusy}
                  onclick={() => void rebuildCovers()}
                >
                  {coverRebuildBusy ? "Rebuilding…" : "Rebuild covers"}
                </button>
              </div>
            </div>
          </div>

          <div class="settings-card">
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Library</div>
                <div class="card-value">
                  Remove all playlists and tracks from the library
                </div>
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
                  {clearAllBusy
                    ? "Clearing…"
                    : clearAllConfirm
                      ? "Are you sure?"
                      : "Delete all"}
                </button>
              </div>
            </div>
          </div>
        </div>
      {:else if activeSection === "downloads"}
        <div class="settings-section">
          <div class="settings-section-header">
            <h2 class="section-title">Downloads</h2>
            <p class="section-desc">
              Where downloaded tracks are saved and which playlist receives
              them.
            </p>
          </div>

          <div class="settings-card">
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Download folder</div>
                <div class="card-value card-value-path">
                  {settings.downloadFolder ??
                    (settings.effectiveDownloadFolder ||
                      "App data / downloads")}
                </div>
              </div>
              <div class="card-actions">
                <button
                  type="button"
                  class="action-btn"
                  onclick={pickDownloadFolder}
                >
                  Choose…
                </button>
                {#if settings.downloadFolder}
                  <button
                    type="button"
                    class="action-btn"
                    onclick={clearDownloadFolder}
                  >
                    Reset
                  </button>
                {/if}
              </div>
            </div>
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Download playlist</div>
                <div class="card-value">
                  Tracks are added here after download
                </div>
              </div>
              <Dropdown
                class="playlist-dropdown"
                bind:open={downloadPlaylistOpen}
                align="right"
              >
                {#snippet trigger({ toggle })}
                  <button
                    type="button"
                    class="playlist-select"
                    onclick={toggle}
                    aria-haspopup="listbox"
                    aria-expanded={downloadPlaylistOpen}
                  >
                    <span class="playlist-select-label">
                      {playlists.find((p) => p.id === settings.downloadPlaylistId)
                        ?.name ?? "Downloads (auto-create)"}
                    </span>
                    <span class="playlist-select-chevron">▾</span>
                  </button>
                {/snippet}
                {#snippet menu()}
                  <button
                    type="button"
                    class="dropdown-item"
                    class:active={!settings.downloadPlaylistId}
                    onclick={() => {
                      settings.setDownloadPlaylistId(null);
                      downloadPlaylistOpen = false;
                    }}
                  >
                    <span class="dropdown-item-label">Downloads (auto-create)</span>
                  </button>
                  {#if playlists.length}
                    <div class="dropdown-divider"></div>
                    {#each playlists as pl (pl.id)}
                      <button
                        type="button"
                        class="dropdown-item"
                        class:active={settings.downloadPlaylistId === pl.id}
                        onclick={() => {
                          settings.setDownloadPlaylistId(pl.id);
                          downloadPlaylistOpen = false;
                        }}
                      >
                        <span class="dropdown-item-label">{pl.name}</span>
                      </button>
                    {/each}
                  {/if}
                {/snippet}
              </Dropdown>
            </div>
          </div>
          <div class="settings-section-header">
            <h2 class="section-title section-title-spaced">YouTube</h2>
            <p class="section-desc">
              Log in to download from YouTube when it asks to confirm you are
              not a bot. Session stays on this device.
            </p>
          </div>

          <div class="settings-card">
            <div class="card-row card-row-stack">
              <div>
                <div class="card-label">Account</div>
                <div class="card-value">
                  {youtubeAuth.logged_in ? "Logged in" : "Not logged in"}
                </div>
                {#if youtubeAuthError}
                  <div class="card-value card-value-error">{youtubeAuthError}</div>
                {/if}
              </div>
              <div class="card-actions">
                {#if youtubeAuth.logged_in}
                  <button
                    type="button"
                    class="action-btn"
                    disabled={youtubeAuthBusy}
                    onclick={() => void youtubeLogout()}
                  >
                    {youtubeAuthBusy ? "Working…" : "Log out"}
                  </button>
                {:else}
                  <button
                    type="button"
                    class="action-btn action-btn-primary"
                    disabled={youtubeAuthBusy}
                    onclick={() => void youtubeLogin()}
                  >
                    {youtubeAuthBusy ? "Waiting…" : "Log in with YouTube"}
                  </button>
                {/if}
              </div>
            </div>
          </div>
          <div class="settings-section-header">
            <h2 class="section-title section-title-spaced">VK Music</h2>
            <p class="section-desc">
              Log in to download tracks and playlists from vk.com / vk.ru. Session
              stays on this device.
            </p>
          </div>

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
                  <button
                    type="button"
                    class="action-btn"
                    disabled={vkAuthBusy}
                    onclick={() => void vkLogout()}
                  >
                    {vkAuthBusy ? "Working…" : "Log out"}
                  </button>
                {:else}
                  <button
                    type="button"
                    class="action-btn action-btn-primary"
                    disabled={vkAuthBusy}
                    onclick={() => void vkLogin()}
                  >
                    {vkAuthBusy ? "Waiting…" : "Log in with VK"}
                  </button>
                {/if}
              </div>
            </div>
          </div>
        </div>
      {:else if activeSection === "plugins"}
        <div class="settings-section">
          <div class="settings-section-header">
            <h2 class="section-title">Plugins</h2>
          </div>

          <div class="settings-card">
            <div class="card-row">
              <div>
                <div class="card-label">Folder</div>
                <div class="card-value card-value-mono">
                  {plugins.dir || "—"}
                </div>
              </div>
              <div class="card-actions">
                <button
                  type="button"
                  class="action-btn"
                  disabled={!plugins.dir}
                  onclick={() => void openPluginsFolder()}
                >
                  Open
                </button>
              </div>
            </div>
            {#if pluginError}
              <div class="card-row">
                <div class="card-value card-value-error">{pluginError}</div>
              </div>
            {/if}
          </div>

          {#each plugins.list as plugin (plugin.id)}
            {@const badge = httpBadge(plugin.http)}
            <div class="settings-card">
              <div class="card-row">
                <div>
                  <div class="card-label">{plugin.name}</div>
                  <div class="card-value">
                    {plugin.id}
                    {#if plugin.version}
                      · {plugin.version}
                    {/if}
                    {#if plugin.author}
                      · {plugin.author}
                    {/if}
                    · {plugin.runtime === "native" ? "native" : "js"}
                  </div>
                  {#if plugin.description}
                    <div class="card-value">{plugin.description}</div>
                  {/if}
                  {#if plugin.error}
                    <div class="card-value card-value-error">{plugin.error}</div>
                  {/if}
                </div>
                <div class="card-actions">
                  {#if badge}
                    <div class="card-badge {badge.kind}">{badge.text}</div>
                  {/if}
                  <label class="settings-toggle">
                    <input
                      type="checkbox"
                      role="switch"
                      checked={plugin.enabled}
                      aria-label={plugin.name}
                      disabled={pluginBusyId === plugin.id}
                      onchange={(e) =>
                        void togglePlugin(
                          plugin.id,
                          (e.target as HTMLInputElement).checked,
                        )}
                    />
                    <span class="settings-switch" aria-hidden="true"></span>
                  </label>
                </div>
              </div>
              {#each plugin.settings as spec (spec.key)}
                <div class="card-row">
                  <div>
                    <div class="card-label">{spec.label || spec.key}</div>
                    {#if spec.description}
                      <div class="card-value">{spec.description}</div>
                    {/if}
                  </div>
                  <div class="card-actions">
                    {#if spec.type === "boolean"}
                      <label class="settings-toggle">
                        <input
                          type="checkbox"
                          role="switch"
                          checked={plugin.config?.[spec.key] === true}
                          aria-label={spec.label || spec.key}
                          onchange={(e) =>
                            void setPluginBool(
                              plugin.id,
                              spec.key,
                              (e.target as HTMLInputElement).checked,
                            )}
                        />
                        <span class="settings-switch" aria-hidden="true"></span>
                      </label>
                    {:else}
                      <input
                        class="port-input"
                        type={spec.type === "number" ? "number" : "text"}
                        min={spec.min ?? undefined}
                        max={spec.max ?? undefined}
                        step={spec.type === "number" ? "1" : undefined}
                        value={settingText(
                          plugin.id,
                          spec.key,
                          plugin.config?.[spec.key],
                        )}
                        oninput={(e) => {
                          settingDrafts = {
                            ...settingDrafts,
                            [settingDraftKey(plugin.id, spec.key)]: (
                              e.target as HTMLInputElement
                            ).value,
                          };
                        }}
                        onchange={() =>
                          void commitPluginSetting(plugin.id, spec)}
                        onkeydown={(e) => {
                          if (e.key === "Enter") {
                            (e.target as HTMLInputElement).blur();
                          }
                        }}
                      />
                    {/if}
                  </div>
                </div>
              {/each}
              {#if plugin.http?.urls?.length}
                <div class="card-row card-row-stack">
                  <div>
                    <div class="card-label">URL</div>
                    <div class="plugin-http-urls">
                      {#each plugin.http.urls as url (url)}
                        <a
                          class="plugin-http-url"
                          href={url}
                          target="_blank"
                          rel="noreferrer">{url}</a
                        >
                      {/each}
                    </div>
                  </div>
                </div>
              {/if}
              {#if plugin.http?.last_error}
                <div class="card-row">
                  <div class="card-value card-value-error">
                    {plugin.http.last_error}
                  </div>
                </div>
              {/if}
            </div>
          {:else}
            <div class="settings-info">No plugins found.</div>
          {/each}
        </div>
      {:else if activeSection === "audio"}
        <div class="settings-section">
          <div class="settings-section-header">
            <h2 class="section-title">Audio</h2>
            <p class="section-desc">
              Build an effect chain — drag effects in, stack them in any order,
              tune each one.
            </p>
          </div>
          <DspChain />

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
                onclick={() =>
                  void settings.setPitchEnabled(!settings.pitchEnabled)}
                title={settings.pitchEnabled
                  ? "Pitch shifts with speed — click to preserve pitch"
                  : "Pitch preserved — click to couple pitch with speed"}
              >
                Pitch
              </button>
            </div>
          </div>
        </div>
      {:else if activeSection === "about"}
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

          <div class="settings-card">
            <div class="card-row">
              <div>
                <div class="card-label">Developer mode</div>
                <div class="card-value">
                  Show the Development tab and in-app console
                </div>
              </div>
              <label class="settings-toggle">
                <input
                  type="checkbox"
                  role="switch"
                  checked={settings.developerMode}
                  aria-label="Developer mode"
                  onchange={(e) =>
                    settings.setDeveloperMode(
                      (e.target as HTMLInputElement).checked,
                    )}
                />
                <span class="settings-switch" aria-hidden="true"></span>
              </label>
            </div>
          </div>

          <div class="about-footer">
            Settings and user data are stored in your system app data directory.
          </div>
        </div>
      {:else if activeSection === "developer"}
        <div class="settings-section">
          <div class="settings-section-header">
            <h2 class="section-title">Development</h2>
            <p class="section-desc">
              Plugin and host logs. Native probe writes here when enabled.
            </p>
          </div>

          <div class="settings-card">
            <div class="card-row">
              <div>
                <div class="card-label">Console</div>
                <div class="card-value">{logLines.length} lines</div>
              </div>
              <div class="card-actions">
                <button
                  type="button"
                  class="action-btn"
                  disabled={!logLines.length}
                  onclick={() => void clearDevLog()}
                >
                  Clear
                </button>
              </div>
            </div>
            <div
              class="dev-console"
              bind:this={consoleEl}
              onscroll={onConsoleScroll}
            >
              {#each logLines as line, i (line.ts + ":" + i)}
                <div
                  class="dev-log-line"
                  class:is-error={line.level === "error"}
                >
                  <span class="dev-log-time">{formatLogTime(line.ts)}</span>
                  <span class="dev-log-src">{line.source}</span>
                  <span class="dev-log-msg">{line.message}</span>
                </div>
              {:else}
                <div class="dev-log-empty">No log lines yet.</div>
              {/each}
            </div>
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  @import "./SettingsWindow.css";
</style>
