use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    path::Path,
};

use eframe::egui::{Pos2, Vec2};

use super::{
    CARD_SIZE, GRID_SPACING,
    layout::{
        BoardLayout, ClusterRegion, LayoutSeed, LayoutStats, PlacementFingerprint, layout_bounds,
        resolved_edges,
    },
    metaballs::{
        BASE_INFLUENCE_RADIUS, FIELD_CELL_BUDGET, FIELD_THRESHOLD, adaptive_field_step,
        build_geometry, field_bounds, field_value_at, required_pair_radius,
    },
};
use crate::vault::{NoteId, NoteRecord, VaultIndex};

const UNTAGGED_KEY: &str = "\0atlas-untagged";
const MEMBERSHIP_EDGE_WEIGHT: u32 = 4;
const REFERENCE_EDGE_WEIGHT: u32 = 1;
const REFERENCE_PULL: f32 = 0.20;
const PRESERVED_WEIGHT: f64 = 32.0;
const NEW_WEIGHT: f64 = 1.0;
const MAX_CONNECTIVITY_PASSES: usize = 32;
const MAX_STALLED_PASSES: usize = 4;
const CLUSTER_GUTTER_SLOTS: i32 = 1;
const SEPARATION_PASSES: usize = 4;
const MAX_SEPARATION_RADIUS: i32 = 32;
const FIELD_PASSES: usize = 8;

type NoteTags = HashMap<NoteId, Vec<(String, String)>>;
type ClusterMembers = BTreeMap<String, Vec<NoteId>>;
type TagWeights = BTreeMap<(String, String), u32>;

pub(crate) fn prepare_board_layout(index: &VaultIndex, seed: Option<&LayoutSeed>) -> BoardLayout {
    if index.notes.is_empty() {
        return BoardLayout::default();
    }

    let mut notes: Vec<_> = index.notes.iter().collect();
    notes.sort_by_key(|note| note_sort_key(note));
    let note_tags: NoteTags = notes
        .iter()
        .map(|note| (note.id.clone(), normalized_tags(note)))
        .collect();
    let fingerprints: HashMap<_, _> = notes
        .iter()
        .map(|note| {
            (
                note.id.clone(),
                placement_fingerprint(note, &note_tags[&note.id]),
            )
        })
        .collect();
    let seeding = Seeding::new(seed, &index.root, &fingerprints, notes.len());

    let members = cluster_members(&notes, &note_tags);
    let (mut positions, connectivity_passes) =
        solve_positions(&notes, &note_tags, &members, &seeding);
    let fields = resolve_cluster_fields(&notes, &note_tags, &members, &mut positions);
    let (clusters, sampled_field_cells) =
        build_cluster_regions(&notes, &note_tags, &members, &positions, &fields);

    BoardLayout {
        edges: resolved_edges(index, &positions),
        content_bounds: layout_bounds(&positions, &clusters),
        stats: LayoutStats {
            connectivity_passes,
            fallback_cluster_count: fields.fallback_tags.len(),
            maximum_influence_radius: fields.radii.values().copied().fold(0.0, f32::max),
            sampled_field_cells,
            field_step: fields.step,
        },
        positions,
        clusters,
        fingerprints,
    }
}

struct Seeding<'a> {
    seed: Option<&'a LayoutSeed>,
    unchanged: HashSet<NoteId>,
    exact_rescan: bool,
}

impl<'a> Seeding<'a> {
    fn new(
        seed: Option<&'a LayoutSeed>,
        root: &Path,
        fingerprints: &HashMap<NoteId, PlacementFingerprint>,
        note_count: usize,
    ) -> Self {
        let seed = seed.filter(|seed| seed.root == root);
        let unchanged: HashSet<_> = seed.map_or_else(HashSet::new, |seed| {
            seed.fingerprints
                .iter()
                .filter(|(note_id, fingerprint)| {
                    fingerprints.get(*note_id) == Some(*fingerprint)
                        && seed.positions.contains_key(*note_id)
                })
                .map(|(note_id, _)| note_id.clone())
                .collect()
        });
        let exact_rescan = seed.is_some_and(|seed| {
            seed.positions.len() == note_count && unchanged.len() == note_count
        });
        Self {
            seed,
            unchanged,
            exact_rescan,
        }
    }

