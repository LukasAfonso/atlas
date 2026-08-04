use std::collections::{HashMap, HashSet};

use eframe::egui::Pos2;

use crate::board::GRID_SPACING;
use crate::vault::NoteId;

pub(super) fn slots_to_positions(slots: HashMap<NoteId, (i32, i32)>) -> HashMap<NoteId, Pos2> {
    slots
        .into_iter()
        .map(|(note_id, slot)| (note_id, slot_position(slot)))
        .collect()
}

pub(super) fn slot_position(slot: (i32, i32)) -> Pos2 {
    Pos2::new(
        slot.0 as f32 * GRID_SPACING.x,
        slot.1 as f32 * GRID_SPACING.y,
    )
}

pub(super) fn position_slot(position: Pos2) -> (i32, i32) {
    (
        (position.x / GRID_SPACING.x).round() as i32,
        (position.y / GRID_SPACING.y).round() as i32,
    )
}

pub(super) fn coarse_slot(position: Pos2, stride: (f32, f32)) -> (i32, i32) {
    (
        (position.x / stride.0).round() as i32,
        (position.y / stride.1).round() as i32,
    )
}

pub(super) fn nearest_free_coarse_slot(
    desired: (i32, i32),
    occupied: &HashSet<(i32, i32)>,
) -> (i32, i32) {
    for radius in 0_i32.. {
        if let Some(slot) = ring_by_distance(desired, radius)
            .into_iter()
            .find(|slot| !occupied.contains(slot))
        {
            return slot;
        }
    }
    unreachable!("an infinite grid always contains a free slot")
}

pub(super) fn ring_by_distance(origin: (i32, i32), radius: i32) -> Vec<(i32, i32)> {
    let mut slots = square_ring(origin, radius);
    slots.sort_by_key(|slot| {
        let dx = slot.0 - origin.0;
        let dy = slot.1 - origin.1;
        (dx * dx + dy * dy, slot.1, slot.0)
    });
    slots
}

fn square_ring(origin: (i32, i32), radius: i32) -> Vec<(i32, i32)> {
    if radius == 0 {
        return vec![origin];
    }
    let mut slots = Vec::with_capacity(radius as usize * 8);
    for x in -radius..=radius {
        slots.push((origin.0 + x, origin.1 - radius));
        slots.push((origin.0 + x, origin.1 + radius));
    }
    for y in (-radius + 1)..radius {
        slots.push((origin.0 - radius, origin.1 + y));
        slots.push((origin.0 + radius, origin.1 + y));
    }
    slots
}

pub(super) fn compact_offset(index: usize) -> (i32, i32) {
    if index == 0 {
        return (0, 0);
    }
    let mut remaining = index;
    for radius in 1_i32.. {
        let ring = ring_by_distance((0, 0), radius);
        if remaining <= ring.len() {
            return ring[remaining - 1];
        }
        remaining -= ring.len();
    }
    unreachable!("an infinite grid always contains an unused offset")
}

pub(super) fn compact_side_length(member_count: usize) -> i32 {
    let side = (member_count as f32).sqrt().ceil().max(1.0) as i32;
    if side % 2 == 0 { side + 1 } else { side }
}

#[cfg(test)]
mod tests {
    use super::{compact_offset, compact_side_length};
    use std::collections::HashSet;

    #[test]
    fn compact_offsets_do_not_repeat() {
        let offsets: HashSet<_> = (0..1_000).map(compact_offset).collect();
        assert_eq!(offsets.len(), 1_000);
    }

    #[test]
    fn shelf_footprints_match_center_out_note_packing() {
        assert_eq!(compact_side_length(1), 1);
        assert_eq!(compact_side_length(9), 3);
        assert_eq!(compact_side_length(10), 5);
        assert_eq!(compact_side_length(64), 9);
        assert_eq!(compact_side_length(100), 11);
    }
}
