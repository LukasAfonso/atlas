use std::collections::HashMap;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::vault::{NoteId, NoteRecord, VaultIndex};

const CARD_SIZE: Vec2 = Vec2::new(230.0, 112.0);
const GRID_SPACING: Vec2 = Vec2::new(284.0, 168.0);
const MIN_SCALE: f32 = 0.08;
const MAX_SCALE: f32 = 3.5;
const MARKER_THRESHOLD: f32 = 0.28;
const TITLE_THRESHOLD: f32 = 0.74;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetailLevel {
    Markers,
    Titles,
    Metadata,
}

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub center: Pos2,
    pub scale: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            center: Pos2::ZERO,
            scale: 1.0,
        }
    }
}

impl Camera {
    pub fn world_to_screen(self, world: Pos2, viewport: Rect) -> Pos2 {
        viewport.center() + (world - self.center) * self.scale
    }

    pub fn screen_to_world(self, screen: Pos2, viewport: Rect) -> Pos2 {
        self.center + (screen - viewport.center()) / self.scale
    }

    pub fn zoom_at(&mut self, screen_anchor: Pos2, zoom_delta: f32, viewport: Rect) {
        if !zoom_delta.is_finite() || zoom_delta <= 0.0 {
            return;
        }
        let world_anchor = self.screen_to_world(screen_anchor, viewport);
        self.scale = (self.scale * zoom_delta).clamp(MIN_SCALE, MAX_SCALE);
        self.center = world_anchor - (screen_anchor - viewport.center()) / self.scale;
    }

    pub fn pan_by_screen_delta(&mut self, screen_delta: Vec2) {
        self.center -= screen_delta / self.scale;
    }
}

#[derive(Debug)]
pub struct BoardState {
    pub camera: Camera,
    pub debug_open: bool,
    positions: HashMap<NoteId, Pos2>,
    edges: Vec<(Pos2, Pos2)>,
    content_bounds: Option<Rect>,
    needs_fit: bool,
    pub visible_notes: usize,
}

impl Default for BoardState {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            debug_open: false,
            positions: HashMap::new(),
            edges: Vec::new(),
            content_bounds: None,
            needs_fit: true,
            visible_notes: 0,
        }
    }
}

#[derive(Debug, Default)]
pub struct BoardOutput {
    pub selection_request: Option<Option<NoteId>>,
}

impl BoardState {
    pub fn rebuild(&mut self, index: &VaultIndex) {
        self.positions = grid_layout(&index.notes);
        self.edges = resolved_edges(index, &self.positions);
        self.content_bounds = layout_bounds(&self.positions);
        self.camera = Camera::default();
        self.needs_fit = true;
        self.visible_notes = 0;
    }

    pub fn request_fit(&mut self) {
        self.needs_fit = true;
    }

    pub fn detail_level(&self) -> DetailLevel {
        detail_level(self.camera.scale)
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        index: &VaultIndex,
        selected_note: Option<&NoteId>,
    ) -> BoardOutput {
        let desired_size = ui.available_size().max(Vec2::splat(1.0));
        let (response, painter) = ui.allocate_painter(desired_size, Sense::click_and_drag());
        let cursor = if response.dragged() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        };
        let response = response.on_hover_cursor(cursor);
        let viewport = response.rect;

        if self.needs_fit {
            self.fit_to_viewport(viewport);
            self.needs_fit = false;
        }

        if response.hovered() {
            let zoom_delta = ui.input(|input| input.zoom_delta());
            if (zoom_delta - 1.0).abs() > f32::EPSILON
                && let Some(pointer) = response.hover_pos()
            {
                self.camera.zoom_at(pointer, zoom_delta, viewport);
            }

            let scroll_delta = ui.input(|input| input.smooth_scroll_delta);
            if scroll_delta != Vec2::ZERO {
                self.camera.pan_by_screen_delta(scroll_delta);
            }
        }

        if response.dragged_by(egui::PointerButton::Primary) {
            self.camera.pan_by_screen_delta(response.drag_delta());
        }

        painter.rect_filled(viewport, 0.0, Color32::from_rgb(244, 245, 240));
        self.paint_grid(&painter, viewport);
        self.paint_edges(&painter, viewport);

        let level = self.detail_level();
        let mut visible = Vec::new();
        for note in &index.notes {
            let Some(world_position) = self.positions.get(&note.id).copied() else {
                continue;
            };
            let rect = self.note_screen_rect(world_position, viewport, level);
            if viewport.intersects(rect) {
                visible.push((note, rect, world_position));
            }
        }
        self.visible_notes = visible.len();

