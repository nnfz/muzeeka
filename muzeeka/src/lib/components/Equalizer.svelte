<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onDestroy } from 'svelte';
  import {
    BAND_COUNT,
    BAND_FREQUENCIES,
    getSettingsStore,
  } from '$lib/stores/settings.svelte';

  const settings = getSettingsStore();

  let dspAttached = $state(false);
  let processCount = $state(0);
  let statusTimer: ReturnType<typeof setInterval> | null = null;

  async function refreshEqStatus() {
    try {
      const status = await invoke<{
        dsp_attached: boolean;
        process_count: number;
      }>('player_get_equalizer_status');
      dspAttached = status.dsp_attached;
      processCount = status.process_count;
    } catch {
      dspAttached = false;
      processCount = 0;
    }
  }

  statusTimer = setInterval(() => {
    void refreshEqStatus();
  }, 1000);
  void refreshEqStatus();

  onDestroy(() => {
    if (statusTimer) clearInterval(statusTimer);
  });

  function formatFreq(freq: number): string {
    if (freq >= 1000) return `${freq / 1000}k`;
    return String(freq);
  }

  function handleBandInput(index: number, e: Event) {
    const value = Number((e.target as HTMLInputElement).value);
    void settings.setBandGain(index, value);
  }

  function handlePreampInput(e: Event) {
    const value = Number((e.target as HTMLInputElement).value);
    void settings.setPreamp(value);
  }

  function handlePresetChange(name: string) {
    if (name) {
      void settings.applyPreset(name);
    }
  }

  // Custom dropdown state
  let dropdownOpen = $state(false);
  let saveMode = $state(false);
  let newPresetName = $state('');

  const currentPresetName = $derived.by(() => {
    const eq = settings.equalizer;
    for (const p of settings.customPresets) {
      if (
        Math.abs(p.preamp_db - eq.preamp_db) < 0.05 &&
        p.bands_db.length === eq.bands_db.length &&
        p.bands_db.every((v, i) => Math.abs(v - eq.bands_db[i]) < 0.05)
      ) {
        return p.name;
      }
    }
    return null;
  });

  function toggleDropdown(e?: MouseEvent) {
    e?.stopPropagation();
    dropdownOpen = !dropdownOpen;
    if (!dropdownOpen) {
      saveMode = false;
      newPresetName = '';
    }
  }

  function closeDropdown() {
    dropdownOpen = false;
    saveMode = false;
    newPresetName = '';
  }

  function startSaveMode(e?: MouseEvent) {
    e?.stopPropagation();
    saveMode = true;
    newPresetName = '';
    // focus input after render
    setTimeout(() => {
      const input = document.querySelector('.preset-save-input') as HTMLInputElement | null;
      input?.focus();
      input?.select();
    }, 0);
  }

  async function confirmSavePreset(e?: Event) {
    e?.stopPropagation?.();
    const name = newPresetName.trim();
    if (!name) return;
    await settings.savePreset(name);
    // After saving, close and the label will update if it matches
    closeDropdown();
  }

  function cancelSaveMode(e?: Event) {
    e?.stopPropagation?.();
    saveMode = false;
    newPresetName = '';
  }

  function handleSaveKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      void confirmSavePreset();
    } else if (e.key === 'Escape') {
      cancelSaveMode();
    }
  }

  function applyPresetAndClose(name: string) {
    handlePresetChange(name);
    closeDropdown();
  }

  async function deleteAndRefresh(name: string, e: MouseEvent) {
    e.stopPropagation();
    await settings.deletePreset(name);
    // dropdown stays open
  }

  // Close dropdown on outside click / escape
  function handleGlobalClick(e: MouseEvent) {
    if (!dropdownOpen) return;
    const target = e.target as HTMLElement;
    if (!target.closest('.preset-dropdown')) {
      closeDropdown();
    }
  }

  function handleGlobalKey(e: KeyboardEvent) {
    if (dropdownOpen && e.key === 'Escape') {
      closeDropdown();
    }
  }
