<script lang="ts">
  import {
    getSettingsStore,
    type EQPreset,
    type FilterPreset,
    type LimiterPreset,
  } from '$lib/stores/settings.svelte';
  import type {
    EffectKind,
    EqualizerSettings,
    FilterSettings,
    LimiterSettings,
  } from '$lib/dsp/effects';
  import Dropdown from '$lib/components/Dropdown.svelte';

  interface Props {
    slotId: string;
    kind: EffectKind;
    value: EqualizerSettings | FilterSettings | LimiterSettings;
    /** Fired after a preset is applied (EQ uses this for slider animation). */
    onApply?: (name: string) => void;
  }

  let { slotId, kind, value, onApply }: Props = $props();

  const settings = getSettingsStore();

  let dropdownOpen = $state(false);
  let saveMode = $state(false);
  let newPresetName = $state('');

  const list = $derived(settings.presetsFor(kind));

  const currentPresetName = $derived.by(() => {
    if (kind === 'equalizer') {
      const v = value as EqualizerSettings;
      for (const p of list as EQPreset[]) {
        if (
          Math.abs(p.preamp_db - v.preamp_db) < 0.05 &&
          p.bands_db.length === v.bands_db.length &&
          p.bands_db.every((g, i) => Math.abs(g - (v.bands_db[i] ?? 0)) < 0.05)
        ) {
          return p.name;
        }
      }
      return null;
    }
    if (kind === 'filter') {
      const v = value as FilterSettings;
      for (const p of list as FilterPreset[]) {
        if (
          Math.abs(p.lp_hz - v.lp_hz) < 1 &&
          Math.abs(p.hp_hz - v.hp_hz) < 1 &&
          Math.abs(p.resonance - v.resonance) < 0.02
        ) {
          return p.name;
        }
      }
      return null;
    }
    const v = value as LimiterSettings;
    for (const p of list as LimiterPreset[]) {
      if (
        Math.abs(p.gain_db - v.gain_db) < 0.05 &&
        Math.abs(p.ceiling_db - v.ceiling_db) < 0.05 &&
        Math.abs(p.release_ms - v.release_ms) < 1 &&
        p.clip === v.clip
      ) {
        return p.name;
      }
    }
    return null;
  });

  function applyPresetAndClose(name: string) {
    dropdownOpen = false;
    saveMode = false;
    newPresetName = '';
    void settings.applyPreset(slotId, name).then(() => onApply?.(name));
  }

  function startSaveMode(e?: Event) {
    e?.stopPropagation?.();
    saveMode = true;
    newPresetName = '';
    setTimeout(() => {
      const input = document.querySelector(
        '.effect-presets .preset-save-input',
      ) as HTMLInputElement | null;
      input?.focus();
      input?.select();
    }, 0);
  }

  async function confirmSavePreset(e?: Event) {
    e?.stopPropagation?.();
    const name = newPresetName.trim();
    if (!name) return;
    await settings.savePreset(slotId, name);
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
    void settings.deletePreset(kind, name);
  }
</script>

<div class="effect-presets">
  <span class="effect-presets-label">Preset:</span>
  <Dropdown class="preset-dropdown" bind:open={dropdownOpen} align="right">
    {#snippet trigger({ toggle })}
      <button
        type="button"
        class="preset-trigger"
        class:custom={!currentPresetName && list.length > 0}
        onclick={toggle}
        aria-haspopup="listbox"
        aria-expanded={dropdownOpen}
      >
        <span class="preset-label">
          {currentPresetName || (list.length ? 'Custom' : 'None')}
        </span>
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
            <button type="button" class="preset-action-btn cancel" onclick={cancelSaveMode}
              >Cancel</button
            >
          </div>
        </div>
      {:else}
        <button type="button" class="dropdown-item accent" onclick={startSaveMode}>
          <span class="dropdown-item-label">+ Save current as...</span>
        </button>

        {#if list.length > 0}
          <div class="dropdown-divider"></div>

          {#each list as preset (preset.name)}
            <button
              type="button"
              class="dropdown-item"
              onclick={() => applyPresetAndClose(preset.name)}
            >
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

<style>
  @import './EffectPresets.css';
</style>
