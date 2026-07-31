<script lang="ts" module>
  export interface DropdownTriggerProps {
    open: boolean;
    toggle: (e?: MouseEvent) => void;
  }
</script>

<script lang="ts">
  import type { Snippet } from 'svelte';
  import './Dropdown.css';

  let {
    trigger,
    menu,
    open = $bindable(false),
    class: className = '',
    align = 'left',
  }: {
    trigger: Snippet<[DropdownTriggerProps]>;
    menu: Snippet;
    open?: boolean;
    class?: string;
    align?: 'left' | 'right';
  } = $props();

  let root: HTMLDivElement | undefined = $state();

  function toggle(e?: MouseEvent) {
    e?.stopPropagation();
    open = !open;
  }

  function handleGlobalClick(e: MouseEvent) {
    if (!open || !root) return;
    if (!e.composedPath().includes(root)) {
      open = false;
    }
  }

  function handleGlobalKey(e: KeyboardEvent) {
    if (open && e.key === 'Escape') {
      open = false;
    }
  }
</script>

<svelte:window onclick={handleGlobalClick} onkeydown={handleGlobalKey} />

<div class={`dropdown ${className}`} bind:this={root}>
  {@render trigger({ open, toggle })}
  {#if open}
    <div class={`dropdown-menu ${align === 'right' ? 'align-right' : ''}`} role="listbox">
      {@render menu()}
    </div>
  {/if}
</div>