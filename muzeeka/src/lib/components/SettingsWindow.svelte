<script lang="ts">
  import '../../app.css';
  import '../../routes/+page.css';
  import WindowControls from './WindowControls.svelte';
  import SettingsSidebar from './SettingsSidebar.svelte';
  import Equalizer from './Equalizer.svelte';
  import { getSettingsStore } from '$lib/stores/settings.svelte';

  const settings = getSettingsStore();

  let activeSection: 'equalizer' = $state('equalizer');
</script>

<div class="settings-window">
  <header class="app-header glass" data-tauri-drag-region>
    <div class="settings-win-title" data-tauri-drag-region>Settings</div>
    <div class="app-header-spacer" data-tauri-drag-region></div>
    <WindowControls showMinimize={false} showMaximize={false} />
  </header>

  <div class="settings-layout">
    <SettingsSidebar bind:activeSection />

    <div class="settings-content">
      {#if activeSection === 'equalizer'}
        <div class="settings-section">
          <h2 class="section-title">Equalizer</h2>
          <p class="section-desc">
            15-band 1/3-octave graphic EQ with 32-bit floating-point DSP processing.
          </p>
          <Equalizer />
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  @import './SettingsWindow.css';
</style>
