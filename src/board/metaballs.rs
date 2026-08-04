use std::collections::{BTreeMap, BTreeSet, HashMap};

use eframe::egui::{Pos2, Rect, Vec2};

use super::CARD_SIZE;

pub(super) const BASE_INFLUENCE_RADIUS: f32 = 200.0;
pub(super) const FIELD_THRESHOLD: f32 = 0.25;
// Keep samples that land exactly on the mathematical isovalue out of the
// region. Without a tiny symbolic offset, a contour can pass through a grid
// vertex and create four-way graph junctions whose topology depends on the
// world-space translation of the same card arrangement.
const MESH_ISO_THRESHOLD: f32 = FIELD_THRESHOLD + 0.000_1;
pub(super) const MIN_FIELD_STEP: f32 = 20.0;
pub(super) const FIELD_CELL_BUDGET: f32 = 500_000.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ClusterGeometry {
    pub(super) vertices: Vec<Pos2>,
    pub(super) indices: Vec<u32>,
    pub(super) contours: Vec<Vec<Pos2>>,
}

pub(super) fn field_bounds(positions: &[Pos2], radius: f32) -> Option<Rect> {
    positions
        .iter()
        .copied()
        .map(|position| Rect::from_center_size(position, CARD_SIZE).expand(radius))
        .reduce(Rect::union)
}

pub(super) fn adaptive_field_step(areas: impl Iterator<Item = f32>) -> f32 {
    let total_area: f32 = areas.filter(|area| area.is_finite() && *area > 0.0).sum();
    let raw = (total_area / FIELD_CELL_BUDGET).sqrt().max(MIN_FIELD_STEP);
    (raw / 4.0).ceil() * 4.0
}