    fn is_warm(&self) -> bool {
        self.seed.is_some()
    }

    fn is_unchanged(&self, note_id: &NoteId) -> bool {
        self.unchanged.contains(note_id)
    }

    fn preserved_position(&self, note_id: &NoteId) -> Option<Pos2> {
        self.is_unchanged(note_id)
            .then(|| self.seed?.positions.get(note_id).copied())
            .flatten()
    }

    fn weight(&self, note_id: &NoteId) -> f64 {
        if self.is_unchanged(note_id) {
            PRESERVED_WEIGHT
        } else {
            NEW_WEIGHT
        }
    }
}

fn solve_positions(
    notes: &[&NoteRecord],
    note_tags: &NoteTags,
    members: &ClusterMembers,
    seeding: &Seeding<'_>,
) -> (HashMap<NoteId, Pos2>, usize) {
    let tag_weights = tag_relationship_weights(notes, note_tags);
    let tag_centers = tag_centers(members, &tag_weights, seeding);
    let desired = desired_note_slots(notes, note_tags, &tag_centers, seeding);

    if seeding.exact_rescan {
        let seed = seeding.seed.expect("an exact rescan has a seed");
        return (seed.positions.clone(), 0);
    }

    let spread = spread_shared_anchors(notes, note_tags, desired.clone(), seeding);
    let initial = slots_to_positions(pack_rows(notes, &spread, seeding));
    refine_connectivity(
        notes,
        members,
        initial,
        &slots_to_positions(desired),
        seeding,
    )
}

struct ClusterFields {
    radii: BTreeMap<String, f32>,
    fallback_tags: BTreeSet<String>,
    step: f32,
}

