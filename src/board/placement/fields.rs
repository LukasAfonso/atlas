use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use eframe::egui::{Pos2, Rect, Vec2};

use super::connectivity::connected_components;
use super::grid::{position_slot, ring_by_distance, slot_position};
use super::tags::cluster_names;
use super::{ClusterMembers, NoteTags};
use crate::board::CARD_SIZE;
use crate::board::layout::ClusterRegion;
use crate::board::metaballs::{
    BASE_INFLUENCE_RADIUS, FIELD_CELL_BUDGET, FIELD_THRESHOLD, adaptive_field_step, build_geometry,
    field_bounds, field_value_at, required_pair_radius,
};
use crate::vault::{NoteId, NoteRecord};

const SEPARATION_PASSES: usize = 4;
const MAX_SEPARATION_RADIUS: i32 = 32;
const FIELD_PASSES: usize = 8;

pub(super) struct ClusterFields {
    pub(super) radii: BTreeMap<String, f32>,
    pub(super) fallback_tags: BTreeSet<String>,
    pub(super) step: f32,
}

pub(super) fn resolve_cluster_fields(
    notes: &[&NoteRecord],
    note_tags: &NoteTags,
    members: &ClusterMembers,
    positions: &mut HashMap<NoteId, Pos2>,
) -> ClusterFields {
    let (mut raw_radii, mut fallback_tags) = cluster_radii(members, positions);
    for _ in 0..SEPARATION_PASSES {
        let moved = separate_foreign_notes(
            notes,
            note_tags,
            members,
            &raw_radii,
            &fallback_tags,
            positions,
        );
        if moved == 0 {
            break;
        }
        (raw_radii, fallback_tags) = cluster_radii(members, positions);
    }

    let mut fields = ClusterFields {
        step: sample_step_for(members, positions, &raw_radii),
        radii: raw_radii.clone(),
        fallback_tags,
    };
    pad_radii_to_sample_step(&raw_radii, members, positions, &mut fields);
    grow_radii_until_connected(members, positions, &mut fields);
    fields
}

fn pad_radii_to_sample_step(
    raw_radii: &BTreeMap<String, f32>,
    members: &ClusterMembers,
    positions: &HashMap<NoteId, Pos2>,
    fields: &mut ClusterFields,
) {
    for _ in 0..FIELD_PASSES {
        for (tag, raw_radius) in raw_radii {
            let padded = if *raw_radius > BASE_INFLUENCE_RADIUS {
                raw_radius + fields.step
            } else {
                *raw_radius
            };
            fields.radii.insert(tag.clone(), padded);
        }
        let next_step = sample_step_for(members, positions, &fields.radii);
        if next_step == fields.step {
            break;
        }
        fields.step = next_step;
    }
}

fn grow_radii_until_connected(
    members: &ClusterMembers,
    positions: &HashMap<NoteId, Pos2>,
    fields: &mut ClusterFields,
) {
    for _ in 0..FIELD_PASSES {
        let mut radius_changed = false;
        for (tag, tag_members) in members {
            if fields.fallback_tags.contains(tag) {
                continue;
            }
            let member_positions = member_positions(tag_members, positions);
            let initial_radius = fields.radii[tag];
            let mut radius = initial_radius;
            let mut geometry = build_geometry(&member_positions, radius, fields.step);
            for _ in 0..FIELD_PASSES {
                if geometry.contours.len() == 1 {
                    break;
                }
                radius += fields.step;
                geometry = build_geometry(&member_positions, radius, fields.step);
            }
            if radius > initial_radius {
                fields.fallback_tags.insert(tag.clone());
                fields.radii.insert(tag.clone(), radius);
                radius_changed = true;
            }
        }
        let next_step = sample_step_for(members, positions, &fields.radii);
        if !radius_changed && next_step == fields.step {
            break;
        }
        fields.step = next_step;
    }
}