pub(super) fn build_geometry(
    positions: &[Pos2],
    radius: f32,
    step: f32,
) -> (Rect, ClusterGeometry, usize) {
    let Some(source_bounds) = field_bounds(positions, radius) else {
        return (Rect::NOTHING, ClusterGeometry::default(), 0);
    };
    let bounds = source_bounds.expand(step);
    let columns = ((bounds.width() / step).ceil() as usize).max(1) + 1;
    let rows = ((bounds.height() / step).ceil() as usize).max(1) + 1;
    let mut field = vec![0.0_f32; columns * rows];
    let mut centers = vec![0.0_f32; columns.saturating_sub(1) * rows.saturating_sub(1)];

    for position in positions {
        let card = Rect::from_center_size(*position, CARD_SIZE);
        let influence = card.expand(radius);
        let min_column = (((influence.left() - bounds.left()) / step).floor() as isize)
            .clamp(0, columns as isize - 1) as usize;
        let max_column = (((influence.right() - bounds.left()) / step).ceil() as isize)
            .clamp(0, columns as isize - 1) as usize;
        let min_row = (((influence.top() - bounds.top()) / step).floor() as isize)
            .clamp(0, rows as isize - 1) as usize;
        let max_row = (((influence.bottom() - bounds.top()) / step).ceil() as isize)
            .clamp(0, rows as isize - 1) as usize;

        for row in min_row..=max_row {
            let y = bounds.top() + row as f32 * step;
            for column in min_column..=max_column {
                let x = bounds.left() + column as f32 * step;
                let distance = distance_to_rect(Pos2::new(x, y), card);
                if distance < radius {
                    let normalized = 1.0 - distance / radius;
                    field[row * columns + column] += normalized * normalized;
                }
            }
        }
        if columns > 1 && rows > 1 {
            let min_center_column = min_column.saturating_sub(1);
            let max_center_column = max_column.min(columns - 2);
            let min_center_row = min_row.saturating_sub(1);
            let max_center_row = max_row.min(rows - 2);
            for row in min_center_row..=max_center_row {
                let y = bounds.top() + (row as f32 + 0.5) * step;
                for column in min_center_column..=max_center_column {
                    let x = bounds.left() + (column as f32 + 0.5) * step;
                    let distance = distance_to_rect(Pos2::new(x, y), card);
                    if distance < radius {
                        let normalized = 1.0 - distance / radius;
                        centers[row * (columns - 1) + column] += normalized * normalized;
                    }
                }
            }
        }
    }

    let mut geometry = ClusterGeometry::default();
    let mut boundary_edges: HashMap<EdgeKey, BoundaryEdge> = HashMap::new();
    for row in 0..rows - 1 {
        for column in 0..columns - 1 {
            let top_left = Sample {
                position: Pos2::new(
                    bounds.left() + column as f32 * step,
                    bounds.top() + row as f32 * step,
                ),
                value: field[row * columns + column],
            };
            let top_right = Sample {
                position: top_left.position + Vec2::new(step, 0.0),
                value: field[row * columns + column + 1],
            };
            let bottom_right = Sample {
                position: top_left.position + Vec2::splat(step),
                value: field[(row + 1) * columns + column + 1],
            };
            let bottom_left = Sample {
                position: top_left.position + Vec2::new(0.0, step),
                value: field[(row + 1) * columns + column],
            };
            let center = Sample {
                position: top_left.position + Vec2::splat(step * 0.5),
                value: centers[row * (columns - 1) + column],
            };
            for (left, right) in cell_contours(
                [top_left, top_right, bottom_right, bottom_left],
                center.value,
            ) {
                register_edge(left, right, &mut boundary_edges);
            }
            for triangle in [
                [top_left, top_right, center],
                [top_right, bottom_right, center],
                [bottom_right, bottom_left, center],
                [bottom_left, top_left, center],
            ] {
                let polygon = clip_triangle(triangle);
                if polygon.len() < 3 {
                    continue;
                }
                let first = geometry.vertices.len() as u32;
                geometry.vertices.extend(polygon.iter().copied());
                for offset in 1..polygon.len() - 1 {
                    geometry.indices.extend([
                        first,
                        first + offset as u32,
                        first + offset as u32 + 1,
                    ]);
                }
            }
        }
    }

    let (contours, open_contour_count) = join_contours(boundary_edges);
    geometry.contours = contours;
    debug_assert_eq!(
        open_contour_count, 0,
        "marching squares emitted open chains"
    );
    debug_assert!(geometry.vertices.iter().all(|point| point.is_finite()));
    debug_assert!(
        geometry
            .contours
            .iter()
            .flatten()
            .all(|point| point.is_finite())
    );
    debug_assert!(
        geometry
            .indices
            .iter()
            .all(|index| (*index as usize) < geometry.vertices.len())
    );
    let cell_count = columns.saturating_sub(1) * rows.saturating_sub(1);
    (bounds, geometry, cell_count)
}

pub(super) fn required_pair_radius(left: Pos2, right: Pos2) -> f32 {
    let left = Rect::from_center_size(left, CARD_SIZE);
    let right = Rect::from_center_size(right, CARD_SIZE);
    let dx = if left.right() < right.left() {
        right.left() - left.right()
    } else if right.right() < left.left() {
        left.left() - right.right()
    } else {
        0.0
    };
    let dy = if left.bottom() < right.top() {
        right.top() - left.bottom()
    } else if right.bottom() < left.top() {
        left.top() - right.bottom()
    } else {
        0.0
    };
    let gap = Vec2::new(dx, dy).length();
    let denominator = 2.0 * (1.0 - (FIELD_THRESHOLD / 2.0).sqrt());
    gap / denominator
}

pub(super) fn field_value_at(positions: &[Pos2], radius: f32, point: Pos2) -> f32 {
    positions
        .iter()
        .map(|position| {
            let distance = distance_to_rect(point, Rect::from_center_size(*position, CARD_SIZE));
            if distance >= radius {
                0.0
            } else {
                let normalized = 1.0 - distance / radius;
                normalized * normalized
            }
        })
        .sum()
}

fn distance_to_rect(point: Pos2, rect: Rect) -> f32 {
    let dx = (rect.left() - point.x).max(0.0).max(point.x - rect.right());
    let dy = (rect.top() - point.y).max(0.0).max(point.y - rect.bottom());
    Vec2::new(dx, dy).length()
}

#[derive(Clone, Copy)]
struct Sample {
    position: Pos2,
    value: f32,
}

