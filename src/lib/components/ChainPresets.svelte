<script lang="ts">
  import {
    chainsMatch,
    getSettingsStore,
  } from '$lib/stores/settings.svelte';
  import Dropdown from '$lib/components/Dropdown.svelte';
  // Global (unscoped) so trigger styles always apply — same pattern as Dropdown.css.
  import './EffectPresets.css';

  const settings = getSettingsStore();

  let dropdownOpen = $state(false);
  let saveMode = $state(false);
  let newPresetName = $state('');

  const list = $derived(settings.chainPresets);

  const currentPresetName = $derived.by(() => {
    const chain = settings.dspChain;
    for (const p of list) {
      if (chainsMatch(chain, p.slots)) return p.name;
    }
    return null;
  });

  function applyPresetAndClose(name: string) {
    dropdownOpen = false;
    saveMode = false;
    newPresetName = '';
    void settings.applyChainPreset(name);
  }

  function startSaveMode(e?: Event) {
    e?.stopPropagation?.();
    saveMode = true;
    newPresetName = '';
    setTimeout(() => {
      const input = document.querySelector(
        '.chain-presets .preset-save-input',
      ) as HTMLInputElement | null;
      input?.focus();
      input?.select();
    }, 0);
  }

  async function confirmSavePreset(e?: Event) {
    e?.stopPropagation?.();
    const name = newPresetName.trim();
    if (!name) return;
    await settings.saveChainPreset(name);
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
    void settings.deleteChainPreset(name);
  }
</script>

<div class="effect-presets chain-presets">
  <span class="effect-presets-label">Preset:</span>
  <Dropdown class="preset-dropdown" bind:open={dropdownOpen} align="right">
    {#snippet trigger({ toggle })}
      <button
        type="button"
        class="preset-trigger chain-preset-trigger"
        class:custom={!currentPresetName && list.length > 0}
        onclick={toggle}
        aria-haspopup="listbox"
        aria-expanded={dropdownOpen}
        title="Save or load the whole effect chain"
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
          <span class="dropdown-item-label">+ Save current chain as...</span>
        </button>

        {#if list.length > 0}
          <div class="dropdown-divider"></div>

          {#each list as preset (preset.name)}
            <button
              type="button"
              class="dropdown-item"
              class:active={currentPresetName === preset.name}
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
  .dropdown-item-meta {
    flex-shrink: 0;
    margin-right: 18px;
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
  }

  :global(.chain-preset-trigger) {
    min-width: 96px;
    height: 26px;
    padding: 0 10px;
    font-size: 11px;
  }
</style>
