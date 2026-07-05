<script lang="ts">
  import { getSettingsStore } from '$lib/stores/settings.svelte';

  const settings = getSettingsStore();

  type Section = 'equalizer';

  let { activeSection = $bindable<'equalizer'>('equalizer') }: {
    activeSection?: Section;
  } = $props();

  const sections: { id: Section; label: string; desc?: string }[] = [
    { id: 'equalizer', label: 'Equalizer', desc: 'Graphic EQ presets & bands' },
  ];

  function select(id: Section) {
    activeSection = id;
  }
</script>

<aside class="settings-sidebar glass">
  <div class="sidebar-header">
    <div class="section-label">Settings</div>
  </div>

  <div class="settings-nav">
    {#each sections as section (section.id)}
      <button
        class="nav-item"
        class:active={activeSection === section.id}
        onclick={() => select(section.id)}
      >
        <span class="nav-label">{section.label}</span>
        {#if section.desc}
          <span class="nav-desc">{section.desc}</span>
        {/if}
      </button>
    {/each}
  </div>

  <div class="sidebar-footer">
    <div class="hint">EQ changes apply live</div>
  </div>
</aside>

<style>
  @import './SettingsSidebar.css';
</style>
