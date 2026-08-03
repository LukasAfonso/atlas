use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use eframe::egui::{Pos2, Rect, Vec2};

use super::{CARD_SIZE, GRID_SPACING};
use crate::vault::{NoteId, NoteRecord, VaultIndex};

const MIN_CLUSTER_SPACING: Vec2 = Vec2::new(GRID_SPACING.x * 3.0, GRID_SPACING.y * 3.0);
const CLUSTER_MEMBER_FOOTPRINT: Vec2 = Vec2::new(CARD_SIZE.x * 2.0, CARD_SIZE.y * 1.65);
const CLUSTER_REGION_MARGIN: f32 = 12.0;
const RELATIONSHIP_WEIGHT: f32 = 0.28;
const UNTAGGED_KEY: &str = "\0atlas-untagged";

#[derive(Clone, Debug, PartialEq)]
pub(super) struct BoardLayout {
    pub(super) positions: HashMap<NoteId, Pos2>,
    pub(super) clusters: Vec<ClusterRegion>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClusterRegion {
    pub(super) key: String,
    pub(super) name: String,
    pub(super) center: Pos2,
    pub(super) bounds: Rect,
    pub(super) note_count: usize,
}

pub(super) fn clustered_layout(index: &VaultIndex) -> BoardLayout {
    if index.notes.is_empty() {
        return BoardLayout {
            positions: HashMap::new(),
            clusters: Vec::new(),
        };
    }

    let mut notes: Vec<_> = index.notes.iter().collect();
    notes.sort_by_key(|note| note_sort_key(note));

    let mut cluster_names = BTreeMap::new();
    let mut cluster_members: BTreeMap<String, Vec<NoteId>> = BTreeMap::new();
    let mut note_tags = HashMap::new();
    for note in &notes {
        let tags = normalized_tags(note);
        for (key, name) in &tags {
            cluster_names
                .entry(key.clone())
                .and_modify(|current: &mut String| {
                    if name.to_lowercase() < current.to_lowercase() {
                        current.clone_from(name);
                    }
                })
                .or_insert_with(|| name.clone());
            cluster_members
                .entry(key.clone())
                .or_default()
                .push(note.id.clone());
        }
        note_tags.insert(
            note.id.clone(),
            tags.into_iter().map(|(key, _)| key).collect::<Vec<_>>(),
        );
    }

    let cluster_spacing = cluster_spacing(&cluster_members);
    let cluster_centers = cluster_centers(cluster_names.keys(), cluster_spacing);
    let anchors: HashMap<_, _> = notes
        .iter()
        .map(|note| {
            let tags = &note_tags[&note.id];
            let sum = tags
                .iter()
                .fold(Vec2::ZERO, |sum, tag| sum + cluster_centers[tag].to_vec2());
            (note.id.clone(), Pos2::ZERO + sum / tags.len() as f32)
        })
        .collect();

    let desired: HashMap<_, _> = notes
        .iter()
        .map(|note| {
            let anchor = anchors[&note.id];
            let neighbors: BTreeSet<_> = note
                .references
                .iter()
                .chain(note.backlinks.iter())
                .filter(|id| anchors.contains_key(*id))
                .collect();
            let position = if neighbors.is_empty() {
                anchor
            } else {
                let neighbor_sum = neighbors
                    .iter()
                    .fold(Vec2::ZERO, |sum, id| sum + anchors[*id].to_vec2());
                let neighbor_center = Pos2::ZERO + neighbor_sum / neighbors.len() as f32;
                anchor.lerp(neighbor_center, RELATIONSHIP_WEIGHT)
            };
            let position = if note_tags[&note.id].len() == 1 {
                clamp_to_cluster_cell(
                    position,
                    cluster_centers[&note_tags[&note.id][0]],
                    cluster_spacing,
                )
            } else {
                position
            };
            (note.id.clone(), position)
        })
        .collect();

    let mut occupied = HashSet::new();
    let mut positions = HashMap::new();
    let mut placement_notes = notes.clone();
    placement_notes.sort_by_key(|note| (note_tags[&note.id].len(), note_sort_key(note)));
    for note in &placement_notes {
        let slot = nearest_free_slot(desired[&note.id], &occupied);
        occupied.insert(slot);
        positions.insert(note.id.clone(), slot_position(slot));
    }

    let clusters = cluster_names
        .into_iter()
        .map(|(key, name)| {
            let members = &cluster_members[&key];
            let bounds = members
                .iter()
                .map(|note_id| Rect::from_center_size(positions[note_id], CLUSTER_MEMBER_FOOTPRINT))
                .reduce(Rect::union)
                .unwrap_or_else(|| Rect::from_center_size(cluster_centers[&key], CARD_SIZE))
                .expand(CLUSTER_REGION_MARGIN);
            ClusterRegion {
                key: key.clone(),
                name,
                center: cluster_centers[&key],
                bounds,
                note_count: members.len(),
            }
        })
        .collect();

    BoardLayout {
        positions,
        clusters,
    }
}

fn normalized_tags(note: &NoteRecord) -> Vec<(String, String)> {
    if note.tags.is_empty() {
        return vec![(UNTAGGED_KEY.to_owned(), "Untagged".to_owned())];
    }

    let mut tags = BTreeMap::new();
    for tag in &note.tags {
        let display = tag.trim().trim_start_matches('#');
        if display.is_empty() {
            continue;
        }
        tags.entry(display.to_lowercase())
            .or_insert_with(|| display.to_owned());
    }
    if tags.is_empty() {
        vec![(UNTAGGED_KEY.to_owned(), "Untagged".to_owned())]
    } else {
        tags.into_iter().collect()
    }
}

fn cluster_spacing(cluster_members: &BTreeMap<String, Vec<NoteId>>) -> Vec2 {
    let largest_cluster = cluster_members.values().map(Vec::len).max().unwrap_or(1);
    let columns = (largest_cluster as f32).sqrt().ceil() as usize;
    let rows = largest_cluster.div_ceil(columns);
    Vec2::new(
        MIN_CLUSTER_SPACING
            .x
            .max((columns as f32 + 1.5) * GRID_SPACING.x),
        MIN_CLUSTER_SPACING
            .y
            .max((rows as f32 + 1.5) * GRID_SPACING.y),
    )
}

fn cluster_centers<'a>(
    keys: impl ExactSizeIterator<Item = &'a String>,
    spacing: Vec2,
) -> HashMap<String, Pos2> {
    let count = keys.len();
    let columns = (count as f32).sqrt().ceil() as usize;
    let rows = count.div_ceil(columns);
    keys.enumerate()
        .map(|(index, key)| {
            let column = index % columns;
            let row = index / columns;
            let x = (column as f32 - columns.saturating_sub(1) as f32 / 2.0) * spacing.x;
            let y = (row as f32 - rows.saturating_sub(1) as f32 / 2.0) * spacing.y;
            (key.clone(), Pos2::new(x, y))
        })
        .collect()
}

