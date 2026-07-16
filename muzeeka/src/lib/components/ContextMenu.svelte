<script lang="ts">
  import type { ContextMenuItem } from '$lib/contextMenu';

  interface Props {
    open: boolean;
    x: number;
    y: number;
    items: ContextMenuItem[];
    onclose: () => void;
  }

  let { open, x, y, items, onclose }: Props = $props();

  function handleSelect(item: ContextMenuItem) {
    if (item.disabled) return;
    item.onSelect();
    if (item.closeOnSelect !== false) onclose();
  }
</script>

<svelte:window
  onclick={() => open && onclose()}
  onkeydown={(e) => open && e.key === 'Escape' && onclose()}
/>

{#if open}
  <div
    class="context-menu"
    style:left="{x}px"
    style:top="{y}px"
    role="menu"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    oncontextmenu={(e) => e.stopPropagation()}
  >
    {#each items as item (item.id)}
      <button
        type="button"
        class="context-menu-item"
        class:danger={item.danger}
        role="menuitem"
        disabled={item.disabled}
        onclick={() => handleSelect(item)}
      >
        {#if item.icon}
          <span class="context-menu-item-icon" aria-hidden="true">
            {#if item.icon === 'rename'}
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M12 20h9"/>
                <path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"/>
              </svg>
            {:else if item.icon === 'delete'}
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <polyline points="3 6 5 6 21 6"/>
                <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>
              </svg>
            {:else if item.icon === 'heart'}
              <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
                <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
              </svg>
            {:else if item.icon === 'folder'}
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />
              </svg>
            {:else if item.icon === 'playlist'}
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <line x1="8" y1="6" x2="21" y2="6" />
                <line x1="8" y1="12" x2="21" y2="12" />
                <line x1="8" y1="18" x2="15" y2="18" />
                <line x1="18" y1="16" x2="18" y2="22" />
                <line x1="15" y1="19" x2="21" y2="19" />
                <circle cx="3.5" cy="6" r="0.5" />
                <circle cx="3.5" cy="12" r="0.5" />
                <circle cx="3.5" cy="18" r="0.5" />
              </svg>
            {:else if item.icon === 'image'}
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <rect x="3" y="3" width="18" height="18" rx="2" />
                <circle cx="8.5" cy="8.5" r="1.5" />
                <path d="m21 15-5-5L5 21" />
              </svg>
            {/if}
          </span>
        {/if}
        {item.label}
      </button>
    {/each}
  </div>
{/if}

<style>
  @import './ContextMenu.css';
</style>