<script lang="ts">
  import { getImportProgressStore } from '$lib/stores/importProgress.svelte';

  const progress = getImportProgressStore();
</script>

{#if progress.active}
  <div
    class="import-progress-bar"
    role="progressbar"
    aria-valuenow={progress.total > 0 ? Math.min(progress.current, progress.total) : undefined}
    aria-valuemin="0"
    aria-valuemax={progress.total > 0 ? progress.total : undefined}
    aria-label={progress.label || 'Importing music'}
  >
    {#if progress.total > 0}
      <div
        class="import-progress-fill"
        style:width={`${Math.max(0, Math.min(100, (progress.current / progress.total) * 100))}%`}
      ></div>
    {:else}
      <div class="import-progress-fill indeterminate"></div>
    {/if}
  </div>
{/if}

<style>
  .import-progress-bar {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 2px;
    z-index: 100;
    background: var(--bg-deep);
  }

  .import-progress-fill {
    height: 100%;
    background: var(--accent);
    transition: width 200ms linear;
    border-radius: 0 1px 1px 0;
  }

  @keyframes import-pulse {
    0% { transform: translateX(-100%); }
    100% { transform: translateX(400%); }
  }

  .import-progress-fill.indeterminate {
    animation: import-pulse 1.5s ease-in-out infinite;
    width: 30% !important;
  }
</style>
