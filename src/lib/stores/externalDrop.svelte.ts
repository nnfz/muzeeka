export type ExternalDropZone = 'none' | 'sidebar' | 'tracks';

export const externalDrop = $state({
  active: false,
  zone: 'none' as ExternalDropZone,
  targetPlaylistId: null as string | null,
  targetPlaylistName: null as string | null,
  ctrlHeld: false,
});

export function resetExternalDrop() {
  externalDrop.active = false;
  externalDrop.zone = 'none';
  externalDrop.targetPlaylistId = null;
  externalDrop.targetPlaylistName = null;
  externalDrop.ctrlHeld = false;
}

export function setExternalDropActive(active: boolean) {
  externalDrop.active = active;
  if (!active) {
    externalDrop.zone = 'none';
    externalDrop.targetPlaylistId = null;
    externalDrop.targetPlaylistName = null;
  }
}

export function setExternalDropHover(options: {
  zone: ExternalDropZone;
  playlistId?: string | null;
  playlistName?: string | null;
  ctrlHeld?: boolean;
}) {
  externalDrop.zone = options.zone;
  externalDrop.targetPlaylistId = options.playlistId ?? null;
  externalDrop.targetPlaylistName = options.playlistName ?? null;
  if (options.ctrlHeld !== undefined) {
    externalDrop.ctrlHeld = options.ctrlHeld;
  }
}

export function setExternalDropCtrl(ctrlHeld: boolean) {
  externalDrop.ctrlHeld = ctrlHeld;
}
