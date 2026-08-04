use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use eframe::egui::{Pos2, Vec2};

use super::grid::{nearest_free_coarse_slot, position_slot};
use super::{NoteTags, Seeding, note_sort_key};
use crate::vault::{NoteId, NoteRecord};

const REFERENCE_PULL: f32 = 0.20;

pub(super) fn desired_note_slots(
    notes: &[&NoteRecord],
    note_tags: &NoteTags,
    tag_centers: &HashMap<String, Pos2>,
    seeding: &Seeding<'_>,
) -> HashMap<NoteId, (i32, i32)> {
    let mut anchors = HashMap::new();
    for note in notes {
        let anchor = seeding.preserved_position(&note.id).unwrap_or_else(|| {
            let tags = &note_tags[&note.id];
            let sum = tags
                .iter()
                .fold(Vec2::ZERO, |sum, (tag, _)| sum + tag_centers[tag].to_vec2());
            Pos2::ZERO + sum / tags.len() as f32
        });
        anchors.insert(note.id.clone(), anchor);
    }

    notes
        .iter()
        .map(|note| {
            if seeding.is_unchanged(&note.id) {
                return (note.id.clone(), position_slot(anchors[&note.id]));
            }
            let neighbors: BTreeSet<_> = note
                .references
                .iter()
                .chain(note.backlinks.iter())
                .filter(|note_id| anchors.contains_key(*note_id))
                .collect();
            let anchor = if neighbors.is_empty() {
                anchors[&note.id]
            } else {
                let sum = neighbors
                    .iter()
                    .fold(Vec2::ZERO, |sum, note_id| sum + anchors[*note_id].to_vec2());
                anchors[&note.id].lerp(Pos2::ZERO + sum / neighbors.len() as f32, REFERENCE_PULL)
            };
            (note.id.clone(), position_slot(anchor))
        })
        .collect()
}

pub(super) fn spread_shared_anchors(
    notes: &[&NoteRecord],
    note_tags: &NoteTags,
    mut desired: HashMap<NoteId, (i32, i32)>,
    seeding: &Seeding<'_>,
) -> HashMap<NoteId, (i32, i32)> {
    let mut groups: BTreeMap<(i32, i32), Vec<&NoteRecord>> = BTreeMap::new();
    for note in notes {
        groups.entry(desired[&note.id]).or_default().push(note);
    }
    for (anchor, group) in groups {
        if group.len() <= 1 {
            continue;
        }
        let mut movable: Vec<_> = group
            .into_iter()
            .filter(|note| !seeding.is_unchanged(&note.id))
            .collect();
        if movable.is_empty() {
            continue;
        }
        movable.sort_by(|left, right| {
            let left_degree = left.references.len() + left.backlinks.len();
            let right_degree = right.references.len() + right.backlinks.len();
            note_tags[&right.id]
                .len()
                .cmp(&note_tags[&left.id].len())
                .then_with(|| right_degree.cmp(&left_degree))
                .then_with(|| note_sort_key(left).cmp(&note_sort_key(right)))
        });
        let group_tags: BTreeSet<&str> = movable
            .iter()
            .flat_map(|note| note_tags[&note.id].iter().map(|(key, _)| key.as_str()))
            .collect();
        let mut occupied: HashSet<(i32, i32)> = notes
            .iter()
            .filter(|note| {
                seeding.is_unchanged(&note.id)
                    && note_tags[&note.id]
                        .iter()
                        .any(|(key, _)| group_tags.contains(key.as_str()))
            })
            .map(|note| desired[&note.id])
            .collect();
        for note in movable {
            let slot = nearest_free_coarse_slot(anchor, &occupied);
            occupied.insert(slot);
            desired.insert(note.id.clone(), slot);
        }
    }
    desired
}

pub(super) fn pack_rows(
    notes: &[&NoteRecord],
    desired: &HashMap<NoteId, (i32, i32)>,
    seeding: &Seeding<'_>,
) -> HashMap<NoteId, (i32, i32)> {
    let mut rows: BTreeMap<i32, Vec<&NoteRecord>> = BTreeMap::new();
    for note in notes {
        rows.entry(desired[&note.id].1).or_default().push(note);
    }
    let mut assigned = HashMap::with_capacity(notes.len());
    for (row, mut row_notes) in rows {
        row_notes.sort_by(|left, right| {
            desired[&left.id]
                .0
                .cmp(&desired[&right.id].0)
                .then_with(|| {
                    seeding
                        .weight(&right.id)
                        .total_cmp(&seeding.weight(&left.id))
                })
                .then_with(|| note_sort_key(left).cmp(&note_sort_key(right)))
        });
        for block in isotonic_blocks(&row_notes, desired, seeding) {
            let fitted = block.mean().round() as i32;
            for (offset, note) in row_notes[block.start..block.end].iter().enumerate() {
                let column = fitted + (block.start + offset) as i32;
                assigned.insert(note.id.clone(), (column, row));
            }
        }
    }
    assigned
}

fn isotonic_blocks(
    row_notes: &[&NoteRecord],
    desired: &HashMap<NoteId, (i32, i32)>,
    seeding: &Seeding<'_>,
) -> Vec<IsotonicBlock> {
    let mut blocks: Vec<IsotonicBlock> = Vec::new();
    for (index, note) in row_notes.iter().enumerate() {
        let weight = seeding.weight(&note.id);
        let target = f64::from(desired[&note.id].0) - index as f64;
        blocks.push(IsotonicBlock {
            start: index,
            end: index + 1,
            weight,
            weighted_sum: weight * target,
        });
        while blocks.len() >= 2 && blocks[blocks.len() - 2].mean() > blocks[blocks.len() - 1].mean()
        {
            let right = blocks
                .pop()
                .expect("the loop condition requires two blocks");
            let left = blocks
                .pop()
                .expect("the loop condition requires two blocks");
            blocks.push(left.merge(&right));
        }
    }
    blocks
}

struct IsotonicBlock {
    start: usize,
    end: usize,
    weight: f64,
    weighted_sum: f64,
}

impl IsotonicBlock {
    fn mean(&self) -> f64 {
        self.weighted_sum / self.weight
    }

    fn merge(self, right: &Self) -> Self {
        Self {
            start: self.start,
            end: right.end,
            weight: self.weight + right.weight,
            weighted_sum: self.weighted_sum + right.weighted_sum,
        }
    }
}
