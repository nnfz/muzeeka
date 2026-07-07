<script lang="ts">
  export type Section = 'general' | 'audio' | 'about';

  let { activeSection = $bindable<Section>('general') }: {
    activeSection?: Section;
  } = $props();

  const sections: { id: Section; label: string; desc?: string }[] = [
    { id: 'general', label: 'General', desc: 'App info & behavior' },
    { id: 'audio', label: 'Audio', desc: 'Equalizer + playback speed' },
    { id: 'about', label: 'About', desc: 'Muzeeka & credits' },
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
    <div class="hint">Changes apply live • Settings auto-save</div>
  </div>
</aside>

<style>
  @import './SettingsSidebar.css';
</style>
