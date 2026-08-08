<script lang="ts">
  import { onDestroy } from 'svelte';
  import { getSettingsStore } from '$lib/stores/settings.svelte';
  import {
    EFFECT_CATALOG,
    MAX_SLOTS,
    effectMeta,
    slotSummary,
    type ChainSlot,
    type EffectKind,
  } from '$lib/dsp/effects';
  import Equalizer from './Equalizer.svelte';
  import Filter from './Filter.svelte';
  import Limiter from './Limiter.svelte';

  const settings = getSettingsStore();

  /** Rows are collapsed by default; the set holds the ids that are open. */
  let expanded = $state(new Set<string>());
  /** Boundary the drop line points at, or null while nothing is being dragged over. */
  let dropAt = $state<number | null>(null);
  let chainEl: HTMLDivElement | null = null;

  /**
   * Reordering runs on pointer events, not HTML5 drag-and-drop. The window has
   * Tauri's `dragDropEnabled`, whose OS-level drop target swallows in-page drag
   * events on Windows — `TrackList.svelte` drags by pointer for the same reason.
   */
  interface RackDrag {
    /** Set when an existing row is moving. */
    slotId: string | null;
    /** Set when a new effect is being dragged out of the catalog. */
    kind: EffectKind | null;
    label: string;
    startX: number;
    startY: number;
    x: number;
    y: number;
    /** False until the pointer clears the threshold — below it this is still a click. */
    active: boolean;
  }

  /** Slop before a press turns into a drag, so clicking a row still expands it. */
  const DRAG_THRESHOLD_PX = 4;

  let drag = $state<RackDrag | null>(null);
  /** A drag ends with a click on the row it started from; that one must not toggle. */
  let swallowClick = false;

  const chain = $derived(settings.dspChain);
  const full = $derived(chain.length >= MAX_SLOTS);

  function toggle(id: string) {
    // A drag ends with a click on the header it started from — that must not
    // also expand the row the user just moved.
    if (swallowClick) {
      swallowClick = false;
      return;
    }
    const next = new Set(expanded);
    if (!next.delete(id)) next.add(id);
    expanded = next;
  }

  async function add(kind: EffectKind, atIndex?: number) {
    const id = await settings.addEffect(kind, atIndex);
    // A freshly added effect is one the user is about to configure — open it.
    if (id) expanded = new Set(expanded).add(id);
  }

  function remove(id: string) {
    const next = new Set(expanded);
    next.delete(id);
    expanded = next;
    void settings.removeSlot(id);
  }

  /**
   * Which gap the pointer is closest to: compare against each row's midpoint, so
   * the line lands above a row in its top half and below it in its bottom half.
   */
  function boundaryAt(clientY: number): number {
    if (!chainEl) return chain.length;
    const rows = chainEl.querySelectorAll<HTMLElement>('[data-slot-row]');
    for (let i = 0; i < rows.length; i += 1) {
      const box = rows[i].getBoundingClientRect();
      if (clientY < box.top + box.height / 2) return i;
    }
    return rows.length;
  }

  function startSlotDrag(e: PointerEvent, id: string, label: string) {
    beginDrag(e, { slotId: id, kind: null, label });
  }

  function startCatalogDrag(e: PointerEvent, kind: EffectKind, label: string) {
    if (full) return;
    beginDrag(e, { slotId: null, kind, label });
  }

  function beginDrag(e: PointerEvent, seed: Pick<RackDrag, 'slotId' | 'kind' | 'label'>) {
    if (e.button !== 0) return;
    // Let the checkbox and the × button work as buttons.
    if ((e.target as HTMLElement).closest('.slot-power, .slot-remove')) return;
    drag = {
      ...seed,
      startX: e.clientX,
      startY: e.clientY,
      x: e.clientX,
      y: e.clientY,
      active: false,
    };
    window.addEventListener('pointermove', onPointerMove);
    window.addEventListener('pointerup', onPointerUp);
    window.addEventListener('pointercancel', cancelDrag);
  }

  function detachDragListeners() {
    window.removeEventListener('pointermove', onPointerMove);
    window.removeEventListener('pointerup', onPointerUp);
    window.removeEventListener('pointercancel', cancelDrag);
  }

  function cancelDrag() {
    detachDragListeners();
    drag = null;
    dropAt = null;
  }

  function onPointerMove(e: PointerEvent) {
    if (!drag) return;
    drag.x = e.clientX;
    drag.y = e.clientY;
    if (!drag.active) {
      const far =
        Math.abs(e.clientX - drag.startX) > DRAG_THRESHOLD_PX ||
        Math.abs(e.clientY - drag.startY) > DRAG_THRESHOLD_PX;
      if (!far) return;
      drag.active = true;
      document.body.classList.add('rack-dragging');
    }
    dropAt = overChain(e.clientX, e.clientY) ? boundaryAt(e.clientY) : null;
  }

  function onPointerUp() {
    if (!drag) return;
    const { slotId, kind, active } = drag;
    const at = dropAt;
    detachDragListeners();
    document.body.classList.remove('rack-dragging');
    drag = null;
    dropAt = null;

    if (!active) {
      // Never moved: a catalog press is a plain add, a row press falls through
      // to the row's own click handler, which expands it.
      if (kind) void add(kind);
      return;
    }
    swallowClick = true;
    if (at === null) return;
    if (kind) void add(kind, at);
    else if (slotId) void settings.moveSlot(slotId, at);
  }

  function overChain(x: number, y: number): boolean {
    if (!chainEl) return false;
    const box = chainEl.getBoundingClientRect();
    return x >= box.left && x <= box.right && y >= box.top && y <= box.bottom;
  }

  /** Keyboard equivalent of dragging a row — Alt+↑/↓ nudges it one place. */
  function nudge(e: KeyboardEvent, index: number, id: string) {
    if (!e.altKey || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
    e.preventDefault();
    // Boundaries, not indices: one slot further down is two gaps away, because
    // the gap right below the row is where it already sits.
    void settings.moveSlot(id, e.key === 'ArrowUp' ? index - 1 : index + 2);
  }

  function isActive(slot: ChainSlot): boolean {
    return slot.enabled && slot.settings.enabled !== false;
  }

  onDestroy(() => {
    detachDragListeners();
    document.body.classList.remove('rack-dragging');
  });
</script>

<div class="rack">
  <div class="rack-chain">
    <div class="rack-head">
      <span class="rack-title">Chain</span>
      <span class="rack-count">{chain.length}/{MAX_SLOTS}</span>
      {#if chain.length}
        <button type="button" class="rack-clear" onclick={() => void settings.clearChain()}>
          Clear
        </button>
      {/if}
    </div>

    <div class="chain-list" class:dragging={drag?.active} bind:this={chainEl} role="list">
      {#each chain as slot, i (slot.id)}
        {@const meta = effectMeta(slot.kind)}
        {#if dropAt === i}
          <div class="drop-line"></div>
        {/if}
        <div
          class="slot"
          class:open={expanded.has(slot.id)}
          class:bypassed={!slot.enabled}
          class:lifted={drag?.active && drag.slotId === slot.id}
          data-slot-row
          role="listitem"
        >
          <!-- Only the header drags: the body holds sliders, and a drag started
               there would fight the control the pointer is actually on. -->
          <div
            class="slot-head"
            role="group"
            aria-label={`${meta.label} slot ${i + 1}`}
            onpointerdown={(e) => startSlotDrag(e, slot.id, meta.label)}
          >
            <span class="slot-grip" aria-hidden="true">⠿</span>
            <span class="slot-index">{i + 1}</span>
            <label class="slot-power" title={slot.enabled ? 'Bypass this effect' : 'Enable this effect'}>
              <input
                type="checkbox"
                checked={slot.enabled}
                onchange={(e) =>
                  void settings.setSlotEnabled(slot.id, (e.target as HTMLInputElement).checked)}
              />
            </label>
            <button
              type="button"
              class="slot-main"
              onclick={() => toggle(slot.id)}
              onkeydown={(e) => nudge(e, i, slot.id)}
              aria-expanded={expanded.has(slot.id)}
            >
              <span class="slot-name">{meta.label}</span>
              <span class="slot-summary">{slotSummary(slot)}</span>
              <span class="slot-chevron" class:up={expanded.has(slot.id)}>▾</span>
            </button>
            <button
              type="button"
              class="slot-remove"
              title="Remove from chain"
              aria-label={`Remove ${meta.label}`}
              onclick={() => remove(slot.id)}
            >
              ×
            </button>
          </div>

          {#if expanded.has(slot.id)}
            <div class="slot-body">
              {#if slot.kind === 'equalizer'}
                <Equalizer slotId={slot.id} value={slot.settings} />
              {:else if slot.kind === 'filter'}
                <Filter slotId={slot.id} value={slot.settings} />
              {:else}
                <Limiter slotId={slot.id} value={slot.settings} active={isActive(slot)} />
              {/if}
            </div>
          {/if}
        </div>
      {/each}

      {#if dropAt === chain.length}
        <div class="drop-line"></div>
      {/if}

      {#if !chain.length}
        <div class="chain-empty">
          <span class="chain-empty-title">The chain is empty</span>
          <span class="chain-empty-hint">
            Drag an effect in from the right, or click it. Audio runs through the chain top to
            bottom, so the order changes the sound.
          </span>
        </div>
      {/if}
    </div>
  </div>

  <div class="rack-catalog">
    <div class="rack-head">
      <span class="rack-title">Available</span>
    </div>
    <div class="catalog-list" role="list">
      {#each EFFECT_CATALOG as effect (effect.kind)}
        <div
          class="catalog-item"
          class:disabled={full}
          class:lifted={drag?.active && drag.kind === effect.kind}
          role="listitem"
          onpointerdown={(e) => startCatalogDrag(e, effect.kind, effect.label)}
        >
          <div class="catalog-text">
            <span class="catalog-name">{effect.label}</span>
            <span class="catalog-blurb">{effect.blurb}</span>
          </div>
          <button
            type="button"
            class="catalog-add"
            disabled={full}
            title={full
              ? `The chain is full (${MAX_SLOTS} effects)`
              : `Add ${effect.label} to the end of the chain`}
            aria-label={`Add ${effect.label}`}
            onclick={() => void add(effect.kind)}
          >
            +
          </button>
        </div>
      {/each}
    </div>
    {#if full}
      <p class="catalog-note">Chain is full — remove an effect to add another.</p>
    {:else}
      <p class="catalog-note">Drag one into the chain, or click it to append.</p>
    {/if}
  </div>
</div>

{#if drag?.active}
  <div class="rack-ghost" style={`left: ${drag.x}px; top: ${drag.y}px`}>
    {drag.label}
  </div>
{/if}

<style>
  @import './DspChain.css';
</style>
