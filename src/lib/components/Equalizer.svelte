<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { BAND_COUNT, BAND_FREQUENCIES, getSettingsStore } from '$lib/stores/settings.svelte';
  import { formatHz, type EqualizerSettings } from '$lib/dsp/effects';
  import EffectPresets from '$lib/components/EffectPresets.svelte';

  interface Props {
    /** Which rack slot this editor writes to. */
    slotId: string;
    value: EqualizerSettings;
  }

  let { slotId, value }: Props = $props();

  const settings = getSettingsStore();

  // Seeded once on mount; the $effect below keeps it in sync afterwards, except
  // while the user is dragging or an animation owns the sliders.
  let displayPreamp = $state(untrack(() => value.preamp_db));
  let displayBands = $state<number[]>(
    untrack(() => Array.from({ length: BAND_COUNT }, (_, i) => value.bands_db[i] ?? 0)),
  );
  let userTouched = $state(false);
  let animRaf = 0;

  function fillPct(v: number, min: number, max: number): number {
    if (max <= min) return 0;
    return Math.max(0, Math.min(100, ((v - min) / (max - min)) * 100));
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
    const eq = value;
    if (userTouched || animRaf) return;
    displayPreamp = eq.preamp_db;
    displayBands = Array.from({ length: BAND_COUNT }, (_, i) => eq.bands_db[i] ?? 0);
  });

  function handleBandInput(index: number, e: Event) {
    userTouched = true;
    cancelAnim();
    const db = Number((e.target as HTMLInputElement).value);
    displayBands = displayBands.map((g, i) => (i === index ? db : g));
    const bands_db = [...value.bands_db];
    bands_db[index] = db;
    void settings.updateSlot(slotId, { bands_db });
  }

  function handlePreampInput(e: Event) {
    userTouched = true;
    cancelAnim();
    const db = Number((e.target as HTMLInputElement).value);
    displayPreamp = db;
    void settings.updateSlot(slotId, { preamp_db: db });
  }

  function handlePresetApplied(name: string) {
    userTouched = true;
    const p = settings.customPresets.find((x) => x.name === name);
    if (!p) return;
    animateDisplay(p.preamp_db, p.bands_db);
  }

  function handleReset() {
    userTouched = true;
    void settings.resetSlot(slotId);
    animateDisplay(0, Array(BAND_COUNT).fill(0));
  }

  onDestroy(() => cancelAnim());
</script>

<div class="equalizer">
  <div class="effect-toolbar">
    <button type="button" class="effect-reset" onclick={handleReset}>Reset</button>
    <EffectPresets
      slotId={slotId}
      kind="equalizer"
      value={value}
      onApply={handlePresetApplied}
    />
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
        <span class="eq-freq">{formatHz(BAND_FREQUENCIES[i])}</span>
      </div>
    {/each}
  </div>
</div>

<style>
  @import './Equalizer.css';
  @import './EffectPresets.css';
</style>