fn resolve_cluster_fields(
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
            let (_, mut geometry, _) = build_geometry(&member_positions, radius, fields.step);
            for _ in 0..FIELD_PASSES {
                if geometry.contours.len() == 1 {
                    break;
                }
                radius += fields.step;
                (_, geometry, _) = build_geometry(&member_positions, radius, fields.step);
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

fn build_cluster_regions(
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
        let (bounds, geometry, cell_count) = build_geometry(
            &member_positions(tag_members, positions),
            radius,
            fields.step,
        );
        debug_assert_eq!(
            geometry.contours.len(),
            1,
            "cluster {tag} did not produce one connected field"
        );
        sampled_cells += cell_count;
        clusters.push(ClusterRegion {
            key: tag.clone(),
            name: names[tag].clone(),
            bounds,
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

fn cluster_names(notes: &[&NoteRecord], note_tags: &NoteTags) -> BTreeMap<String, String> {
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

fn cluster_members(notes: &[&NoteRecord], note_tags: &NoteTags) -> ClusterMembers {
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

fn tag_relationship_weights(notes: &[&NoteRecord], note_tags: &NoteTags) -> TagWeights {
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

fn tag_centers(
    members: &ClusterMembers,
    weights: &TagWeights,
    seeding: &Seeding<'_>,
) -> HashMap<String, Pos2> {
    if !seeding.is_warm() {
        return cold_side_by_side_centers(members, weights);
    }

    let stride = (GRID_SPACING.x * 2.0, GRID_SPACING.y * 2.0);
    let mut centers = BTreeMap::new();
    let mut occupied = HashSet::new();
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
        occupied.insert(coarse_slot(center, stride));
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
        let slot = nearest_free_coarse_slot(desired, &occupied);
        occupied.insert(slot);
        centers.insert(
            next,
            Pos2::new(slot.0 as f32 * stride.0, slot.1 as f32 * stride.1),
        );
    }
    centers.into_iter().collect()
}

fn cold_side_by_side_centers(
    members: &ClusterMembers,
    weights: &TagWeights,
) -> HashMap<String, Pos2> {
    let order = relationship_order(members, weights);
    let side_lengths: BTreeMap<_, _> = members
        .iter()
        .map(|(tag, members)| (tag.clone(), compact_side_length(members.len())))
        .collect();
    let total_padded_area: i32 = side_lengths
        .values()
        .map(|side| {
            let padded = side + CLUSTER_GUTTER_SLOTS;
            padded * padded
        })
        .sum();
    let shelf_width = (total_padded_area as f32)
        .sqrt()
        .ceil()
        .max(side_lengths.values().copied().max().unwrap_or(1) as f32) as i32;

    let mut slot_centers = BTreeMap::new();
    let mut cursor_x = 0;
    let mut cursor_y = 0;
    let mut row_height = 0;
    let mut used_width = 0;
    for tag in order {
        let side = side_lengths[&tag];
        if cursor_x > 0 && cursor_x + side > shelf_width {
            cursor_x = 0;
            cursor_y += row_height + CLUSTER_GUTTER_SLOTS;
            row_height = 0;
        }
        slot_centers.insert(
            tag,
            (
                cursor_x as f32 + (side - 1) as f32 * 0.5,
                cursor_y as f32 + (side - 1) as f32 * 0.5,
            ),
        );
        cursor_x += side + CLUSTER_GUTTER_SLOTS;
        row_height = row_height.max(side);
        used_width = used_width.max(cursor_x - CLUSTER_GUTTER_SLOTS);
    }
    let used_height = cursor_y + row_height;
    let offset = (
        (used_width.saturating_sub(1)) as f32 * 0.5,
        (used_height.saturating_sub(1)) as f32 * 0.5,
    );
    slot_centers
        .into_iter()
        .map(|(tag, center)| {
            (
                tag,
                Pos2::new(
                    (center.0 - offset.0) * GRID_SPACING.x,
                    (center.1 - offset.1) * GRID_SPACING.y,
                ),
            )
        })
        .collect()
}

fn compact_side_length(member_count: usize) -> i32 {
    let side = (member_count as f32).sqrt().ceil().max(1.0) as i32;
    if side % 2 == 0 { side + 1 } else { side }
}

fn relationship_order(members: &ClusterMembers, weights: &TagWeights) -> Vec<String> {
    let mut order = Vec::with_capacity(members.len());
    let mut placed = BTreeMap::new();
    while let Some(next) = next_tag_by_affinity(members, &placed, weights) {
        placed.insert(next.clone(), Pos2::ZERO);
        order.push(next);
    }
    order
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

fn coarse_slot(position: Pos2, stride: (f32, f32)) -> (i32, i32) {
    (
        (position.x / stride.0).round() as i32,
        (position.y / stride.1).round() as i32,
    )
}

fn nearest_free_coarse_slot(desired: (i32, i32), occupied: &HashSet<(i32, i32)>) -> (i32, i32) {
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

fn ring_by_distance(origin: (i32, i32), radius: i32) -> Vec<(i32, i32)> {
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

fn desired_note_slots(
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

fn spread_shared_anchors(
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
        movable.sort_by(|left, right| {
            let left_degree = left.references.len() + left.backlinks.len();
            let right_degree = right.references.len() + right.backlinks.len();
            note_tags[&right.id]
                .len()
                .cmp(&note_tags[&left.id].len())
                .then_with(|| right_degree.cmp(&left_degree))
                .then_with(|| note_sort_key(left).cmp(&note_sort_key(right)))
        });
        let reserve_center = desired
            .iter()
            .any(|(id, slot)| seeding.is_unchanged(id) && *slot == anchor);
        for (index, note) in movable.into_iter().enumerate() {
            let offset = compact_offset(index + usize::from(reserve_center));
            desired.insert(note.id.clone(), (anchor.0 + offset.0, anchor.1 + offset.1));
        }
    }
    desired
}

fn compact_offset(index: usize) -> (i32, i32) {
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
    unreachable!()
}

fn pack_rows(
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
        let mut blocks: Vec<IsotonicBlock> = Vec::new();
        for (index, note) in row_notes.iter().enumerate() {
            let weight = seeding.weight(&note.id);
            let q = desired[&note.id].0 as f64 - index as f64;
            blocks.push(IsotonicBlock {
                start: index,
                end: index + 1,
                weight,
                weighted_sum: weight * q,
            });
            while blocks.len() >= 2 {
                let last = blocks.len() - 1;
                if blocks[last - 1].mean() <= blocks[last].mean() {
                    break;
                }
                let right = blocks.pop().expect("right block");
                let left = blocks.pop().expect("left block");
                blocks.push(left.merge(right));
            }
        }
        for block in blocks {
            let fitted = block.mean().round() as i32;
            for (index, note) in row_notes
                .iter()
                .enumerate()
                .take(block.end)
                .skip(block.start)
            {
                assigned.insert(note.id.clone(), (fitted + index as i32, row));
            }
        }
    }
    assigned
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

    fn merge(self, right: Self) -> Self {
        Self {
            start: self.start,
            end: right.end,
            weight: self.weight + right.weight,
            weighted_sum: self.weighted_sum + right.weighted_sum,
        }
    }
}

fn refine_connectivity(
    notes: &[&NoteRecord],
    members: &ClusterMembers,
    initial: HashMap<NoteId, Pos2>,
    desired: &HashMap<NoteId, Pos2>,
    seeding: &Seeding<'_>,
) -> (HashMap<NoteId, Pos2>, usize) {
    let mut current = initial;
    let mut best = current.clone();
    let mut best_score = layout_score(&best, members, desired, seeding);
    let mut stalled = 0;
    let mut passes = 0;

    for pass in 0..MAX_CONNECTIVITY_PASSES {
        let mut requests: HashMap<NoteId, (i32, i32)> = HashMap::new();
        let mut disconnected = false;
        for tag_members in members.values() {
            let components = connected_components(tag_members, &current);
            if components.len() <= 1 {
                continue;
            }
            disconnected = true;
            let root_index = root_component(&components, seeding);
            let root = &components[root_index];
            let root_buckets = TagBucketIndex::new(root, &current);
            for (index, component) in components.iter().enumerate() {
                if index == root_index {
                    continue;
                }
                let (left, right) = closest_pair(component, &root_buckets, &current);
                let left_slot = position_slot(current[left]);
                let right_slot = position_slot(current[right]);
                let direction = (
                    (right_slot.0 - left_slot.0).signum(),
                    (right_slot.1 - left_slot.1).signum(),
                );
                let left_request = requests.entry(left.clone()).or_default();
                left_request.0 += direction.0;
                left_request.1 += direction.1;
                let right_request = requests.entry(right.clone()).or_default();
                right_request.0 -= direction.0;
                right_request.1 -= direction.1;
            }
        }
        if !disconnected {
            return (current, pass);
        }
        let mut proposed: HashMap<_, _> = notes
            .iter()
            .map(|note| (note.id.clone(), position_slot(current[&note.id])))
            .collect();
        for (note_id, request) in requests {
            let slot = proposed[&note_id];
            proposed.insert(
                note_id,
                (slot.0 + request.0.signum(), slot.1 + request.1.signum()),
            );
        }
        current = slots_to_positions(pack_rows(notes, &proposed, seeding));
        passes = pass + 1;
        let score = layout_score(&current, members, desired, seeding);
        if score < best_score {
            best_score = score;
            best.clone_from(&current);
            stalled = 0;
        } else {
            stalled += 1;
            if stalled >= MAX_STALLED_PASSES {
                break;
            }
        }
    }
    (best, passes)
}

#[derive(Clone, Debug, PartialEq)]
struct LayoutScore {
    disconnected_components: usize,
    preserved_displacement: f32,
    desired_displacement: f32,
    slots: Vec<(i32, i32)>,
}

impl Eq for LayoutScore {}

impl PartialOrd for LayoutScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LayoutScore {
    fn cmp(&self, other: &Self) -> Ordering {
        self.disconnected_components
            .cmp(&other.disconnected_components)
            .then_with(|| {
                self.preserved_displacement
                    .total_cmp(&other.preserved_displacement)
            })
            .then_with(|| {
                self.desired_displacement
                    .total_cmp(&other.desired_displacement)
            })
            .then_with(|| self.slots.cmp(&other.slots))
    }
}

fn layout_score(
    positions: &HashMap<NoteId, Pos2>,
    members: &ClusterMembers,
    desired: &HashMap<NoteId, Pos2>,
    seeding: &Seeding<'_>,
) -> LayoutScore {
    let disconnected_components = members
        .values()
        .map(|members| {
            connected_components(members, positions)
                .len()
                .saturating_sub(1)
        })
        .sum();
    let mut note_ids: Vec<_> = positions.keys().collect();
    note_ids.sort();
    let preserved_displacement = note_ids
        .iter()
        .filter_map(|note_id| {
            Some(positions[*note_id].distance(seeding.preserved_position(note_id)?))
        })
        .sum();
    let desired_displacement = note_ids
        .iter()
        .map(|note_id| positions[*note_id].distance(desired[*note_id]))
        .sum();
    let slots = note_ids
        .into_iter()
        .map(|note_id| position_slot(positions[note_id]))
        .collect();
    LayoutScore {
        disconnected_components,
        preserved_displacement,
        desired_displacement,
        slots,
    }
}

fn connected_components(members: &[NoteId], positions: &HashMap<NoteId, Pos2>) -> Vec<Vec<NoteId>> {
    let by_slot: HashMap<_, _> = members
        .iter()
        .map(|note_id| (position_slot(positions[note_id]), note_id))
        .collect();
    let mut remaining: BTreeSet<_> = members.iter().cloned().collect();
    let mut components = Vec::new();
    while let Some(start) = remaining.first().cloned() {
        remaining.remove(&start);
        let mut queue = VecDeque::from([start]);
        let mut component = Vec::new();
        while let Some(note_id) = queue.pop_front() {
            let slot = position_slot(positions[&note_id]);
            component.push(note_id);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    if let Some(neighbor) = by_slot.get(&(slot.0 + dx, slot.1 + dy))
                        && remaining.remove(*neighbor)
                    {
                        queue.push_back((*neighbor).clone());
                    }
                }
            }
        }
        component.sort();
        components.push(component);
    }
    components.sort_by(|left, right| left[0].cmp(&right[0]));
    components
}

fn root_component(components: &[Vec<NoteId>], seeding: &Seeding<'_>) -> usize {
    let unchanged_count = |component: &Vec<NoteId>| {
        component
            .iter()
            .filter(|note_id| seeding.is_unchanged(note_id))
            .count()
    };
    components
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            unchanged_count(left)
                .cmp(&unchanged_count(right))
                .then_with(|| left.len().cmp(&right.len()))
                .then_with(|| right[0].cmp(&left[0]))
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

const CONNECTIVITY_BUCKET_SIZE: i32 = 8;

struct TagBucketIndex<'a> {
    buckets: BTreeMap<(i32, i32), Vec<&'a NoteId>>,
}

impl<'a> TagBucketIndex<'a> {
    fn new(members: &'a [NoteId], positions: &HashMap<NoteId, Pos2>) -> Self {
        let mut buckets: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for note_id in members {
            let slot = position_slot(positions[note_id]);
            buckets
                .entry((
                    slot.0.div_euclid(CONNECTIVITY_BUCKET_SIZE),
                    slot.1.div_euclid(CONNECTIVITY_BUCKET_SIZE),
                ))
                .or_default()
                .push(note_id);
        }
        Self { buckets }
    }
}

fn closest_pair<'a>(
    left: &'a [NoteId],
    right: &TagBucketIndex<'a>,
    positions: &HashMap<NoteId, Pos2>,
) -> (&'a NoteId, &'a NoteId) {
    let mut best: Option<(i64, &NoteId, &NoteId)> = None;
    for left_id in left {
        let left_slot = position_slot(positions[left_id]);
        for (bucket, right_ids) in &right.buckets {
            let bucket_min = (
                bucket.0 * CONNECTIVITY_BUCKET_SIZE,
                bucket.1 * CONNECTIVITY_BUCKET_SIZE,
            );
            let bucket_max = (
                bucket_min.0 + CONNECTIVITY_BUCKET_SIZE - 1,
                bucket_min.1 + CONNECTIVITY_BUCKET_SIZE - 1,
            );
            let lower_dx = distance_to_interval(left_slot.0, bucket_min.0, bucket_max.0);
            let lower_dy = distance_to_interval(left_slot.1, bucket_min.1, bucket_max.1);
            let lower_bound = lower_dx * lower_dx + lower_dy * lower_dy;
            if best.is_some_and(|(distance, _, _)| lower_bound > distance) {
                continue;
            }
            for right_id in right_ids {
                let right_slot = position_slot(positions[*right_id]);
                let dx = i64::from(left_slot.0) - i64::from(right_slot.0);
                let dy = i64::from(left_slot.1) - i64::from(right_slot.1);
                let candidate = (dx * dx + dy * dy, left_id, *right_id);
                if best.as_ref().is_none_or(|current| candidate < *current) {
                    best = Some(candidate);
                }
            }
        }
    }
    let (_, left, right) = best.expect("both components contain members");
    (left, right)
}

fn distance_to_interval(value: i32, minimum: i32, maximum: i32) -> i64 {
    if value < minimum {
        i64::from(minimum) - i64::from(value)
    } else if value > maximum {
        i64::from(value) - i64::from(maximum)
    } else {
        0
    }
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

fn separate_foreign_notes(
    notes: &[&NoteRecord],
    note_tags: &NoteTags,
    members: &ClusterMembers,
    radii: &BTreeMap<String, f32>,
    fallback_tags: &BTreeSet<String>,
    positions: &mut HashMap<NoteId, Pos2>,
) -> usize {
    if fallback_tags.is_empty() {
        return 0;
    }
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
        let mut destination = None;
        for radius in 1_i32..=MAX_SEPARATION_RADIUS {
            destination = ring_by_distance(current, radius)
                .into_iter()
                .find(|candidate| {
                    !occupied.contains(candidate)
                        && preserves_own_tag_proximity(
                            &note.id, *candidate, &own_tags, members, positions,
                        )
                        && members.keys().all(|tag| {
                            own_tags.contains(tag.as_str())
                                || !tag_field_contains(
                                    tag,
                                    slot_position(*candidate),
                                    members,
                                    radii,
                                    positions,
                                )
                        })
                });
            if destination.is_some() {
                break;
            }
        }
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
    members: &BTreeMap<String, Vec<NoteId>>,
    radii: &BTreeMap<String, f32>,
    positions: &HashMap<NoteId, Pos2>,
) -> bool {
    let sources: Vec<_> = members[tag]
        .iter()
        .map(|note_id| positions[note_id])
        .collect();
    field_value_at(&sources, radii[tag], point) >= FIELD_THRESHOLD
}

fn preserves_own_tag_proximity(
    note_id: &NoteId,
    candidate: (i32, i32),
    own_tags: &BTreeSet<&str>,
    members: &BTreeMap<String, Vec<NoteId>>,
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
    members: &BTreeMap<String, Vec<NoteId>>,
    positions: &HashMap<NoteId, Pos2>,
    radii: &BTreeMap<String, f32>,
) -> f32 {
    let bounds: Vec<_> = members
        .iter()
        .filter_map(|(tag, members)| {
            let positions: Vec<_> = members.iter().map(|note_id| positions[note_id]).collect();
            field_bounds(&positions, radii[tag])
        })
        .collect();
    let mut step = adaptive_field_step(bounds.iter().map(|bounds| bounds.area()));
    while estimated_field_cells(&bounds, step) > FIELD_CELL_BUDGET as usize {
        step += 4.0;
    }
    step
}

fn estimated_field_cells(bounds: &[eframe::egui::Rect], step: f32) -> usize {
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

fn slots_to_positions(slots: HashMap<NoteId, (i32, i32)>) -> HashMap<NoteId, Pos2> {
    slots
        .into_iter()
        .map(|(note_id, slot)| (note_id, slot_position(slot)))
        .collect()
}

fn slot_position(slot: (i32, i32)) -> Pos2 {
    Pos2::new(
        slot.0 as f32 * GRID_SPACING.x,
        slot.1 as f32 * GRID_SPACING.y,
    )
}

fn position_slot(position: Pos2) -> (i32, i32) {
    (
        (position.x / GRID_SPACING.x).round() as i32,
        (position.y / GRID_SPACING.y).round() as i32,
    )
}

fn normalized_tags(note: &NoteRecord) -> Vec<(String, String)> {
    let mut tags = BTreeMap::new();
    for tag in &note.tags {
        let display = tag.trim().trim_start_matches('#');
        if !display.is_empty() {
            tags.entry(display.to_lowercase())
                .or_insert_with(|| display.to_owned());
        }
    }
    if tags.is_empty() {
        vec![(UNTAGGED_KEY.to_owned(), "Untagged".to_owned())]
    } else {
        tags.into_iter().collect()
    }
}

fn placement_fingerprint(note: &NoteRecord, tags: &[(String, String)]) -> PlacementFingerprint {
    let mut related: Vec<_> = note
        .references
        .iter()
        .chain(note.backlinks.iter())
        .cloned()
        .collect();
    related.sort();
    related.dedup();
    PlacementFingerprint {
        tags: tags.iter().map(|(key, _)| key.clone()).collect(),
        related,
    }
}

fn note_sort_key(note: &NoteRecord) -> (String, String) {
    let id = note.id.display();
    (id.to_lowercase(), id)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use super::{
        MAX_CONNECTIVITY_PASSES, cold_side_by_side_centers, compact_offset, compact_side_length,
        minimum_spanning_radius,
    };
    use crate::board::{GRID_SPACING, metaballs::BASE_INFLUENCE_RADIUS};
    use crate::vault::NoteId;
    use eframe::egui::Pos2;

    #[test]
    fn compact_offsets_do_not_repeat() {
        let offsets: std::collections::HashSet<_> = (0..1_000).map(compact_offset).collect();
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

    #[test]
    fn spanning_radius_keeps_adjacent_grid_notes_at_the_base_radius() {
        assert!(
            minimum_spanning_radius(&[Pos2::ZERO, Pos2::new(GRID_SPACING.x, GRID_SPACING.y),])
                <= BASE_INFLUENCE_RADIUS
        );
        assert_eq!(MAX_CONNECTIVITY_PASSES, 32);
    }

    #[test]
    fn static_shelves_place_small_clusters_beside_large_clusters() {
        let mut members = BTreeMap::new();
        members.insert(
            "large".to_owned(),
            (0..100)
                .map(|index| NoteId(PathBuf::from(format!("large-{index}.md"))))
                .collect(),
        );
        members.insert("small".to_owned(), vec![NoteId(PathBuf::from("small.md"))]);

        let centers = cold_side_by_side_centers(&members, &BTreeMap::new());
        let large = centers["large"];
        let small = centers["small"];

        assert_eq!((small.x - large.x) / GRID_SPACING.x, 7.0);
        assert_eq!((small.y - large.y) / GRID_SPACING.y, -5.0);
    }
}