pub(super) fn build_cluster_regions(
    notes: &[&NoteRecord],
    note_tags: &NoteTags,
    members: &ClusterMembers,
    positions: &HashMap<NoteId, Pos2>,
    fields: &ClusterFields,
) -> (Vec<ClusterRegion>, usize) {
    let names = cluster_names(notes, note_tags);
    let mut sampled_cells = 0;
    let mut clusters = Vec::with_capacity(members.len());

    for (tag, tag_members) in members {
        let radius = fields.radii[tag];
        let geometry = build_geometry(
            &member_positions(tag_members, positions),
            radius,
            fields.step,
        );
        debug_assert_eq!(
            geometry.contours.len(),
            1,
            "cluster {tag} did not produce one connected field"
        );
        sampled_cells += geometry.sampled_cells;
        clusters.push(ClusterRegion {
            key: tag.clone(),
            name: names[tag].clone(),
            label_anchor: cluster_label_anchor(tag_members, positions, radius),
            note_count: tag_members.len(),
            geometry,
            influence_radius: radius,
        });
    }
    (clusters, sampled_cells)
}

fn cluster_label_anchor(
    members: &[NoteId],
    positions: &HashMap<NoteId, Pos2>,
    radius: f32,
) -> Pos2 {
    let anchor_member = members
        .iter()
        .min_by(|left, right| {
            positions[*left]
                .y
                .total_cmp(&positions[*right].y)
                .then_with(|| positions[*left].x.total_cmp(&positions[*right].x))
                .then_with(|| left.cmp(right))
        })
        .expect("clusters always have members");
    positions[anchor_member]
        + Vec2::new(
            -CARD_SIZE.x * 0.5 + 12.0,
            -CARD_SIZE.y * 0.5 - radius * 0.35,
        )
}

fn member_positions(members: &[NoteId], positions: &HashMap<NoteId, Pos2>) -> Vec<Pos2> {
    members.iter().map(|note_id| positions[note_id]).collect()
}

fn cluster_radii(
    members: &ClusterMembers,
    positions: &HashMap<NoteId, Pos2>,
) -> (BTreeMap<String, f32>, BTreeSet<String>) {
    let mut radii = BTreeMap::new();
    let mut fallback_tags = BTreeSet::new();
    for (tag, tag_members) in members {
        let connected = connected_components(tag_members, positions).len() <= 1;
        let radius = if connected {
            BASE_INFLUENCE_RADIUS
        } else {
            fallback_tags.insert(tag.clone());
            minimum_spanning_radius(&member_positions(tag_members, positions))
                .max(BASE_INFLUENCE_RADIUS)
        };
        radii.insert(tag.clone(), radius);
    }
    (radii, fallback_tags)
}

fn minimum_spanning_radius(positions: &[Pos2]) -> f32 {
    if positions.len() <= 1 {
        return BASE_INFLUENCE_RADIUS;
    }
    let mut used = vec![false; positions.len()];
    let mut minimum = vec![f32::INFINITY; positions.len()];
    minimum[0] = 0.0;
    let mut maximum_edge = 0.0_f32;
    for _ in 0..positions.len() {
        let next = (0..positions.len())
            .filter(|index| !used[*index])
            .min_by(|left, right| {
                minimum[*left]
                    .total_cmp(&minimum[*right])
                    .then_with(|| left.cmp(right))
            })
            .expect("the spanning tree has an unused vertex");
        used[next] = true;
        maximum_edge = maximum_edge.max(minimum[next]);
        for candidate in 0..positions.len() {
            if !used[candidate] {
                minimum[candidate] = minimum[candidate]
                    .min(required_pair_radius(positions[next], positions[candidate]));
            }
        }
    }
    maximum_edge
}

