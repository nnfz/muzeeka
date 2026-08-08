<script lang="ts">
  import { getSettingsStore } from '$lib/stores/settings.svelte';
  import { HP_OPEN_HZ, LP_OPEN_HZ, formatHz, type FilterSettings } from '$lib/dsp/effects';

  interface Props {
    /** Which rack slot this editor writes to. */
    slotId: string;
    value: FilterSettings;
  }

  let { slotId, value }: Props = $props();

  const settings = getSettingsStore();

  /**
   * Cutoffs move on a log scale — a linear slider would spend three quarters of
   * its travel above 5 kHz, where almost nothing interesting happens.
   */
  const MIN_HZ = HP_OPEN_HZ;
  const MAX_HZ = LP_OPEN_HZ;
  const LOG_MIN = Math.log(MIN_HZ);
  const LOG_SPAN = Math.log(MAX_HZ) - LOG_MIN;

  function hzToPos(hz: number): number {
    const clamped = Math.max(MIN_HZ, Math.min(MAX_HZ, hz));
    return ((Math.log(clamped) - LOG_MIN) / LOG_SPAN) * 100;
  }

  function posToHz(pos: number): number {
    const hz = Math.exp(LOG_MIN + (pos / 100) * LOG_SPAN);
    // Snap the ends so "fully open" is reachable by dragging, not just by Reset.
    if (pos <= 0.5) return MIN_HZ;
    if (pos >= 99.5) return MAX_HZ;
    return Math.round(hz);
  }

  const lpOpen = $derived(value.lp_hz >= LP_OPEN_HZ);
  const hpOpen = $derived(value.hp_hz <= HP_OPEN_HZ);
  /** Both cutoffs crossed: the passband is empty and nothing gets through. */
  const inverted = $derived(!lpOpen && !hpOpen && value.hp_hz >= value.lp_hz);

  function patch(p: Partial<FilterSettings>) {
    void settings.updateSlot(slotId, p);
  }
</script>

<div class="filter-card">
  <div class="filter-grid">
    <div class="filter-field">
      <div class="filter-row-head">
        <span class="filter-row-label">High-pass</span>
        <span class="filter-row-value" class:open={hpOpen}>
          {hpOpen ? 'Open' : `${formatHz(value.hp_hz)} Hz`}
        </span>
      </div>
      <input
        type="range"
        class="filter-slider"
        min="0"
        max="100"
        step="0.1"
        value={hzToPos(value.hp_hz)}
        style={`--fill: ${hzToPos(value.hp_hz)}%`}
        oninput={(e) => patch({ hp_hz: posToHz(Number((e.target as HTMLInputElement).value)) })}
        aria-label="High-pass cutoff"
      />
      <div class="filter-bounds">
        <span>Open</span>
        <span>20k</span>
      </div>
    </div>

    <div class="filter-field">
      <div class="filter-row-head">
        <span class="filter-row-label">Low-pass</span>
        <span class="filter-row-value" class:open={lpOpen}>
          {lpOpen ? 'Open' : `${formatHz(value.lp_hz)} Hz`}
        </span>
      </div>
      <input
        type="range"
        class="filter-slider"
        min="0"
        max="100"
        step="0.1"
        value={hzToPos(value.lp_hz)}
        style={`--fill: ${hzToPos(value.lp_hz)}%`}
        oninput={(e) => patch({ lp_hz: posToHz(Number((e.target as HTMLInputElement).value)) })}
        aria-label="Low-pass cutoff"
      />
      <div class="filter-bounds">
        <span>20</span>
        <span>Open</span>
      </div>
    </div>
  </div>

  <div class="filter-field resonance">
    <div class="filter-row-head">
      <span class="filter-row-label">Resonance</span>
      <span class="filter-row-value">Q {value.resonance.toFixed(2)}</span>
    </div>
    <input
      type="range"
      class="filter-slider"
      min="0.5"
      max="8"
      step="0.05"
      value={value.resonance}
      style={`--fill: ${((value.resonance - 0.5) / 7.5) * 100}%`}
      oninput={(e) => patch({ resonance: Number((e.target as HTMLInputElement).value) })}
      aria-label="Filter resonance"
    />
    <div class="filter-bounds">
      <span>Flat</span>
      <span>Squelch</span>
    </div>
  </div>

  {#if inverted}
    <div class="filter-warning">
      High-pass is above low-pass — nothing is left in the passband.
    </div>
  {/if}

  <div class="filter-presets">
    {#each [
      { label: 'Open', lp: LP_OPEN_HZ, hp: HP_OPEN_HZ },
      { label: 'Telephone', lp: 3000, hp: 400 },
      { label: 'Muffled', lp: 800, hp: HP_OPEN_HZ },
      { label: 'Thin', lp: LP_OPEN_HZ, hp: 400 },
      { label: 'Sub only', lp: 120, hp: HP_OPEN_HZ },
    ] as p (p.label)}
      <button
        type="button"
        class="preset-btn"
        class:active={Math.abs(value.lp_hz - p.lp) < 1 && Math.abs(value.hp_hz - p.hp) < 1}
        onclick={() => patch({ lp_hz: p.lp, hp_hz: p.hp })}
      >
        {p.label}
      </button>
    {/each}
    <button type="button" class="preset-btn reset" onclick={() => void settings.resetSlot(slotId)}>
      Reset
    </button>
  </div>
</div>

<style>
  @import './Filter.css';
</style>
