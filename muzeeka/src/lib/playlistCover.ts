import type { MusicFile, Playlist } from '$lib/stores/player.svelte';

/** Track-shaped object for `TrackCover` — custom cover or first track that has art. */
export function resolvePlaylistCoverTrack(playlist: Playlist): MusicFile | null {
  const custom = playlist.cover_path?.trim();
  if (custom) {
    return {
      path: '',
      file_name: '',
      cover_path: custom,
    };
  }

  for (const track of playlist.tracks) {
    if (track.cover_path?.trim()) {
      return track;
    }
  }

  return null;
}

export function collectPlaylistCoverPaths(playlists: Playlist[]): string[] {
  const paths: string[] = [];
  for (const playlist of playlists) {
    const custom = playlist.cover_path?.trim();
    if (custom) {
      paths.push(custom);
      continue;
    }
    for (const track of playlist.tracks) {
      const cover = track.cover_path?.trim();
      if (cover) {
        paths.push(cover);
        break;
      }
    }
  }
  return paths;
}