        for (note, rect, _) in &visible {
            self.paint_note(
                &painter,
                note,
                *rect,
                level,
                selected_note == Some(&note.id),
            );
        }

        let mut output = BoardOutput::default();
        if response.clicked() {
            let hit = response
                .interact_pointer_pos()
                .and_then(|pointer| hit_test(pointer, &visible));
            output.selection_request = Some(hit);
        }

        if self.debug_open {
            let frame_ms = ui.input(|input| input.stable_dt * 1_000.0);
            painter.text(
                viewport.left_top() + Vec2::new(10.0, 10.0),
                Align2::LEFT_TOP,
                format!(
                    "{frame_ms:.1} ms · {} / {} visible · {:.2}× · {:?}",
                    self.visible_notes,
                    index.notes.len(),
                    self.camera.scale,
                    level
                ),
                FontId::monospace(11.0),
                Color32::from_rgb(77, 83, 75),
            );
        }

        output
    }

    fn fit_to_viewport(&mut self, viewport: Rect) {
        let Some(bounds) = self.content_bounds else {
            self.camera = Camera::default();
            return;
        };
        let available = (viewport.size() - Vec2::splat(72.0)).max(Vec2::splat(1.0));
        let scale_x = available.x / bounds.width().max(1.0);
        let scale_y = available.y / bounds.height().max(1.0);
        self.camera.center = bounds.center();
        self.camera.scale = scale_x.min(scale_y).clamp(MIN_SCALE, 1.0);
    }

    fn note_screen_rect(&self, world: Pos2, viewport: Rect, level: DetailLevel) -> Rect {
        let center = self.camera.world_to_screen(world, viewport);
        match level {
            DetailLevel::Markers => Rect::from_center_size(center, Vec2::splat(14.0)),
            DetailLevel::Titles | DetailLevel::Metadata => {
                Rect::from_center_size(center, CARD_SIZE * self.camera.scale)
            }
        }
    }

    fn paint_grid(&self, painter: &egui::Painter, viewport: Rect) {
        let spacing = (96.0 * self.camera.scale).clamp(24.0, 96.0);
        let origin = self.camera.world_to_screen(Pos2::ZERO, viewport);
        let start_x = viewport.left() + (origin.x - viewport.left()).rem_euclid(spacing);
        let start_y = viewport.top() + (origin.y - viewport.top()).rem_euclid(spacing);
        let color = Color32::from_rgba_unmultiplied(86, 96, 86, 20);
        let mut x = start_x;
        while x <= viewport.right() {
            let mut y = start_y;
            while y <= viewport.bottom() {
                painter.circle_filled(Pos2::new(x, y), 1.0, color);
                y += spacing;
            }
            x += spacing;
        }
    }

    fn paint_edges(&self, painter: &egui::Painter, viewport: Rect) {
        let color = Color32::from_rgba_unmultiplied(71, 104, 88, 46);
        for &(source, target) in &self.edges {
            let source = self.camera.world_to_screen(source, viewport);
            let target = self.camera.world_to_screen(target, viewport);
            if viewport.intersects(Rect::from_two_pos(source, target).expand(2.0)) {
                painter.line_segment([source, target], Stroke::new(1.0, color));
            }
        }
    }

    fn paint_note(
        &self,
        painter: &egui::Painter,
        note: &NoteRecord,
        rect: Rect,
        level: DetailLevel,
        selected: bool,
    ) {
        let accent = note_color(note);
        if level == DetailLevel::Markers {
            painter.circle_filled(rect.center(), if selected { 6.5 } else { 4.5 }, accent);
            if selected {
                painter.circle_stroke(rect.center(), 8.0, Stroke::new(2.0, Color32::WHITE));
            }
            return;
        }

        let fill = if selected {
            Color32::from_rgb(250, 250, 246)
        } else {
            Color32::from_rgb(253, 252, 248)
        };
        let shadow_rect = rect.translate(Vec2::new(0.0, 3.0));
        painter.rect_filled(
            shadow_rect,
            9.0,
            Color32::from_rgba_unmultiplied(45, 54, 45, 24),
        );
        painter.rect_filled(rect, 9.0, fill);
        painter.rect_filled(
            Rect::from_min_max(rect.min, Pos2::new(rect.min.x + 4.0, rect.max.y)),
            4.0,
            accent,
        );
        painter.rect_stroke(
            rect,
            9.0,
            Stroke::new(
                if selected { 2.0 } else { 1.0 },
                if selected {
                    Color32::from_rgb(65, 104, 86)
                } else {
                    Color32::from_rgba_unmultiplied(72, 79, 69, 42)
                },
            ),
            StrokeKind::Inside,
        );

        let padding = (14.0 * self.camera.scale).clamp(8.0, 18.0);
        let font_sizes = font_sizes(level, self.camera.scale);
        let content_painter = painter.with_clip_rect(rect.shrink(padding.min(rect.width() / 4.0)));
        content_painter.text(
            rect.left_top() + Vec2::new(padding, padding),
            Align2::LEFT_TOP,
            &note.title,
            FontId::proportional(font_sizes.title),
            Color32::from_rgb(35, 39, 34),
        );

        if level == DetailLevel::Metadata {
            let tag_text = note
                .tags
                .iter()
                .take(3)
                .map(|tag| format!("#{tag}"))
                .collect::<Vec<_>>()
                .join("  ");
            if !tag_text.is_empty() {
                content_painter.text(
                    rect.left_bottom() + Vec2::new(padding, -padding - 15.0),
                    Align2::LEFT_BOTTOM,
                    tag_text,
                    FontId::proportional(font_sizes.metadata),
                    Color32::from_rgb(88, 104, 93),
                );
            }
            content_painter.text(
                rect.right_bottom() + Vec2::new(-padding, -padding),
                Align2::RIGHT_BOTTOM,
                format!(
                    "{} ↗  {} ↙  {} @",
                    note.references.len(),
                    note.backlinks.len(),
                    note.citations.len()
                ),
                FontId::proportional(font_sizes.counts),
                Color32::from_rgb(112, 116, 108),
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FontSizes {
    title: f32,
    metadata: f32,
    counts: f32,
}

fn font_sizes(level: DetailLevel, scale: f32) -> FontSizes {
    match level {
        DetailLevel::Markers => FontSizes {
            title: 0.0,
            metadata: 0.0,
            counts: 0.0,
        },
        DetailLevel::Titles => FontSizes {
            title: 11.0,
            metadata: 0.0,
            counts: 0.0,
        },
        DetailLevel::Metadata if scale < 1.4 => FontSizes {
            title: 16.0,
            metadata: 10.0,
            counts: 9.0,
        },
        DetailLevel::Metadata => FontSizes {
            title: 20.0,
            metadata: 12.0,
            counts: 11.0,
        },
    }
}

pub fn detail_level(scale: f32) -> DetailLevel {
    if scale < MARKER_THRESHOLD {
        DetailLevel::Markers
    } else if scale < TITLE_THRESHOLD {
        DetailLevel::Titles
    } else {
        DetailLevel::Metadata
    }
}

fn grid_layout(notes: &[NoteRecord]) -> HashMap<NoteId, Pos2> {
    if notes.is_empty() {
        return HashMap::new();
    }
    let columns = (notes.len() as f32).sqrt().ceil() as usize;
    let rows = notes.len().div_ceil(columns);
    notes
        .iter()
        .enumerate()
        .map(|(index, note)| {
            let column = index % columns;
            let row = index / columns;
            let x = (column as f32 - (columns.saturating_sub(1) as f32 / 2.0)) * GRID_SPACING.x;
            let y = (row as f32 - (rows.saturating_sub(1) as f32 / 2.0)) * GRID_SPACING.y;
            (note.id.clone(), Pos2::new(x, y))
        })
        .collect()
}

fn layout_bounds(positions: &HashMap<NoteId, Pos2>) -> Option<Rect> {
    let mut positions = positions.values().copied();
    let first = positions.next()?;
    let mut bounds = Rect::from_center_size(first, CARD_SIZE);
    for position in positions {
        bounds = bounds.union(Rect::from_center_size(position, CARD_SIZE));
    }
    Some(bounds)
}

fn resolved_edges(index: &VaultIndex, positions: &HashMap<NoteId, Pos2>) -> Vec<(Pos2, Pos2)> {
    index
        .notes
        .iter()
        .flat_map(|note| {
            let source = positions.get(&note.id).copied();
            note.references
                .iter()
                .filter_map(move |target| Some((source?, positions.get(target).copied()?)))
        })
        .collect()
}

fn hit_test(pointer: Pos2, visible: &[(&NoteRecord, Rect, Pos2)]) -> Option<NoteId> {
    visible
        .iter()
        .rev()
        .find(|(_, rect, _)| rect.expand(3.0).contains(pointer))
        .map(|(note, _, _)| note.id.clone())
}

fn note_color(note: &NoteRecord) -> Color32 {
    const PALETTE: [Color32; 6] = [
        Color32::from_rgb(75, 116, 98),
        Color32::from_rgb(166, 107, 55),
        Color32::from_rgb(97, 84, 148),
        Color32::from_rgb(62, 114, 139),
        Color32::from_rgb(145, 83, 105),
        Color32::from_rgb(115, 119, 73),
    ];
    let value = note.tags.first().map(String::as_str).unwrap_or(&note.title);
    let hash = value.bytes().fold(0_usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    });
    PALETTE[hash % PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui::{Pos2, Rect, Vec2};

    use super::{Camera, DetailLevel, detail_level, font_sizes, grid_layout, resolved_edges};
    use crate::vault::{NoteId, NoteRecord, VaultIndex};

    fn note(name: &str) -> NoteRecord {
        let path = PathBuf::from(format!("{name}.md"));
        NoteRecord {
            id: NoteId(path.clone()),
            relative_path: path,
            title: name.to_owned(),
            aliases: Vec::new(),
            tags: Vec::new(),
            references: Vec::new(),
            backlinks: Vec::new(),
            citations: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn zoom_keeps_the_pointer_world_position_fixed() {
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0));
        let pointer = Pos2::new(730.0, 240.0);
        let mut camera = Camera::default();
        let before = camera.screen_to_world(pointer, viewport);
        camera.zoom_at(pointer, 1.8, viewport);
        let after = camera.screen_to_world(pointer, viewport);
        assert!((before - after).length() < 0.001);
    }

    #[test]
    fn screen_pan_is_scaled_into_world_space() {
        let mut camera = Camera {
            center: Pos2::new(100.0, 100.0),
            scale: 2.0,
        };
        camera.pan_by_screen_delta(Vec2::new(20.0, -10.0));
        assert_eq!(camera.center, Pos2::new(90.0, 105.0));
    }

    #[test]
    fn consecutive_drag_frames_accumulate() {
        let mut camera = Camera::default();
        camera.pan_by_screen_delta(Vec2::new(12.0, 4.0));
        camera.pan_by_screen_delta(Vec2::new(8.0, -6.0));
        assert_eq!(camera.center, Pos2::new(-20.0, 2.0));
    }

    #[test]
    fn grid_layout_is_deterministic_and_non_overlapping() {
        let notes = [note("A"), note("B"), note("C"), note("D")];
        let first = grid_layout(&notes);
        let second = grid_layout(&notes);
        assert_eq!(first, second);
        let values: Vec<_> = first.values().collect();
        for (index, left) in values.iter().enumerate() {
            for right in values.iter().skip(index + 1) {
                assert!((**left - **right).length() >= 168.0);
            }
        }
    }

    #[test]
    fn detail_levels_follow_scale_thresholds() {
        assert_eq!(detail_level(0.1), DetailLevel::Markers);
        assert_eq!(detail_level(0.4), DetailLevel::Titles);
        assert_eq!(detail_level(1.0), DetailLevel::Metadata);
    }

    #[test]
    fn font_sizes_use_stable_zoom_buckets() {
        assert_eq!(
            font_sizes(DetailLevel::Titles, 0.3),
            font_sizes(DetailLevel::Titles, 0.7)
        );
        assert_eq!(
            font_sizes(DetailLevel::Metadata, 0.8),
            font_sizes(DetailLevel::Metadata, 1.3)
        );
        assert_ne!(
            font_sizes(DetailLevel::Metadata, 1.3),
            font_sizes(DetailLevel::Metadata, 1.4)
        );
    }

    #[test]
    fn resolved_edge_geometry_is_cached_from_note_ids() {
        let mut source = note("Source");
        let target = note("Target");
        source.references.push(target.id.clone());
        let notes = vec![source, target];
        let positions = grid_layout(&notes);
        let index = VaultIndex {
            root: PathBuf::from("/vault"),
            notes,
            diagnostics: Vec::new(),
            scan_duration: std::time::Duration::ZERO,
        };
        let edges = resolved_edges(&index, &positions);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, positions[&NoteId(PathBuf::from("Source.md"))]);
        assert_eq!(edges[0].1, positions[&NoteId(PathBuf::from("Target.md"))]);
    }
}
