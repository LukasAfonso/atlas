use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
};

use eframe::egui::Pos2;

use super::grid::{position_slot, slots_to_positions};
use super::slots::pack_rows;
use super::{ClusterMembers, Seeding};
use crate::vault::{NoteId, NoteRecord};

pub(super) const MAX_CONNECTIVITY_PASSES: usize = 32;
const MAX_STALLED_PASSES: usize = 4;
const BUCKET_SIZE: i32 = 8;

pub(super) fn refine_connectivity(
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
        let requests = rejoin_requests(members, &current, seeding);
        if requests.is_empty() {
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

fn rejoin_requests(
    members: &ClusterMembers,
    positions: &HashMap<NoteId, Pos2>,
    seeding: &Seeding<'_>,
) -> HashMap<NoteId, (i32, i32)> {
    let mut requests: HashMap<NoteId, (i32, i32)> = HashMap::new();
    for tag_members in members.values() {
        let components = connected_components(tag_members, positions);
        if components.len() <= 1 {
            continue;
        }
        let root_index = root_component(&components, seeding);
        let root_buckets = SlotBuckets::new(&components[root_index], positions);
        for (index, component) in components.iter().enumerate() {
            if index == root_index {
                continue;
            }
            let (left, right) = closest_pair(component, &root_buckets, positions);
            let left_slot = position_slot(positions[left]);
            let right_slot = position_slot(positions[right]);
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
    requests
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

pub(super) fn connected_components(
    members: &[NoteId],
    positions: &HashMap<NoteId, Pos2>,
) -> Vec<Vec<NoteId>> {
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
        .map_or(0, |(index, _)| index)
}

struct SlotBuckets<'a> {
    buckets: BTreeMap<(i32, i32), Vec<&'a NoteId>>,
}

impl<'a> SlotBuckets<'a> {
    fn new(members: &'a [NoteId], positions: &HashMap<NoteId, Pos2>) -> Self {
        let mut buckets: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for note_id in members {
            let slot = position_slot(positions[note_id]);
            buckets
                .entry((
                    slot.0.div_euclid(BUCKET_SIZE),
                    slot.1.div_euclid(BUCKET_SIZE),
                ))
                .or_default()
                .push(note_id);
        }
        Self { buckets }
    }
}

fn closest_pair<'a>(
    left: &'a [NoteId],
    right: &SlotBuckets<'a>,
    positions: &HashMap<NoteId, Pos2>,
) -> (&'a NoteId, &'a NoteId) {
    let mut best: Option<(i64, &NoteId, &NoteId)> = None;
    for left_id in left {
        let left_slot = position_slot(positions[left_id]);
        for (bucket, right_ids) in &right.buckets {
            if best
                .is_some_and(|(distance, _, _)| bucket_lower_bound(left_slot, *bucket) > distance)
            {
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

fn bucket_lower_bound(slot: (i32, i32), bucket: (i32, i32)) -> i64 {
    let minimum = (bucket.0 * BUCKET_SIZE, bucket.1 * BUCKET_SIZE);
    let maximum = (minimum.0 + BUCKET_SIZE - 1, minimum.1 + BUCKET_SIZE - 1);
    let dx = distance_to_interval(slot.0, minimum.0, maximum.0);
    let dy = distance_to_interval(slot.1, minimum.1, maximum.1);
    dx * dx + dy * dy
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

#[cfg(test)]
mod tests {
    use super::MAX_CONNECTIVITY_PASSES;

    #[test]
    fn connectivity_refinement_is_bounded() {
        assert_eq!(MAX_CONNECTIVITY_PASSES, 32);
    }
}
