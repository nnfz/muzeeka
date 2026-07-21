<script lang="ts">
  import { getImportProgressStore } from '$lib/stores/importProgress.svelte';

  const progress = getImportProgressStore();
</script>

<div class="import-progress-bar" role="progressbar" aria-valuenow={progress.current} aria-valuemax={progress.total} aria-label="Importing...">
  {#if progress.total > 0}
    <div class="import-progress-fill" style="width: {(progress.current / progress.total) * 100}%"></div>
  {:else if progress.active}
    <div class="import-progress-fill indeterminate"></div>
  {/if}
</div>

<style>
  .import-progress-bar {
    position: sticky;
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

  .import-progress-fill.indeterminate {
    animation: import-pulse 1.5s ease-in-out infinite;
    width: 30% !important;
  }
</style>
