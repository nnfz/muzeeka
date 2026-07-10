import type { MusicFile } from '$lib/stores/player.svelte';

/** Resolve the on-disk audio file path for sharing / drag-out. */
export function exportAudioPathForTrack(
  track: MusicFile | null | undefined,
  filePath: string | null | undefined,
): string | null {
  if (!filePath) return null;

  if (track?.audio_path) return track.audio_path;

  const cueMarker = '#cue:';
  const markerPos = filePath.lastIndexOf(cueMarker);
  if (markerPos > 0) return filePath.slice(0, markerPos);

  return filePath;
}