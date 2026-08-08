<script lang="ts">
  import { onDestroy } from 'svelte';
  import { LIMITER_MAX_GAIN_DB, getSettingsStore } from '$lib/stores/settings.svelte';
  import type { LimiterSettings } from '$lib/dsp/effects';

  interface Props {
    /** Which rack slot this editor writes to. */
    slotId: string;
    value: LimiterSettings;
    /** False while the row is bypassed — the meter would read 0 forever. */
    active: boolean;
  }

  let { slotId, value, active }: Props = $props();

  const settings = getSettingsStore();

  /** Meter poll interval — fast enough to look live, slow enough to stay cheap. */
  const METER_POLL_MS = 100;
  /** Reduction (dB) that fills the meter bar. */
  const METER_RANGE_DB = 12;

  let reductionDb = $state(0);
  let meterTimer: ReturnType<typeof setInterval> | null = null;

  const meterPct = $derived(Math.min(100, (reductionDb / METER_RANGE_DB) * 100));

  function fillPct(v: number, min: number, max: number): number {
    if (max <= min) return 0;
    return Math.max(0, Math.min(100, ((v - min) / (max - min)) * 100));
  }

  function stopMeter() {
    if (meterTimer) {
      clearInterval(meterTimer);
      meterTimer = null;
    }
    reductionDb = 0;
  }

  function patch(p: Partial<LimiterSettings>) {
    void settings.updateSlot(slotId, p);
  }

  // Poll only while this slot is actually limiting: idle or bypassed always reads 0.
  $effect(() => {
    if (!active) {
      stopMeter();
      return;
    }
    if (meterTimer) return;
    meterTimer = setInterval(async () => {
      const status = await settings.fetchChainStatus();
      const next = status?.slots.find((s) => s.id === slotId)?.meter_db ?? 0;
      // Decay towards the reading so short peaks stay readable instead of flickering.
      reductionDb = next > reductionDb ? next : reductionDb + (next - reductionDb) * 0.35;
    }, METER_POLL_MS);
  });

  onDestroy(stopMeter);
</script>

<div class="limiter-card">
  <div class="limiter-gain">
    <div class="limiter-row-head">
      <span class="limiter-row-label">Gain</span>
      <span class="limiter-row-value">+{value.gain_db.toFixed(1)} dB</span>
    </div>
    <input
      type="range"
      class="limiter-slider gain"
      min="0"
      max={LIMITER_MAX_GAIN_DB}
      step="0.5"
      value={value.gain_db}
      style={`--fill: ${fillPct(value.gain_db, 0, LIMITER_MAX_GAIN_DB)}%`}
      oninput={(e) => patch({ gain_db: Number((e.target as HTMLInputElement).value) })}
      aria-label="Limiter gain"
    />
    <div class="limiter-bounds">
      <span>0 dB</span>
      <span>+{LIMITER_MAX_GAIN_DB} dB</span>
    </div>
  </div>

  <label class="limiter-clip" class:on={value.clip}>
    <input
      type="checkbox"
      checked={value.clip}
      onchange={(e) => patch({ clip: (e.target as HTMLInputElement).checked })}
    />
    <span class="clip-text">
      <span class="clip-label">Hard clip</span>
      <span class="clip-hint">Chop the peaks instead of holding them back — loud and dirty</span>
    </span>
  </label>

  <div class="limiter-meter" class:idle={!active}>
    <div class="limiter-row-head">
      <span class="limiter-row-label">{value.clip ? 'Clipped' : 'Gain reduction'}</span>
      <span class="limiter-row-value">−{reductionDb.toFixed(1)} dB</span>
    </div>
    <div class="meter-track">
      <div class="meter-fill" class:clipping={value.clip} style={`width: ${meterPct}%`}></div>
    </div>
  </div>

  <div class="limiter-grid">
    <div class="limiter-field">
      <div class="limiter-row-head">
        <span class="limiter-row-label">Ceiling</span>
        <span class="limiter-row-value">{value.ceiling_db.toFixed(1)} dBFS</span>
      </div>
      <input
        type="range"
        class="limiter-slider"
        min="-6"
        max="0"
        step="0.1"
        value={value.ceiling_db}
        style={`--fill: ${fillPct(value.ceiling_db, -6, 0)}%`}
        oninput={(e) => patch({ ceiling_db: Number((e.target as HTMLInputElement).value) })}
        aria-label="Limiter ceiling"
      />
    </div>

    <div class="limiter-field">
      <div class="limiter-row-head">
        <span class="limiter-row-label">Release</span>
        <span class="limiter-row-value">{Math.round(value.release_ms)} ms</span>
      </div>
      <input
        type="range"
        class="limiter-slider"
        min="10"
        max="1000"
        step="5"
        value={value.release_ms}
        style={`--fill: ${fillPct(value.release_ms, 10, 1000)}%`}
        oninput={(e) => patch({ release_ms: Number((e.target as HTMLInputElement).value) })}
        aria-label="Limiter release"
      />
    </div>
  </div>

  <div class="limiter-presets">
    {#each [
      { label: 'Transparent', gain: 2, release: 200 },
      { label: 'Loud', gain: 6, release: 120 },
      { label: 'Hard', gain: 9, release: 60 },
      { label: 'Ебашит', gain: 12, release: 30 },
    ] as p (p.label)}
      <button
        type="button"
        class="preset-btn"
        class:active={Math.abs(value.gain_db - p.gain) < 0.01}
        onclick={() => patch({ gain_db: p.gain, release_ms: p.release })}
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
  @import './Limiter.css';
</style>
