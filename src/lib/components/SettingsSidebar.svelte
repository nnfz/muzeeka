<script lang="ts">
  export type Section = 'general' | 'downloads' | 'plugins' | 'audio' | 'developer' | 'about';

  let {
    activeSection = $bindable<Section>('general'),
    showDeveloper = false,
  }: {
    activeSection?: Section;
    showDeveloper?: boolean;
  } = $props();

  const allSections: { id: Section; label: string; icon: Section }[] = [
    { id: 'general', label: 'General', icon: 'general' },
    { id: 'downloads', label: 'Downloads', icon: 'downloads' },
    { id: 'plugins', label: 'Plugins', icon: 'plugins' },
    { id: 'audio', label: 'Audio', icon: 'audio' },
    { id: 'developer', label: 'Development', icon: 'developer' },
    { id: 'about', label: 'About', icon: 'about' },
  ];

  const sections = $derived(
    showDeveloper ? allSections : allSections.filter((s) => s.id !== 'developer'),
  );

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
          {:else if section.icon === 'plugins'}
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
              <path d="M12 2v4" />
              <path d="M12 18v4" />
              <path d="M4.93 4.93l2.83 2.83" />
              <path d="M16.24 16.24l2.83 2.83" />
              <path d="M2 12h4" />
              <path d="M18 12h4" />
              <path d="M4.93 19.07l2.83-2.83" />
              <path d="M16.24 7.76l2.83-2.83" />
            </svg>
          {:else if section.icon === 'audio'}
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
              <path d="M4 10v4M8 7v10M12 4v16M16 7v10M20 10v4" />
            </svg>
          {:else if section.icon === 'developer'}
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
              <polyline points="4 17 10 11 4 5" />
              <line x1="12" y1="19" x2="20" y2="19" />
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
