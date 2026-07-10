export const trackDrag = $state({
  copyTargetPlaylistId: null as string | null,
  isCopyDrag: false,
  isDraggingTracks: false,
});

export function resetTrackDrag() {
  trackDrag.copyTargetPlaylistId = null;
  trackDrag.isCopyDrag = false;
  trackDrag.isDraggingTracks = false;
}

export function setTrackDragActive(active: boolean, copy = false) {
  trackDrag.isDraggingTracks = active;
  trackDrag.isCopyDrag = copy;
  if (!active) {
    trackDrag.copyTargetPlaylistId = null;
  }
}

export function setTrackDragCopyTarget(playlistId: string | null) {
  trackDrag.copyTargetPlaylistId = playlistId;
}