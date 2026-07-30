<script lang="ts">
  export type Section = 'general' | 'downloads' | 'remote' | 'audio' | 'about';

  let { activeSection = $bindable<Section>('general') }: {
    activeSection?: Section;
  } = $props();

  const sections: { id: Section; label: string; icon: 'general' | 'downloads' | 'remote' | 'audio' | 'about' }[] = [
    { id: 'general', label: 'General', icon: 'general' },
    { id: 'downloads', label: 'Downloads', icon: 'downloads' },
    { id: 'remote', label: 'Remote', icon: 'remote' },
    { id: 'audio', label: 'Audio', icon: 'audio' },
    { id: 'about', label: 'About', icon: 'about' },
  ];

  function select(id: Section) {
    activeSection = id;
  }
</script>

<aside class="settings-sidebar glass">
  <div class="settings-nav">
    {#each sections as section (section.id)}
      <button
        type="button"
        class="nav-item"
        class:active={activeSection === section.id}
        onclick={() => select(section.id)}
      >
        <span class="nav-icon" aria-hidden="true">
          {#if section.icon === 'general'}
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M12 1v2M12 21v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M1 12h2M21 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42" />
            </svg>
          {:else if section.icon === 'downloads'}
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
              <path d="M12 3v12" />
              <path d="M7 10l5 5 5-5" />
              <path d="M5 21h14" />
            </svg>
          {:else if section.icon === 'remote'}
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
              <rect x="7" y="2" width="10" height="20" rx="2" />
              <path d="M11 18h2" />
              <path d="M9 6h6" />
            </svg>
          {:else if section.icon === 'audio'}
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
              <path d="M4 10v4M8 7v10M12 4v16M16 7v10M20 10v4" />
            </svg>
          {:else}
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
              <circle cx="12" cy="12" r="9" />
              <path d="M12 16v-4" />
              <path d="M12 8h.01" />
            </svg>
          {/if}
        </span>
        <span class="nav-label">{section.label}</span>
      </button>
    {/each}
  </div>
</aside>

<style>
  @import './SettingsSidebar.css';
</style>
