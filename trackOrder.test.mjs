import assert from 'node:assert/strict';
import test from 'node:test';

import { reorderItemsAtBoundary } from './src/lib/trackOrder.ts';

const reorder = (items, moving, boundary) =>
  reorderItemsAtBoundary(items, moving, boundary, (item) => item);

test('moving down inserts at the visible drop boundary', () => {
  assert.deepEqual(reorder(['1', '2', '3', '4'], ['2'], 3), ['1', '3', '2', '4']);
  assert.deepEqual(reorder(['1', '2', '3', '4'], ['2'], 4), ['1', '3', '4', '2']);
});

test('moving up and moving multiple rows preserve their relative order', () => {
  assert.deepEqual(reorder(['1', '2', '3', '4'], ['3'], 1), ['1', '3', '2', '4']);
  assert.deepEqual(
    reorder(['1', '2', '3', '4', '5'], ['2', '3'], 5),
    ['1', '4', '5', '2', '3'],
  );
});
