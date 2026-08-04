use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
};

use eframe::egui::{Pos2, Vec2};

use super::{
    CARD_SIZE, GRID_SPACING,
    layout::{
        BoardLayout, ClusterRegion, LayoutSeed, LayoutStats, PlacementFingerprint, layout_bounds,
        resolved_edges,
    },
    metaballs::{
        BASE_INFLUENCE_RADIUS, FIELD_CELL_BUDGET, adaptive_field_step, build_geometry,
        field_bounds, required_pair_radius,
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

pub(crate) fn prepare_board_layout(index: &VaultIndex, seed: Option<&LayoutSeed>) -> BoardLayout {
    if index.notes.is_empty() {
        return BoardLayout {
            positions: HashMap::new(),
            clusters: Vec::new(),
            edges: Vec::new(),
            stats: LayoutStats::default(),
            fingerprints: HashMap::new(),
            content_bounds: None,
        };
    }

    let mut notes: Vec<_> = index.notes.iter().collect();
    notes.sort_by_key(|note| note_sort_key(note));
    let note_tags: HashMap<_, _> = notes
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
    let usable_seed = seed.filter(|seed| seed.root == index.root);
    let unchanged: HashSet<_> = usable_seed.map_or_else(HashSet::new, |seed| {
        seed.fingerprints
            .iter()
            .filter(|(note_id, fingerprint)| {
                fingerprints.get(*note_id) == Some(*fingerprint)
                    && seed.positions.contains_key(*note_id)
            })
            .map(|(note_id, _)| note_id.clone())
            .collect()
    });
    let exact_rescan = usable_seed
        .is_some_and(|seed| seed.positions.len() == notes.len() && unchanged.len() == notes.len());

    let cluster_names = cluster_names(&notes, &note_tags);
    let cluster_members = cluster_members(&notes, &note_tags);
    let tag_weights = tag_relationship_weights(&notes, &note_tags);
    let tag_centers = tag_centers(&cluster_members, &tag_weights, usable_seed, &unchanged);
    let desired = desired_note_slots(&notes, &note_tags, &tag_centers, usable_seed, &unchanged);
    let weights: HashMap<_, _> = notes
        .iter()
        .map(|note| {
            (
                note.id.clone(),
                if unchanged.contains(&note.id) {
                    PRESERVED_WEIGHT
                } else {
                    NEW_WEIGHT
                },
            )
        })
        .collect();

    let mut positions = if exact_rescan {
        usable_seed
            .expect("an exact rescan has a seed")
            .positions
            .clone()
    } else {
        let spread = spread_shared_anchors(&notes, &note_tags, desired.clone(), &unchanged);
        slots_to_positions(pack_rows(&notes, &spread, &weights))
    };
    let desired_positions = slots_to_positions(desired.clone());
    let (refined, connectivity_passes) = if exact_rescan {
        (positions, 0)
    } else {
        refine_connectivity(
            &notes,
            &note_tags,
            positions,
            &desired_positions,
            usable_seed,
            &unchanged,
            &weights,
        )
    };
    positions = refined;

    let mut raw_radii = BTreeMap::new();
    let mut fallback_cluster_count = 0;
    for (tag, members) in &cluster_members {
        let member_positions: Vec<_> = members.iter().map(|id| positions[id]).collect();
        let components = connected_components(members, &positions);
        let raw_radius = if components.len() <= 1 {
            BASE_INFLUENCE_RADIUS
        } else {
            fallback_cluster_count += 1;
            minimum_spanning_radius(&member_positions).max(BASE_INFLUENCE_RADIUS)
        };
        raw_radii.insert(tag.clone(), raw_radius);
    }

    let mut sample_step = sample_step_for(&cluster_members, &positions, &raw_radii);
    let mut radii = raw_radii.clone();
    for _ in 0..8 {
        for (tag, raw_radius) in &raw_radii {
            radii.insert(
                tag.clone(),
                if *raw_radius > BASE_INFLUENCE_RADIUS {
                    raw_radius + sample_step
                } else {
                    *raw_radius
                },
            );
        }
        let next_step = sample_step_for(&cluster_members, &positions, &radii);
        if next_step == sample_step {
            break;
        }
        sample_step = next_step;
    }

    let mut sampled_cells = 0;
    let mut clusters = Vec::with_capacity(cluster_members.len());
    for (key, members) in cluster_members {
        let member_positions: Vec<_> = members.iter().map(|note_id| positions[note_id]).collect();
        let radius = radii[&key];
        let (bounds, geometry, cell_count) = build_geometry(&member_positions, radius, sample_step);
        debug_assert_eq!(
            geometry.contours.len(),
            1,
            "cluster {key} did not produce one connected field"
        );
        sampled_cells += cell_count;
        let label_member = members
            .iter()
            .min_by(|left, right| {
                positions[*left]
                    .y
                    .total_cmp(&positions[*right].y)
                    .then_with(|| positions[*left].x.total_cmp(&positions[*right].x))
                    .then_with(|| left.cmp(right))
            })
            .expect("clusters always have members");
        let label_anchor = positions[label_member]
            + Vec2::new(
                -CARD_SIZE.x * 0.5 + 12.0,
                -CARD_SIZE.y * 0.5 - radius * 0.35,
            );
        clusters.push(ClusterRegion {
            key: key.clone(),
            name: cluster_names[&key].clone(),
            bounds,
            label_anchor,
            note_count: members.len(),
            geometry,
            influence_radius: radius,
        });
    }
    clusters.sort_by(|left, right| left.key.cmp(&right.key));

    let edges = resolved_edges(index, &positions);
    let content_bounds = layout_bounds(&positions, &clusters);
    BoardLayout {
        positions,
        clusters,
        edges,
        stats: LayoutStats {
            connectivity_passes,
            fallback_cluster_count,
            maximum_influence_radius: radii.values().copied().fold(0.0, f32::max),
            sampled_field_cells: sampled_cells,
            field_step: sample_step,
        },
        fingerprints,
        content_bounds,
    }
}

fn cluster_names(
    notes: &[&NoteRecord],
    note_tags: &HashMap<NoteId, Vec<(String, String)>>,
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

fn cluster_members(
    notes: &[&NoteRecord],
    note_tags: &HashMap<NoteId, Vec<(String, String)>>,
) -> BTreeMap<String, Vec<NoteId>> {
    let mut members: BTreeMap<String, Vec<NoteId>> = BTreeMap::new();
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

fn tag_relationship_weights(
    notes: &[&NoteRecord],
    note_tags: &HashMap<NoteId, Vec<(String, String)>>,
) -> BTreeMap<(String, String), u32> {
    let mut weights = BTreeMap::new();
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

fn add_tag_weight(
    weights: &mut BTreeMap<(String, String), u32>,
    left: &str,
    right: &str,
    weight: u32,
) {
    let key = if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    };
    *weights.entry(key).or_default() += weight;
}

fn tag_centers(
    members: &BTreeMap<String, Vec<NoteId>>,
    weights: &BTreeMap<(String, String), u32>,
    seed: Option<&LayoutSeed>,
    unchanged: &HashSet<NoteId>,
) -> HashMap<String, Pos2> {
    let largest = members.values().map(Vec::len).max().unwrap_or(1);
    let columns = (largest as f32).sqrt().ceil() as i32;
    let rows = largest.div_ceil(columns as usize) as i32;
    let stride = (
        (columns + 2).max(3) as f32 * GRID_SPACING.x,
        (rows + 2).max(3) as f32 * GRID_SPACING.y,
    );
    let mut centers = BTreeMap::new();
    let mut occupied = HashSet::new();
    if let Some(seed) = seed {
        for (tag, tag_members) in members {
            let mut preserved: Vec<_> = tag_members
                .iter()
                .filter(|note_id| unchanged.contains(*note_id))
                .filter_map(|note_id| seed.positions.get(note_id).copied())
                .collect();
            if preserved.is_empty() {
                continue;
            }
            preserved.sort_by(|left, right| {
                left.x
                    .total_cmp(&right.x)
                    .then_with(|| left.y.total_cmp(&right.y))
            });
            let x = median(preserved.iter().map(|position| position.x).collect());
            let y = median(preserved.iter().map(|position| position.y).collect());
            let center = Pos2::new(x, y);
            occupied.insert(coarse_slot(center, stride));
            centers.insert(tag.clone(), center);
        }
    }

    let all_tags: BTreeSet<_> = members.keys().cloned().collect();
    while centers.len() < all_tags.len() {
        let next = all_tags
            .iter()
            .filter(|tag| !centers.contains_key(*tag))
            .max_by(|left, right| {
                placed_weight(left, &centers, weights)
                    .cmp(&placed_weight(right, &centers, weights))
                    .then_with(|| members[*left].len().cmp(&members[*right].len()))
                    .then_with(|| {
                        weighted_degree(left, weights).cmp(&weighted_degree(right, weights))
                    })
                    .then_with(|| right.cmp(left))
            })
            .expect("an unplaced tag exists")
            .clone();
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

fn median(mut values: Vec<f32>) -> f32 {
    values.sort_by(f32::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    }
}

fn placed_weight(
    tag: &str,
    centers: &BTreeMap<String, Pos2>,
    weights: &BTreeMap<(String, String), u32>,
) -> u32 {
    centers
        .keys()
        .map(|placed| tag_weight(tag, placed, weights))
        .sum()
}

fn weighted_degree(tag: &str, weights: &BTreeMap<(String, String), u32>) -> u32 {
    weights
        .iter()
        .filter_map(|((left, right), weight)| (left == tag || right == tag).then_some(*weight))
        .sum()
}

fn tag_weight(left: &str, right: &str, weights: &BTreeMap<(String, String), u32>) -> u32 {
    if left == right {
        return 0;
    }
    let key = if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    };
    weights.get(&key).copied().unwrap_or(0)
}

fn coarse_slot(position: Pos2, stride: (f32, f32)) -> (i32, i32) {
    (
        (position.x / stride.0).round() as i32,
        (position.y / stride.1).round() as i32,
    )
}

fn nearest_free_coarse_slot(desired: (i32, i32), occupied: &HashSet<(i32, i32)>) -> (i32, i32) {
    for radius in 0_i32.. {
        let mut candidates = square_ring(desired, radius);
        candidates.sort_by_key(|slot| {
            let dx = slot.0 - desired.0;
            let dy = slot.1 - desired.1;
            (dx * dx + dy * dy, slot.1, slot.0)
        });
        if let Some(slot) = candidates.into_iter().find(|slot| !occupied.contains(slot)) {
            return slot;
        }
    }
    unreachable!("an infinite grid always contains a free slot")
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
    note_tags: &HashMap<NoteId, Vec<(String, String)>>,
    tag_centers: &HashMap<String, Pos2>,
    seed: Option<&LayoutSeed>,
    unchanged: &HashSet<NoteId>,
) -> HashMap<NoteId, (i32, i32)> {
    let mut anchors = HashMap::new();
    for note in notes {
        if unchanged.contains(&note.id)
            && let Some(position) = seed.and_then(|seed| seed.positions.get(&note.id)).copied()
        {
            anchors.insert(note.id.clone(), position);
            continue;
        }
        let tags = &note_tags[&note.id];
        let sum = tags
            .iter()
            .fold(Vec2::ZERO, |sum, (tag, _)| sum + tag_centers[tag].to_vec2());
        anchors.insert(note.id.clone(), Pos2::ZERO + sum / tags.len() as f32);
    }

    notes
        .iter()
        .map(|note| {
            if unchanged.contains(&note.id) {
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
    note_tags: &HashMap<NoteId, Vec<(String, String)>>,
    mut desired: HashMap<NoteId, (i32, i32)>,
    unchanged: &HashSet<NoteId>,
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
            .filter(|note| !unchanged.contains(&note.id))
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
            .any(|(id, slot)| unchanged.contains(id) && *slot == anchor);
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
        let mut ring = square_ring((0, 0), radius);
        ring.sort_by_key(|slot| {
            let distance = slot.0 * slot.0 + slot.1 * slot.1;
            (distance, slot.1, slot.0)
        });
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
    weights: &HashMap<NoteId, f64>,
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
                .then_with(|| weights[&right.id].total_cmp(&weights[&left.id]))
                .then_with(|| note_sort_key(left).cmp(&note_sort_key(right)))
        });
        let mut blocks: Vec<IsotonicBlock> = Vec::new();
        for (index, note) in row_notes.iter().enumerate() {
            let weight = weights[&note.id];
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
    note_tags: &HashMap<NoteId, Vec<(String, String)>>,
    initial: HashMap<NoteId, Pos2>,
    desired: &HashMap<NoteId, Pos2>,
    seed: Option<&LayoutSeed>,
    unchanged: &HashSet<NoteId>,
    weights: &HashMap<NoteId, f64>,
) -> (HashMap<NoteId, Pos2>, usize) {
    let members = cluster_members(notes, note_tags);
    let mut current = initial;
    let mut best = current.clone();
    let mut best_score = layout_score(&best, &members, desired, seed, unchanged);
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
            let root_index = root_component(&components, unchanged);
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
        current = slots_to_positions(pack_rows(notes, &proposed, weights));
        passes = pass + 1;
        let score = layout_score(&current, &members, desired, seed, unchanged);
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
    members: &BTreeMap<String, Vec<NoteId>>,
    desired: &HashMap<NoteId, Pos2>,
    seed: Option<&LayoutSeed>,
    unchanged: &HashSet<NoteId>,
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
    let preserved_displacement = seed.map_or(0.0, |seed| {
        note_ids
            .iter()
            .copied()
            .filter(|note_id| unchanged.contains(*note_id))
            .filter_map(|note_id| Some(positions[note_id].distance(*seed.positions.get(note_id)?)))
            .sum()
    });
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

fn root_component(components: &[Vec<NoteId>], unchanged: &HashSet<NoteId>) -> usize {
    components
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            left.iter()
                .filter(|note_id| unchanged.contains(*note_id))
                .count()
                .cmp(
                    &right
                        .iter()
                        .filter(|note_id| unchanged.contains(*note_id))
                        .count(),
                )
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
    use super::{MAX_CONNECTIVITY_PASSES, compact_offset, minimum_spanning_radius};
    use crate::board::{GRID_SPACING, metaballs::BASE_INFLUENCE_RADIUS};
    use eframe::egui::Pos2;

    #[test]
    fn compact_offsets_do_not_repeat() {
        let offsets: std::collections::HashSet<_> = (0..1_000).map(compact_offset).collect();
        assert_eq!(offsets.len(), 1_000);
    }

    #[test]
    fn spanning_radius_keeps_adjacent_grid_notes_at_the_base_radius() {
        assert!(
            minimum_spanning_radius(&[Pos2::ZERO, Pos2::new(GRID_SPACING.x, GRID_SPACING.y),])
                <= BASE_INFLUENCE_RADIUS
        );
        assert_eq!(MAX_CONNECTIVITY_PASSES, 32);
    }
}
