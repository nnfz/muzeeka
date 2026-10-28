use std::collections::HashSet;
use std::hash::Hash;

pub(crate) fn reorder_at_boundary<T>(items: &[T], moving: &[T], insert_index: usize) -> Vec<T>
where
    T: Clone + Eq + Hash,
{
    let moving_set: HashSet<&T> = moving.iter().collect();
    let moving_items: Vec<T> = items
        .iter()
        .filter(|item| moving_set.contains(item))
        .cloned()
        .collect();
    if moving_items.is_empty() {
        return items.to_vec();
    }

    let original_at = insert_index.min(items.len());
    let removed_before = items
        .iter()
        .take(original_at)
        .filter(|item| moving_set.contains(item))
        .count();
    let remaining: Vec<T> = items
        .iter()
        .filter(|item| !moving_set.contains(item))
        .cloned()
        .collect();
    let insert_at = original_at
        .saturating_sub(removed_before)
        .min(remaining.len());
    let mut next = Vec::with_capacity(items.len());
    next.extend(remaining[..insert_at].iter().cloned());
    next.extend(moving_items);
    next.extend(remaining[insert_at..].iter().cloned());
    next
}

pub(crate) fn slot_from_y(y: f32, count: usize, row_h: f32) -> usize {
    if count == 0 || row_h <= 0. {
        return 0;
    }
    let raw = (y / row_h).round();
    if raw <= 0. {
        0
    } else {
        (raw as usize).min(count)
    }
}

pub(crate) fn stabilize_slot(
    next: usize,
    prev: Option<usize>,
    y: f32,
    row_h: f32,
    hysteresis: f32,
) -> usize {
    let Some(prev) = prev else {
        return next;
    };
    if prev == next || row_h <= 0. {
        return next;
    }
    let low = prev.min(next) as f32;
    let boundary = (low + 0.5) * row_h;
    let dist = y - boundary;
    if next > prev && dist < hysteresis {
        return prev;
    }
    if next < prev && dist > -hysteresis {
        return prev;
    }
    next
}

pub(crate) fn single_row_shift(row: usize, from: usize, slot: usize) -> i32 {
    let removed_before = usize::from(from < slot);
    let dest = slot.saturating_sub(removed_before);
    if row == from {
        return dest as i32 - row as i32;
    }
    let rank = if row > from { row - 1 } else { row };
    let new_index = if rank < dest { rank } else { rank + 1 };
    new_index as i32 - row as i32
}

pub(crate) fn index_after_move(row: usize, from: usize, slot: usize, count: usize) -> usize {
    if count == 0 || from >= count || row >= count {
        return row.min(count.saturating_sub(1));
    }
    let shifted = row as i32 + single_row_shift(row, from, slot);
    if shifted <= 0 {
        0
    } else {
        (shifted as usize).min(count - 1)
    }
}

pub(crate) fn playlist_slot(
    y: f32,
    centers: &[f32],
    prev: Option<usize>,
    hysteresis: f32,
) -> usize {
    let mut index = 0;
    while index < centers.len() && y > centers[index] {
        index += 1;
    }
    let Some(prev) = prev else {
        return index;
    };
    if prev == index || centers.is_empty() {
        return index;
    }
    let boundary = centers[prev.min(index).min(centers.len() - 1)];
    if index > prev && y - boundary < hysteresis {
        return prev;
    }
    if index < prev && boundary - y < hysteresis {
        return prev;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::{
        index_after_move, playlist_slot, reorder_at_boundary, single_row_shift, slot_from_y,
        stabilize_slot,
    };

    #[test]
    fn moving_a_row_down_accounts_for_the_hole_it_left() {
        let items = ["a", "b", "c", "d"];
        assert_eq!(
            reorder_at_boundary(&items, &["a"], 3),
            vec!["b", "c", "a", "d"]
        );
    }

    #[test]
    fn moving_a_row_up_inserts_at_the_boundary() {
        let items = ["a", "b", "c", "d"];
        assert_eq!(
            reorder_at_boundary(&items, &["d"], 0),
            vec!["d", "a", "b", "c"]
        );
    }

    #[test]
    fn a_block_keeps_its_original_order() {
        let items = ["a", "b", "c", "d"];
        assert_eq!(
            reorder_at_boundary(&items, &["c", "b"], 4),
            vec!["a", "d", "b", "c"]
        );
    }

    #[test]
    fn dropping_on_the_same_slot_is_a_no_op() {
        let items = ["a", "b", "c"];
        assert_eq!(reorder_at_boundary(&items, &["b"], 1), items);
        assert_eq!(reorder_at_boundary(&items, &["b"], 2), items);
    }

    #[test]
    fn track_slots_round_to_the_nearest_boundary() {
        assert_eq!(slot_from_y(0., 4, 52.), 0);
        assert_eq!(slot_from_y(20., 4, 52.), 0);
        assert_eq!(slot_from_y(26., 4, 52.), 1);
        assert_eq!(slot_from_y(200., 4, 52.), 4);
    }

    #[test]
    fn hysteresis_holds_the_slot_on_the_boundary() {
        assert_eq!(stabilize_slot(2, Some(1), 78., 52., 14.), 1);
        assert_eq!(stabilize_slot(2, Some(1), 92., 52., 14.), 2);
    }

    #[test]
    fn single_row_shift_matches_the_reordered_indexes() {
        let count = 5;
        for from in 0..count {
            for slot in 0..=count {
                let ids: Vec<_> = (0..count).collect();
                let next = reorder_at_boundary(&ids, &[from], slot);
                for row in 0..count {
                    let expected = next.iter().position(|id| *id == row).unwrap();
                    assert_eq!(
                        index_after_move(row, from, slot, count),
                        expected,
                        "row {row} from {from} slot {slot}"
                    );
                    assert_eq!(
                        row as i32 + single_row_shift(row, from, slot),
                        expected as i32,
                        "shift row {row} from {from} slot {slot}"
                    );
                }
            }
        }
    }

    #[test]
    fn playlist_slots_use_row_centers() {
        let centers = [25., 75., 125.];
        assert_eq!(playlist_slot(10., &centers, None, 4.), 0);
        assert_eq!(playlist_slot(30., &centers, None, 4.), 1);
        assert_eq!(playlist_slot(200., &centers, None, 4.), 3);
        assert_eq!(playlist_slot(27., &centers, Some(0), 4.), 0);
        assert_eq!(playlist_slot(30., &centers, Some(0), 4.), 1);
    }
}
