export interface ContextMenuItem {
  id: string;
  label: string;
  danger?: boolean;
  disabled?: boolean;
  icon?: 'rename' | 'delete' | 'heart';
  onSelect: () => void;
}

export interface ContextMenuPosition {
  x: number;
  y: number;
}

export interface ContextMenuSize {
  width: number;
  height: number;
}

const DEFAULT_MENU_SIZE: ContextMenuSize = { width: 168, height: 80 };

export function getContextMenuPosition(
  event: MouseEvent,
  size: ContextMenuSize = DEFAULT_MENU_SIZE
): ContextMenuPosition {
  const x = Math.min(event.clientX, window.innerWidth - size.width - 8);
  const y = Math.min(event.clientY, window.innerHeight - size.height - 8);

  return {
    x: Math.max(8, x),
    y: Math.max(8, y),
  };
}

export function openContextMenuFromEvent(
  event: MouseEvent,
  size: ContextMenuSize = DEFAULT_MENU_SIZE
): ContextMenuPosition {
  event.preventDefault();
  event.stopPropagation();
  return getContextMenuPosition(event, size);
}