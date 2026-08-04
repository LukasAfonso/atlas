use std::collections::{BTreeMap, BTreeSet};

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
    let field = ScalarField::sample(positions, radius, step, source_bounds);
    let geometry = field.march();
    (field.bounds, geometry, field.cell_count())
}

struct ScalarField {
    bounds: Rect,
    step: f32,
    columns: usize,
    rows: usize,
    corners: Vec<f32>,
    centers: Vec<f32>,
}

impl ScalarField {
    fn sample(positions: &[Pos2], radius: f32, step: f32, source_bounds: Rect) -> Self {
        let bounds = source_bounds.expand(step);
        let columns = ((bounds.width() / step).ceil() as usize).max(1) + 1;
        let rows = ((bounds.height() / step).ceil() as usize).max(1) + 1;
        let mut field = Self {
            bounds,
            step,
            columns,
            rows,
            corners: vec![0.0; columns * rows],
            centers: vec![0.0; columns.saturating_sub(1) * rows.saturating_sub(1)],
        };

        for position in positions {
            let card = Rect::from_center_size(*position, CARD_SIZE);
            field.accumulate_corners(card, radius);
            field.accumulate_centers(card, radius);
        }
        field
    }

    fn accumulate_corners(&mut self, card: Rect, radius: f32) {
        let (columns, step, bounds) = (self.columns, self.step, self.bounds);
        for (column, row) in influenced_cells(card, radius, bounds, step, columns, self.rows, 0.0) {
            let point = self.grid_position(column, row, 0.0);
            self.corners[row * columns + column] += influence_at(point, card, radius);
        }
    }

    fn accumulate_centers(&mut self, card: Rect, radius: f32) {
        if self.columns < 2 || self.rows < 2 {
            return;
        }
        let (columns, step, bounds) = (self.columns - 1, self.step, self.bounds);
        for (column, row) in
            influenced_cells(card, radius, bounds, step, columns, self.rows - 1, 0.5)
        {
            let point = self.grid_position(column, row, 0.5);
            self.centers[row * columns + column] += influence_at(point, card, radius);
        }
    }

    fn grid_position(&self, column: usize, row: usize, offset: f32) -> Pos2 {
        self.bounds.min + Vec2::new(column as f32 + offset, row as f32 + offset) * self.step
    }

    fn corner(&self, column: usize, row: usize) -> Sample {
        Sample {
            position: self.grid_position(column, row, 0.0),
            value: self.corners[row * self.columns + column],
        }
    }

    fn center(&self, column: usize, row: usize) -> Sample {
        Sample {
            position: self.grid_position(column, row, 0.5),
            value: self.centers[row * (self.columns - 1) + column],
        }
    }

    fn cell_count(&self) -> usize {
        self.columns.saturating_sub(1) * self.rows.saturating_sub(1)
    }

    fn march(&self) -> ClusterGeometry {
        let mut geometry = ClusterGeometry::default();
        let mut boundary_edges = BTreeSet::new();

        for row in 0..self.rows - 1 {
            for column in 0..self.columns - 1 {
                let corners = [
                    self.corner(column, row),
                    self.corner(column + 1, row),
                    self.corner(column + 1, row + 1),
                    self.corner(column, row + 1),
                ];
                let center = self.center(column, row);
                for (left, right) in cell_contours(corners, center.value) {
                    register_edge(left, right, &mut boundary_edges);
                }
                for [left, right] in [[0, 1], [1, 2], [2, 3], [3, 0]] {
                    push_polygon(
                        &mut geometry,
                        clip_triangle([corners[left], corners[right], center]),
                    );
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
        geometry
    }
}

fn influenced_cells(
    card: Rect,
    radius: f32,
    bounds: Rect,
    step: f32,
    columns: usize,
    rows: usize,
    offset: f32,
) -> impl Iterator<Item = (usize, usize)> {
    let influence = card.expand(radius);
    let range = |low: f32, high: f32, origin: f32, count: usize| {
        let last = count as isize - 1;
        let min = (((low - origin) / step - offset).floor() as isize).clamp(0, last) as usize;
        let max = (((high - origin) / step - offset).ceil() as isize).clamp(0, last) as usize;
        min..=max
    };
    let column_range = range(influence.left(), influence.right(), bounds.left(), columns);
    let row_range = range(influence.top(), influence.bottom(), bounds.top(), rows);
    row_range.flat_map(move |row| column_range.clone().map(move |column| (column, row)))
}

fn influence_at(point: Pos2, card: Rect, radius: f32) -> f32 {
    let distance = distance_to_rect(point, card);
    if distance >= radius {
        return 0.0;
    }
    let normalized = 1.0 - distance / radius;
    normalized * normalized
}

fn push_polygon(geometry: &mut ClusterGeometry, polygon: Vec<Pos2>) {
    if polygon.len() < 3 {
        return;
    }
    let first = geometry.vertices.len() as u32;
    geometry.vertices.extend(polygon.iter().copied());
    for offset in 1..polygon.len() - 1 {
        geometry
            .indices
            .extend([first, first + offset as u32, first + offset as u32 + 1]);
    }
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
        .map(|position| influence_at(point, Rect::from_center_size(*position, CARD_SIZE), radius))
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

fn register_edge(left: Pos2, right: Pos2, edges: &mut BTreeSet<EdgeKey>) {
    let left = PointKey::new(left);
    let right = PointKey::new(right);
    if left != right {
        edges.insert(EdgeKey::new(left, right));
    }
}

fn join_contours(edges: BTreeSet<EdgeKey>) -> (Vec<Vec<Pos2>>, usize) {
    let mut adjacency: BTreeMap<PointKey, BTreeSet<PointKey>> = BTreeMap::new();
    for key in &edges {
        adjacency.entry(key.0).or_default().insert(key.1);
        adjacency.entry(key.1).or_default().insert(key.0);
    }
    let mut unused = edges;

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
            contours.push(keys.into_iter().map(PointKey::position).collect());
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
