import { invoke } from '@tauri-apps/api/core';
import type { MusicFile } from '$lib/stores/player.svelte';
import { exportAudioPathForTrack } from '$lib/trackPaths';

export async function startFileDrag(paths: string[], iconPath?: string | null): Promise<void> {
  const unique = [...new Set(paths.filter(Boolean))];
  if (unique.length === 0) return;

  await invoke('start_file_drag', {
    paths: unique,
    icon_path: iconPath ?? null,
  });
}

export function audioPathsForDrag(
  paths: string[],
  resolveTrack: (path: string) => MusicFile | undefined,
): string[] {
  const out: string[] = [];
  for (const path of paths) {
    const track = resolveTrack(path);
    const audio = exportAudioPathForTrack(track ?? null, path);
    if (audio) out.push(audio);
  }
  return [...new Set(out)];
}