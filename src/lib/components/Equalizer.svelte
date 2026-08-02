<script lang="ts">
  import { onDestroy } from 'svelte';
  import {
    BAND_COUNT,
    BAND_FREQUENCIES,
    getSettingsStore,
  } from '$lib/stores/settings.svelte';
  import Dropdown from '$lib/components/Dropdown.svelte';

  const settings = getSettingsStore();

  let displayPreamp = $state(settings.equalizer.preamp_db);
  let displayBands = $state<number[]>(
    Array.from({ length: BAND_COUNT }, (_, i) => settings.equalizer.bands_db[i] ?? 0),
  );
  let userTouched = $state(false);
  let animRaf = 0;

  function formatFreq(freq: number): string {
    if (freq >= 1000) {
      const k = freq / 1000;
      // 12.5k not 12.5k with trailing noise; 16k / 20k stay clean integers.
      return Number.isInteger(k) ? `${k}k` : `${k.toFixed(1)}k`;
    }
    return String(freq);
  }

  function fillPct(value: number, min: number, max: number): number {
    if (max <= min) return 0;
    return Math.max(0, Math.min(100, ((value - min) / (max - min)) * 100));
  }

  function cancelAnim() {
    if (animRaf) {
      cancelAnimationFrame(animRaf);
      animRaf = 0;
    }
  }

  function easeOutCubic(t: number): number {
    return 1 - Math.pow(1 - t, 3);
  }

  function animateDisplay(targetPreamp: number, targetBands: number[], duration = 320) {
    cancelAnim();
    const fromPreamp = displayPreamp;
    const fromBands = [...displayBands];
    const toBands = Array.from({ length: BAND_COUNT }, (_, i) => targetBands[i] ?? 0);
    const start = performance.now();

    const tick = (now: number) => {
      const t = Math.min(1, (now - start) / duration);
      const e = easeOutCubic(t);
      displayPreamp = fromPreamp + (targetPreamp - fromPreamp) * e;
      displayBands = fromBands.map((b, i) => b + (toBands[i] - b) * e);
      if (t < 1) {
        animRaf = requestAnimationFrame(tick);
      } else {
        animRaf = 0;
        displayPreamp = targetPreamp;
        displayBands = [...toBands];
      }
    };
    animRaf = requestAnimationFrame(tick);
  }

  $effect(() => {
    const eq = settings.equalizer;
    if (userTouched || animRaf) return;
    displayPreamp = eq.preamp_db;
    displayBands = Array.from({ length: BAND_COUNT }, (_, i) => eq.bands_db[i] ?? 0);
  });

  function handleBandInput(index: number, e: Event) {
    userTouched = true;
    cancelAnim();
    const value = Number((e.target as HTMLInputElement).value);
    displayBands = displayBands.map((g, i) => (i === index ? value : g));
    void settings.setBandGain(index, value);
  }

  function handlePreampInput(e: Event) {
    userTouched = true;
    cancelAnim();
    const value = Number((e.target as HTMLInputElement).value);
    displayPreamp = value;
    void settings.setPreamp(value);
  }

  function applyPresetAndClose(name: string) {
    const p = settings.customPresets.find((x) => x.name === name);
    if (!p) return;
    userTouched = true;
    dropdownOpen = false;
    saveMode = false;
    newPresetName = '';
    void settings.applyPreset(name);
    animateDisplay(p.preamp_db, p.bands_db);
  }

  function handleReset() {
    userTouched = true;
    void settings.resetEqualizer();
    animateDisplay(0, Array(BAND_COUNT).fill(0));
  }

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

  function startSaveMode(e?: Event) {
    e?.stopPropagation?.();
    saveMode = true;
    newPresetName = '';
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
    dropdownOpen = false;
    saveMode = false;
    newPresetName = '';
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

  function handleDeletePreset(e: MouseEvent | KeyboardEvent, name: string) {
    e.stopPropagation();
    void settings.deletePreset(name);
  }

  onDestroy(() => cancelAnim());
</script>

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

    <div class="eq-presets">
      <span class="eq-presets-label">Preset:</span>
      <Dropdown class="preset-dropdown" bind:open={dropdownOpen} align="right">
        {#snippet trigger({ toggle })}
          <button
            type="button"
            class="preset-trigger"
            class:custom={!currentPresetName && settings.customPresets.length > 0}
            onclick={toggle}
            aria-haspopup="listbox"
            aria-expanded={dropdownOpen}
          >
            <span class="preset-label">{currentPresetName || (settings.customPresets.length ? 'Custom' : 'None')}</span>
            <span class="preset-chevron">▾</span>
          </button>
        {/snippet}
        {#snippet menu()}
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
            <button type="button" class="dropdown-item accent" onclick={startSaveMode}>
              <span class="dropdown-item-label">+ Save current as...</span>
            </button>

            {#if settings.customPresets.length > 0}
              <div class="dropdown-divider"></div>

              {#each settings.customPresets as preset (preset.name)}
                <button type="button" class="dropdown-item" onclick={() => applyPresetAndClose(preset.name)}>
                  <span class="dropdown-item-label">{preset.name}</span>
                  <span
                    class="dropdown-item-action danger"
                    title="Delete preset"
                    role="button"
                    tabindex="0"
                    onclick={(e) => handleDeletePreset(e, preset.name)}
                    onkeydown={(e) => {
                      if (e.key !== 'Enter' && e.key !== ' ') return;
                      e.preventDefault();
                      handleDeletePreset(e, preset.name);
                    }}
                  >
                    ×
                  </span>
                </button>
              {/each}
            {/if}
          {/if}
        {/snippet}
      </Dropdown>
    </div>

    <button type="button" class="eq-reset" onclick={handleReset}>
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
        value={displayPreamp}
        style={`--fill: ${fillPct(displayPreamp, -15, 15)}%`}
        oninput={handlePreampInput}
        aria-label="Preamp"
      />
      <span class="eq-gain">{displayPreamp > 0 ? '+' : ''}{displayPreamp.toFixed(1)}</span>
      <span class="eq-freq">Preamp</span>
    </div>

    {#each Array(BAND_COUNT) as _, i (i)}
      {@const gain = displayBands[i] ?? 0}
      <div class="eq-band">
        <input
          type="range"
          class="eq-slider"
          min="-20"
          max="20"
          step="0.1"
          value={gain}
          style={`--fill: ${fillPct(gain, -20, 20)}%`}
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