</script>

<svelte:window onclick={handleGlobalClick} onkeydown={handleGlobalKey} />

<div class="equalizer">
  <div class="eq-toolbar">
    <label class="eq-toggle">
      <input
        type="checkbox"
        checked={settings.equalizer.enabled}
        onchange={(e) => settings.setEqualizerEnabled((e.target as HTMLInputElement).checked)}
      />
      <span>Enable EQ</span>
    </label>

    <span class="eq-badge" class:eq-badge--active={dspAttached}>
      {dspAttached ? 'DSP active' : 'DSP inactive'} · {processCount} buffers
    </span>

    <div class="eq-presets">
      <span class="eq-presets-label">Preset:</span>
      <div class="preset-dropdown">
        <button
          type="button"
          class="preset-trigger"
          class:custom={!currentPresetName && settings.customPresets.length > 0}
          onclick={toggleDropdown}
          aria-haspopup="listbox"
          aria-expanded={dropdownOpen}
        >
          <span class="preset-label">{currentPresetName || (settings.customPresets.length ? 'Custom' : 'None')}</span>
          <span class="preset-chevron">▾</span>
        </button>

        {#if dropdownOpen}
          <div class="preset-menu glass" role="listbox">
            {#if saveMode}
              <div class="preset-save">
                <input
                  type="text"
                  class="preset-save-input"
                  placeholder="Preset name"
                  bind:value={newPresetName}
                  onkeydown={handleSaveKeydown}
                />
                <div class="preset-save-actions">
                  <button type="button" class="preset-action-btn" onclick={confirmSavePreset}>Save</button>
                  <button type="button" class="preset-action-btn cancel" onclick={cancelSaveMode}>Cancel</button>
                </div>
              </div>
            {:else}
              <button
                type="button"
                class="preset-menu-item save"
                onclick={startSaveMode}
              >
                + Save current as...
              </button>

              {#if settings.customPresets.length > 0}
                <div class="preset-divider"></div>

                {#each settings.customPresets as preset (preset.name)}
                  <div class="preset-row">
                    <button
                      type="button"
                      class="preset-menu-item"
                      onclick={() => applyPresetAndClose(preset.name)}
                    >
                      {preset.name}
                    </button>
                    <button
                      type="button"
                      class="preset-delete-btn"
                      title="Delete preset"
                      onclick={(e) => deleteAndRefresh(preset.name, e)}
                    >
                      ×
                    </button>
                  </div>
                {/each}
              {/if}
            {/if}
          </div>
        {/if}
      </div>
    </div>

    <button type="button" class="eq-reset" onclick={() => settings.resetEqualizer()}>
      Reset
    </button>
  </div>

  <div class="eq-sliders">
    <div class="eq-band preamp-band">
      <input
        type="range"
        class="eq-slider"
        min="-15"
        max="15"
        step="0.1"
        value={settings.equalizer.preamp_db}
        oninput={handlePreampInput}
        aria-label="Preamp"
      />
      <span class="eq-gain">{settings.equalizer.preamp_db > 0 ? '+' : ''}{settings.equalizer.preamp_db.toFixed(1)}</span>
      <span class="eq-freq">Preamp</span>
    </div>

    {#each Array(BAND_COUNT) as _, i (i)}
      {@const gain = settings.equalizer.bands_db[i] ?? 0}
      <div class="eq-band">
        <input
          type="range"
          class="eq-slider"
          min="-20"
          max="20"
          step="0.1"
          value={gain}
          oninput={(e) => handleBandInput(i, e)}
          aria-label={`${BAND_FREQUENCIES[i]} Hz`}
        />
        <span class="eq-gain">{gain > 0 ? '+' : ''}{gain.toFixed(1)}</span>
        <span class="eq-freq">{formatFreq(BAND_FREQUENCIES[i])}</span>
      </div>
    {/each}
  </div>
</div>

<style>
  @import './Equalizer.css';
</style>