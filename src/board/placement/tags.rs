use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use eframe::egui::{Pos2, Vec2};

use super::grid::{
    coarse_slot, compact_side_length, nearest_free_coarse_block, reserve_coarse_block,
};
use super::{ClusterMembers, NoteTags, Seeding, TagWeights};
use crate::board::GRID_SPACING;
use crate::vault::NoteRecord;

const MEMBERSHIP_EDGE_WEIGHT: u32 = 4;
const REFERENCE_EDGE_WEIGHT: u32 = 1;

pub(super) fn cluster_members(notes: &[&NoteRecord], note_tags: &NoteTags) -> ClusterMembers {
    let mut members: ClusterMembers = BTreeMap::new();
    for note in notes {
        for (key, _) in &note_tags[&note.id] {
            members
                .entry(key.clone())
                .or_default()
                .push(note.id.clone());
        }
    }
    members
}

pub(super) fn cluster_names(
    notes: &[&NoteRecord],
    note_tags: &NoteTags,
) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    for note in notes {
        for (key, name) in &note_tags[&note.id] {
            names
                .entry(key.clone())
                .and_modify(|current: &mut String| {
                    if name.to_lowercase() < current.to_lowercase() {
                        current.clone_from(name);
                    }
                })
                .or_insert_with(|| name.clone());
        }
    }
    names
}

pub(super) fn tag_relationship_weights(notes: &[&NoteRecord], note_tags: &NoteTags) -> TagWeights {
    let mut weights = TagWeights::new();
    for note in notes {
        let tags: Vec<_> = note_tags[&note.id].iter().map(|(key, _)| key).collect();
        for left_index in 0..tags.len() {
            for right in tags.iter().skip(left_index + 1) {
                add_tag_weight(
                    &mut weights,
                    tags[left_index],
                    right,
                    MEMBERSHIP_EDGE_WEIGHT,
                );
            }
        }
    }

    let by_id: HashMap<_, _> = notes.iter().map(|note| (&note.id, *note)).collect();
    for note in notes {
        let targets: BTreeSet<_> = note.references.iter().collect();
        for target in &targets {
            let Some(target_note) = by_id.get(target) else {
                continue;
            };
            for (left, _) in &note_tags[&note.id] {
                for (right, _) in &note_tags[&target_note.id] {
                    if left != right {
                        add_tag_weight(&mut weights, left, right, REFERENCE_EDGE_WEIGHT);
                    }
                }
            }
        }
    }
    weights
}

fn add_tag_weight(weights: &mut TagWeights, left: &str, right: &str, weight: u32) {
    *weights.entry(weight_key(left, right)).or_default() += weight;
}

fn weight_key(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    }
}

pub(super) fn tag_centers(
    members: &ClusterMembers,
    weights: &TagWeights,
    seeding: &Seeding<'_>,
) -> HashMap<String, Pos2> {
    let stride = (GRID_SPACING.x * 2.0, GRID_SPACING.y * 2.0);
    let mut centers = BTreeMap::new();
    let mut occupied = HashSet::new();
    let footprint_radius =
        |tag: &str| ((compact_side_length(members[tag].len()) as f32) / 4.0).ceil() as i32;
    for (tag, tag_members) in members {
        let preserved: Vec<_> = tag_members
            .iter()
            .filter_map(|note_id| seeding.preserved_position(note_id))
            .collect();
        if preserved.is_empty() {
            continue;
        }
        let center = Pos2::new(
            median(preserved.iter().map(|position| position.x).collect()),
            median(preserved.iter().map(|position| position.y).collect()),
        );
        reserve_coarse_block(&mut occupied, coarse_slot(center, stride), footprint_radius(tag));
        centers.insert(tag.clone(), center);
    }

    while let Some(next) = next_tag_by_affinity(members, &centers, weights) {
        let related: Vec<_> = centers
            .iter()
            .filter_map(|(tag, position)| {
                let weight = tag_weight(&next, tag, weights);
                (weight > 0).then_some((*position, weight))
            })
            .collect();
        let desired = if related.is_empty() {
            (0, 0)
        } else {
            let total: u32 = related.iter().map(|(_, weight)| weight).sum();
            let sum = related.iter().fold(Vec2::ZERO, |sum, (position, weight)| {
                sum + position.to_vec2() * (*weight as f32 / total as f32)
            });
            coarse_slot(Pos2::ZERO + sum, stride)
        };
        let radius = footprint_radius(&next);
        let slot = nearest_free_coarse_block(desired, &occupied, radius);
        reserve_coarse_block(&mut occupied, slot, radius);
        centers.insert(
            next,
            Pos2::new(slot.0 as f32 * stride.0, slot.1 as f32 * stride.1),
        );
    }
    centers.into_iter().collect()
}

fn next_tag_by_affinity(
    members: &ClusterMembers,
    placed: &BTreeMap<String, Pos2>,
    weights: &TagWeights,
) -> Option<String> {
    members
        .keys()
        .filter(|tag| !placed.contains_key(*tag))
        .max_by(|left, right| {
            placed_weight(left, placed, weights)
                .cmp(&placed_weight(right, placed, weights))
                .then_with(|| members[*left].len().cmp(&members[*right].len()))
                .then_with(|| weighted_degree(left, weights).cmp(&weighted_degree(right, weights)))
                .then_with(|| right.cmp(left))
        })
        .cloned()
}

fn median(mut values: Vec<f32>) -> f32 {
    values.sort_by(f32::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    }
}

fn placed_weight(tag: &str, centers: &BTreeMap<String, Pos2>, weights: &TagWeights) -> u32 {
    centers
        .keys()
        .map(|placed| tag_weight(tag, placed, weights))
        .sum()
}

fn weighted_degree(tag: &str, weights: &TagWeights) -> u32 {
    weights
        .iter()
        .filter_map(|((left, right), weight)| (left == tag || right == tag).then_some(*weight))
        .sum()
}

fn tag_weight(left: &str, right: &str, weights: &TagWeights) -> u32 {
    if left == right {
        return 0;
    }
    weights.get(&weight_key(left, right)).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap},
        path::{Path, PathBuf},
    };

    use super::{tag_centers, weight_key};
    use crate::board::GRID_SPACING;
    use crate::board::placement::Seeding;
    use crate::vault::NoteId;

    fn fake_members(tag: &str, count: usize) -> Vec<NoteId> {
        (0..count)
            .map(|index| NoteId(PathBuf::from(format!("{tag}-{index}.md"))))
            .collect()
    }

    #[test]
    fn a_large_unrelated_cluster_does_not_split_up_a_related_family() {
        let mut members = BTreeMap::new();
        members.insert("big".to_owned(), fake_members("big", 48));
        members.insert("evidence".to_owned(), fake_members("evidence", 5));
        members.insert("outdated".to_owned(), fake_members("outdated", 2));
        members.insert("unfinished".to_owned(), fake_members("unfinished", 2));

        let mut weights = BTreeMap::new();
        weights.insert(weight_key("evidence", "outdated"), 2);
        weights.insert(weight_key("evidence", "unfinished"), 2);

        let seeding = Seeding::new(None, Path::new("/vault"), &HashMap::new(), 0);
        let centers = tag_centers(&members, &weights, &seeding);

        let distance_to_family = |other: &str| centers["evidence"].distance(centers[other]);
        let compact_bound = GRID_SPACING.x.max(GRID_SPACING.y) * 10.0;
        assert!(distance_to_family("outdated") < compact_bound);
        assert!(distance_to_family("unfinished") < compact_bound);
    }
}