fn clip_triangle(triangle: [Sample; 3]) -> Vec<Pos2> {
    let mut output = Vec::with_capacity(5);
    for index in 0..triangle.len() {
        let current = triangle[index];
        let next = triangle[(index + 1) % triangle.len()];
        let current_inside = current.value >= MESH_ISO_THRESHOLD;
        let next_inside = next.value >= MESH_ISO_THRESHOLD;
        match (current_inside, next_inside) {
            (true, true) => output.push(next.position),
            (true, false) => output.push(intersection(current, next)),
            (false, true) => {
                output.push(intersection(current, next));
                output.push(next.position);
            }
            (false, false) => {}
        }
    }
    output
}

fn intersection(left: Sample, right: Sample) -> Pos2 {
    let denominator = right.value - left.value;
    let progress = if denominator.abs() <= f32::EPSILON {
        0.5
    } else {
        ((MESH_ISO_THRESHOLD - left.value) / denominator).clamp(0.0, 1.0)
    };
    left.position.lerp(right.position, progress)
}

fn cell_contours(corners: [Sample; 4], center_value: f32) -> Vec<(Pos2, Pos2)> {
    let mut crossings: [Option<Pos2>; 4] = [None; 4];
    for (edge, (left, right)) in [(0, 1), (1, 2), (2, 3), (3, 0)].into_iter().enumerate() {
        let left_inside = corners[left].value >= MESH_ISO_THRESHOLD;
        let right_inside = corners[right].value >= MESH_ISO_THRESHOLD;
        if left_inside != right_inside {
            crossings[edge] = Some(intersection(corners[left], corners[right]));
        }
    }
    let present: Vec<_> = crossings
        .iter()
        .enumerate()
        .filter_map(|(edge, point)| point.map(|point| (edge, point)))
        .collect();
    match present.as_slice() {
        [(_, left), (_, right)] => vec![(*left, *right)],
        [_, _, _, _] => {
            let mask = corners
                .iter()
                .enumerate()
                .fold(0_u8, |mask, (index, sample)| {
                    mask | (u8::from(sample.value >= MESH_ISO_THRESHOLD) << index)
                });
            let pair_adjacent = match mask {
                0b0101 => center_value >= MESH_ISO_THRESHOLD,
                0b1010 => center_value < MESH_ISO_THRESHOLD,
                _ => true,
            };
            if pair_adjacent {
                vec![(present[0].1, present[1].1), (present[2].1, present[3].1)]
            } else {
                vec![(present[0].1, present[3].1), (present[1].1, present[2].1)]
            }
        }
        _ => Vec::new(),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct PointKey(i64, i64);

impl PointKey {
    fn new(point: Pos2) -> Self {
        Self(
            (point.x as f64 * 4.0).round() as i64,
            (point.y as f64 * 4.0).round() as i64,
        )
    }

    fn position(self) -> Pos2 {
        Pos2::new(self.0 as f32 / 4.0, self.1 as f32 / 4.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct EdgeKey(PointKey, PointKey);

impl EdgeKey {
    fn new(left: PointKey, right: PointKey) -> Self {
        if left <= right {
            Self(left, right)
        } else {
            Self(right, left)
        }
    }
}

struct BoundaryEdge {
    count: usize,
    left: PointKey,
    right: PointKey,
}

fn register_edge(left: Pos2, right: Pos2, edges: &mut HashMap<EdgeKey, BoundaryEdge>) {
    let left_key = PointKey::new(left);
    let right_key = PointKey::new(right);
    if left_key == right_key {
        return;
    }
    let key = EdgeKey::new(left_key, right_key);
    edges
        .entry(key)
        .and_modify(|edge| edge.count += 1)
        .or_insert(BoundaryEdge {
            count: 1,
            left: left_key,
            right: right_key,
        });
}

fn join_contours(edges: HashMap<EdgeKey, BoundaryEdge>) -> (Vec<Vec<Pos2>>, usize) {
    let mut positions = BTreeMap::new();
    let mut adjacency: BTreeMap<PointKey, BTreeSet<PointKey>> = BTreeMap::new();
    let mut unused = BTreeSet::new();
    for (key, edge) in edges {
        let _duplicate_count = edge.count;
        positions.insert(edge.left, edge.left.position());
        positions.insert(edge.right, edge.right.position());
        adjacency.entry(edge.left).or_default().insert(edge.right);
        adjacency.entry(edge.right).or_default().insert(edge.left);
        unused.insert(key);
    }

    let mut contours: Vec<Vec<Pos2>> = Vec::new();
    let mut open_contour_count = 0;
    while let Some(first_edge) = unused.first().copied() {
        let start = first_edge.0;
        let mut previous = start;
        let mut current = first_edge.1;
        let mut keys = vec![start];
        unused.remove(&first_edge);

        while current != start {
            keys.push(current);
            let Some(neighbors) = adjacency.get(&current) else {
                break;
            };
            let next = neighbors
                .iter()
                .copied()
                .find(|neighbor| {
                    *neighbor != previous && unused.contains(&EdgeKey::new(current, *neighbor))
                })
                .or_else(|| {
                    neighbors
                        .iter()
                        .copied()
                        .find(|neighbor| unused.contains(&EdgeKey::new(current, *neighbor)))
                });
            let Some(next) = next else {
                break;
            };
            unused.remove(&EdgeKey::new(current, next));
            previous = current;
            current = next;
        }

        if current == start && keys.len() >= 3 {
            canonicalize_loop(&mut keys);
            contours.push(
                keys.into_iter()
                    .filter_map(|key| positions.get(&key).copied())
                    .collect(),
            );
        } else {
            open_contour_count += 1;
        }
    }
    contours.sort_by(|left, right| {
        let left = left.first().copied().map(PointKey::new);
        let right = right.first().copied().map(PointKey::new);
        left.cmp(&right)
    });
    (contours, open_contour_count)
}

fn canonicalize_loop(points: &mut [PointKey]) {
    let Some((minimum_index, _)) = points.iter().enumerate().min_by_key(|(_, point)| **point)
    else {
        return;
    };
    points.rotate_left(minimum_index);
    if points.len() > 2 && points[1] > points[points.len() - 1] {
        points[1..].reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::{BASE_INFLUENCE_RADIUS, MIN_FIELD_STEP, build_geometry, required_pair_radius};
    use crate::board::GRID_SPACING;
    use eframe::egui::Pos2;

    #[test]
    fn one_source_produces_a_closed_finite_region() {
        let (_, geometry, _) = build_geometry(&[Pos2::ZERO], BASE_INFLUENCE_RADIUS, MIN_FIELD_STEP);
        assert!(!geometry.vertices.is_empty());
        assert!(!geometry.indices.is_empty());
        assert_eq!(geometry.contours.len(), 1);
        assert!(geometry.contours[0].len() >= 8);
        assert!(geometry.vertices.iter().all(|point| point.is_finite()));
    }

    #[test]
    fn translated_source_produces_the_same_closed_topology() {
        let (_, origin, _) = build_geometry(&[Pos2::ZERO], BASE_INFLUENCE_RADIUS, MIN_FIELD_STEP);
        let (_, translated, _) = build_geometry(
            &[Pos2::new(-1_880.0, 0.0)],
            BASE_INFLUENCE_RADIUS,
            MIN_FIELD_STEP,
        );
        assert_eq!(origin.contours.len(), 1);
        assert_eq!(translated.contours.len(), 1);
        assert_eq!(origin.contours[0].len(), translated.contours[0].len());
    }

    #[test]
    fn neighboring_grid_slots_merge_at_the_base_radius() {
        for neighbor in [
            Pos2::new(GRID_SPACING.x, 0.0),
            Pos2::new(0.0, GRID_SPACING.y),
            Pos2::new(GRID_SPACING.x, GRID_SPACING.y),
        ] {
            assert!(required_pair_radius(Pos2::ZERO, neighbor) <= BASE_INFLUENCE_RADIUS);
            let (_, geometry, _) = build_geometry(
                &[Pos2::ZERO, neighbor],
                BASE_INFLUENCE_RADIUS,
                MIN_FIELD_STEP,
            );
            assert_eq!(geometry.contours.len(), 1, "neighbor {neighbor:?}");
        }
    }
}
