import {
  getPlayerStore,
  isEditablePlaylist,
  supportsPlaylistReorder,
  VIRTUAL_ALL_ID,
  VIRTUAL_LIKED_ID,
} from '$lib/stores/player.svelte';

export interface TrackDropSession {
  paths: string[];
  sourcePlaylistId: string;
  isCopy: boolean;
}

export interface TrackDropTarget {
  dropPlaylistId: string | null;
  dropIndex: number | null;
}

export function hitTestTrackDrop(
  x: number,
  y: number,
  sourcePlaylistId: string,
  canReorder: boolean,
): TrackDropTarget {
  const el = document.elementFromPoint(x, y);
  const playlistId = el?.closest('[data-playlist-id]')?.getAttribute('data-playlist-id') ?? null;
  const validPlaylistTarget =
    playlistId &&
    playlistId !== sourcePlaylistId &&
    isEditablePlaylist(playlistId);

  if (validPlaylistTarget) {
    return { dropPlaylistId: playlistId, dropIndex: null };
  }

  if (canReorder) {
    const row = el?.closest('[data-track-index]');
    if (row) {
      const idx = Number(row.getAttribute('data-track-index'));
      const rect = row.getBoundingClientRect();
      const before = y < rect.top + rect.height / 2;
      return { dropPlaylistId: null, dropIndex: before ? idx : idx + 1 };
    }
    const rows = document.querySelectorAll('.track-rows [data-track-index]');
    return { dropPlaylistId: null, dropIndex: rows.length };
  }

  return { dropPlaylistId: null, dropIndex: null };
}

function reorderTracksInView(
  player: ReturnType<typeof getPlayerStore>,
  playlistId: string,
  paths: string[],
  insertIndex: number,
) {
  const tracks = player.tracks;
  const movingSet = new Set(paths);
  const moving = tracks.filter((track) => movingSet.has(track.path));
  if (moving.length === 0) return;

  const remaining = tracks.filter((track) => !movingSet.has(track.path));
  const insertAt = Math.max(0, Math.min(insertIndex, remaining.length));
  const newOrder = [
    ...remaining.slice(0, insertAt),
    ...moving,
    ...remaining.slice(insertAt),
  ];

  if (playlistId === VIRTUAL_LIKED_ID || playlistId === VIRTUAL_ALL_ID) {
    player.reorderTracksInView(playlistId, paths, insertAt);
  } else {
    player.setPlaylistTrackOrder(playlistId, newOrder);
  }
}

export function applyTrackDrop(
  player: ReturnType<typeof getPlayerStore>,
  session: TrackDropSession,
  target: TrackDropTarget,
  onToast: (message: string) => void,
): void {
  const { paths, sourcePlaylistId, isCopy, dropPlaylistId, dropIndex } = {
    ...session,
    ...target,
  };
  const canReorder = supportsPlaylistReorder(sourcePlaylistId);

  if (
    dropPlaylistId &&
    isEditablePlaylist(dropPlaylistId) &&
    dropPlaylistId !== sourcePlaylistId
  ) {
    const playlistTarget = player.playlists.find((p) => p.id === dropPlaylistId)?.name ?? 'playlist';
    if (isCopy) {
      const added = player.copyTracksToPlaylist(paths, dropPlaylistId, sourcePlaylistId);
      if (added > 0) {
        onToast(`Copied ${added} track${added !== 1 ? 's' : ''} to ${playlistTarget}`);
      }
    } else {
      const moved = player.moveTracksToPlaylist(paths, dropPlaylistId, sourcePlaylistId);
      if (moved > 0) {
        onToast(`Moved ${moved} track${moved !== 1 ? 's' : ''} to ${playlistTarget}`);
      }
    }
    return;
  }

  if (!isCopy && canReorder && dropIndex !== null) {
    reorderTracksInView(player, sourcePlaylistId, paths, dropIndex);
  }
}

