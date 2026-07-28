export function reorderItemsAtBoundary<T>(
  items: readonly T[],
  movingPaths: readonly string[],
  insertIndex: number,
  pathOf: (item: T) => string,
): T[] {
  const movingSet = new Set(movingPaths);
  const moving = items.filter((item) => movingSet.has(pathOf(item)));
  if (moving.length === 0) return [...items];

  // The drop boundary belongs to the original list. Account for moving rows
  // removed before that boundary before inserting them again.
  const originalInsertAt = Math.max(0, Math.min(insertIndex, items.length));
  const removedBefore = items
    .slice(0, originalInsertAt)
    .filter((item) => movingSet.has(pathOf(item))).length;
  const remaining = items.filter((item) => !movingSet.has(pathOf(item)));
  const insertAt = Math.max(
    0,
    Math.min(originalInsertAt - removedBefore, remaining.length),
  );

  return [
    ...remaining.slice(0, insertAt),
    ...moving,
    ...remaining.slice(insertAt),
  ];
}