fn separate_foreign_notes(
    notes: &[&NoteRecord],
    note_tags: &NoteTags,
    members: &ClusterMembers,
    radii: &BTreeMap<String, f32>,
    fallback_tags: &BTreeSet<String>,
    positions: &mut HashMap<NoteId, Pos2>,
) -> usize {
    let separable_fallbacks: BTreeSet<_> = fallback_tags
        .iter()
        .filter(|tag| {
            let shared_members = members[*tag]
                .iter()
                .filter(|note_id| note_tags[*note_id].len() > 1)
                .count();
            shared_members * 2 <= members[*tag].len()
        })
        .collect();
    if separable_fallbacks.is_empty() {
        return 0;
    }
    let mut occupied: HashSet<_> = positions.values().copied().map(position_slot).collect();
    let mut moved = 0;

    for note in notes {
        let own_tags: BTreeSet<_> = note_tags[&note.id]
            .iter()
            .map(|(key, _)| key.as_str())
            .collect();
        if !separable_fallbacks.iter().any(|tag| {
            !own_tags.contains(tag.as_str())
                && tag_field_contains(tag, positions[&note.id], members, radii, positions)
        }) {
            continue;
        }

        let current = position_slot(positions[&note.id]);
        let destination = (1..=MAX_SEPARATION_RADIUS).find_map(|radius| {
            ring_by_distance(current, radius).into_iter().find(|slot| {
                !occupied.contains(slot)
                    && preserves_own_tag_proximity(&note.id, *slot, &own_tags, members, positions)
                    && members.keys().all(|tag| {
                        own_tags.contains(tag.as_str())
                            || !tag_field_contains(
                                tag,
                                slot_position(*slot),
                                members,
                                radii,
                                positions,
                            )
                    })
            })
        });
        if let Some(destination) = destination {
            occupied.remove(&current);
            occupied.insert(destination);
            positions.insert(note.id.clone(), slot_position(destination));
            moved += 1;
        }
    }
    moved
}

fn tag_field_contains(
    tag: &str,
    point: Pos2,
    members: &ClusterMembers,
    radii: &BTreeMap<String, f32>,
    positions: &HashMap<NoteId, Pos2>,
) -> bool {
    field_value_at(
        &member_positions(&members[tag], positions),
        radii[tag],
        point,
    ) >= FIELD_THRESHOLD
}

fn preserves_own_tag_proximity(
    note_id: &NoteId,
    candidate: (i32, i32),
    own_tags: &BTreeSet<&str>,
    members: &ClusterMembers,
    positions: &HashMap<NoteId, Pos2>,
) -> bool {
    own_tags.iter().all(|tag| {
        let others: Vec<_> = members[*tag]
            .iter()
            .filter(|member| *member != note_id)
            .map(|member| position_slot(positions[member]))
            .collect();
        if others.is_empty() {
            return true;
        }
        let current = position_slot(positions[note_id]);
        let nearest = |slot: (i32, i32)| {
            others
                .iter()
                .map(|other| slot.0.abs_diff(other.0).max(slot.1.abs_diff(other.1)))
                .min()
                .expect("a multi-note tag has another member")
        };
        nearest(candidate) <= nearest(current).max(1)
    })
}

fn sample_step_for(
    members: &ClusterMembers,
    positions: &HashMap<NoteId, Pos2>,
    radii: &BTreeMap<String, f32>,
) -> f32 {
    let bounds: Vec<_> = members
        .iter()
        .filter_map(|(tag, members)| {
            field_bounds(&member_positions(members, positions), radii[tag])
        })
        .collect();
    let mut step = adaptive_field_step(bounds.iter().map(Rect::area));
    while estimated_field_cells(&bounds, step) > FIELD_CELL_BUDGET as usize {
        step += 4.0;
    }
    step
}

fn estimated_field_cells(bounds: &[Rect], step: f32) -> usize {
    bounds
        .iter()
        .map(|bounds| {
            let expanded = bounds.expand(step);
            let columns = ((expanded.width() / step).ceil() as usize).max(1);
            let rows = ((expanded.height() / step).ceil() as usize).max(1);
            columns.saturating_mul(rows)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{BASE_INFLUENCE_RADIUS, minimum_spanning_radius};
    use crate::board::GRID_SPACING;
    use eframe::egui::Pos2;

    #[test]
    fn spanning_radius_keeps_adjacent_grid_notes_at_the_base_radius() {
        assert!(
            minimum_spanning_radius(&[Pos2::ZERO, Pos2::new(GRID_SPACING.x, GRID_SPACING.y)])
                <= BASE_INFLUENCE_RADIUS
        );
    }
}
