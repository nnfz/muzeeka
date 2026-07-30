import { initialize, isSupported } from 'tauri-plugin-taskbar';

let setupDone = false;

/** Attach Windows taskbar thumbnail controls once at startup. */
export async function setupTaskbar(): Promise<void> {
  if (setupDone) return;
  setupDone = true;

  try {
    if (!(await isSupported())) return;
    await initialize();
  } catch (error) {
    console.error('Taskbar controls unavailable:', error);
  }
}