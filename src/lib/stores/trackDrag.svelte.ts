export const trackDrag = $state({
  copyTargetPlaylistId: null as string | null,
  isCopyDrag: false,
  isDraggingTracks: false,
  draggingPaths: [] as string[],
  dropPlaylistId: null as string | null,
  dropIndex: null as number | null,
  isExportSession: false,
});

export function resetTrackDrag() {
  trackDrag.copyTargetPlaylistId = null;
  trackDrag.isCopyDrag = false;
  trackDrag.isDraggingTracks = false;
  trackDrag.draggingPaths = [];
  trackDrag.dropPlaylistId = null;
  trackDrag.dropIndex = null;
  trackDrag.isExportSession = false;
}

export function setTrackDragActive(active: boolean, copy = false) {
  trackDrag.isDraggingTracks = active;
  trackDrag.isCopyDrag = copy;
  if (!active) {
    trackDrag.copyTargetPlaylistId = null;
    trackDrag.dropPlaylistId = null;
    trackDrag.dropIndex = null;
  }
}

export function setTrackDragCopyTarget(playlistId: string | null) {
  trackDrag.copyTargetPlaylistId = playlistId;
}

export function setTrackDragTargets(playlistId: string | null, dropIndex: number | null) {
  trackDrag.dropPlaylistId = playlistId;
  trackDrag.dropIndex = dropIndex;
}

export function beginExportTrackDragUi(paths: string[], isCopy: boolean) {
  trackDrag.isDraggingTracks = true;
  trackDrag.isCopyDrag = isCopy;
  trackDrag.draggingPaths = [...paths];
  trackDrag.isExportSession = true;
  trackDrag.dropPlaylistId = null;
  trackDrag.dropIndex = null;
  trackDrag.copyTargetPlaylistId = null;
}

export function endExportTrackDragUi() {
  resetTrackDrag();
}