fn cluster_cell(center: Pos2, spacing: Vec2) -> Rect {
    Rect::from_center_size(center, spacing - GRID_SPACING)
}

fn clamp_to_cluster_cell(position: Pos2, center: Pos2, spacing: Vec2) -> Pos2 {
    let inset = CLUSTER_MEMBER_FOOTPRINT / 2.0 + Vec2::splat(CLUSTER_REGION_MARGIN);
    let cell = cluster_cell(center, spacing).shrink2(inset);
    Pos2::new(
        position.x.clamp(cell.left(), cell.right()),
        position.y.clamp(cell.top(), cell.bottom()),
    )
}

fn nearest_free_slot(desired: Pos2, occupied: &HashSet<(i32, i32)>) -> (i32, i32) {
    let origin = (
        (desired.x / GRID_SPACING.x).round() as i32,
        (desired.y / GRID_SPACING.y).round() as i32,
    );
    for radius in 0..=occupied.len() as i32 + 1 {
        let mut candidates = Vec::new();
        if radius == 0 {
            candidates.push(origin);
        } else {
            for offset in -radius..=radius {
                candidates.push((origin.0 + offset, origin.1 - radius));
                candidates.push((origin.0 + offset, origin.1 + radius));
            }
            for offset in (-radius + 1)..radius {
                candidates.push((origin.0 - radius, origin.1 + offset));
                candidates.push((origin.0 + radius, origin.1 + offset));
            }
        }
        if let Some(slot) = candidates
            .into_iter()
            .filter(|slot| !occupied.contains(slot))
            .min_by(|left, right| {
                normalized_slot_distance_sq(*left, desired)
                    .total_cmp(&normalized_slot_distance_sq(*right, desired))
                    .then_with(|| left.cmp(right))
            })
        {
            return slot;
        }
    }
    unreachable!("the expanding slot search always reaches an unoccupied cell")
}

fn normalized_slot_distance_sq(slot: (i32, i32), desired: Pos2) -> f32 {
    let desired_slot = Vec2::new(desired.x / GRID_SPACING.x, desired.y / GRID_SPACING.y);
    let delta = Vec2::new(slot.0 as f32, slot.1 as f32) - desired_slot;
    delta.length_sq()
}

fn slot_position(slot: (i32, i32)) -> Pos2 {
    Pos2::new(
        slot.0 as f32 * GRID_SPACING.x,
        slot.1 as f32 * GRID_SPACING.y,
    )
}

fn note_sort_key(note: &NoteRecord) -> (String, String) {
    let id = note.id.display();
    (id.to_lowercase(), id)
}

pub(super) fn layout_bounds(
    positions: &HashMap<NoteId, Pos2>,
    clusters: &[ClusterRegion],
) -> Option<Rect> {
    let note_bounds = positions
        .values()
        .copied()
        .map(|position| Rect::from_center_size(position, CARD_SIZE))
        .reduce(Rect::union);
    clusters
        .iter()
        .map(|cluster| cluster.bounds)
        .chain(note_bounds)
        .reduce(Rect::union)
}

pub(super) fn resolved_edges(
    index: &VaultIndex,
    positions: &HashMap<NoteId, Pos2>,
) -> Vec<BoardEdge> {
    index
        .notes
        .iter()
        .flat_map(|note| {
            let source = positions.get(&note.id).copied();
            note.references.iter().filter_map(move |target| {
                Some(BoardEdge {
                    source: note.id.clone(),
                    target: target.clone(),
                    source_position: source?,
                    target_position: positions.get(target).copied()?,
                })
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct BoardEdge {
    pub(super) source: NoteId,
    pub(super) target: NoteId,
    pub(super) source_position: Pos2,
    pub(super) target_position: Pos2,
}
