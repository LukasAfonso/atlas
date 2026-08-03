use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::markdown::{CARD_BODY_WIDTH_WORLD, MarkdownCache};
use crate::vault::{NoteId, NoteRecord, VaultIndex};

const CARD_SIZE: Vec2 = Vec2::new(230.0, 112.0);
const GRID_SPACING: Vec2 = Vec2::new(284.0, 168.0);
const MIN_SCALE: f32 = 0.08;
const MAX_SCALE: f32 = 32.0;
const MARKER_THRESHOLD: f32 = 0.28;
const TITLE_THRESHOLD: f32 = 0.74;
const FADE_START_THRESHOLD: f32 = 0.55;
const ISOLATION_THRESHOLD: f32 = 0.75;
const SNAP_MAGNET_THRESHOLD: f32 = 0.85;
const SNAP_THRESHOLD: f32 = 1.0;
const SNAP_EXIT_SCALE: f32 = 0.85;
const RELATIONSHIP_TRAY_HEIGHT: f32 = 132.0;
const MAX_CLICK_DISTANCE: f32 = 6.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetailLevel {
    Markers,
    Titles,
    Content,
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
    edges: Vec<BoardEdge>,
    content_bounds: Option<Rect>,
    needs_fit: bool,
    click_origin: Option<Pos2>,
    snapped_note: Option<NoteId>,
    snapped_scroll: f32,
    markdown_cache: MarkdownCache,
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
            click_origin: None,
            snapped_note: None,
            snapped_scroll: 0.0,
            markdown_cache: MarkdownCache::default(),
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
        self.click_origin = None;
        self.snapped_note = None;
        self.snapped_scroll = 0.0;
        self.markdown_cache.clear();
        self.visible_notes = 0;
    }

    pub fn request_fit(&mut self) {
        self.snapped_note = None;
        self.snapped_scroll = 0.0;
        self.needs_fit = true;
    }

    pub fn select_note(&mut self, note_id: Option<&NoteId>) {
        let Some(note_id) = note_id else {
            return;
        };
        let selecting_current_snap = self.snapped_note.as_ref() == Some(note_id);
        if !selecting_current_snap && let Some(position) = self.positions.get(note_id).copied() {
            self.camera.center = position;
        }
        if self.snapped_note.is_some() {
            self.snapped_note = Some(note_id.clone());
            self.snapped_scroll = 0.0;
        }
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

        let was_snapped = self.snapped_note.is_some();
        let mut exited_snap = false;
        let pointer_position = ui.input(|input| input.pointer.latest_pos());
        let pointer_inside = pointer_position.is_some_and(|pointer| viewport.contains(pointer));
        let zoom_delta = ui.input(|input| input.zoom_delta());
        let zoom_focus = self.zoom_focus(index, viewport);
        if was_snapped && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.exit_snapped(viewport.width());
            exited_snap = true;
        } else if (pointer_inside || was_snapped) && (zoom_delta - 1.0).abs() > f32::EPSILON {
            if was_snapped && zoom_delta < 1.0 {
                self.snapped_note = None;
                self.snapped_scroll = 0.0;
                exited_snap = true;
            }
            if !was_snapped || exited_snap {
                let progress_before = snap_magnet_progress(self.camera.scale, viewport);
                let snap_scale = viewport.width() * SNAP_THRESHOLD / CARD_SIZE.x;
                let magnet_scale = viewport.width() * SNAP_MAGNET_THRESHOLD / CARD_SIZE.x;
                let current_scale = self.camera.scale;
                let target_scale = if zoom_delta > 1.0 {
                    (current_scale * zoom_delta).min(snap_scale)
                } else {
                    current_scale * zoom_delta
                };
                let pointer_anchor = pointer_position.unwrap_or(viewport.center());

                if let Some((_, position)) = zoom_focus.as_ref() {
                    if current_scale < magnet_scale && target_scale > magnet_scale {
                        self.camera
                            .zoom_at(pointer_anchor, magnet_scale / current_scale, viewport);
                        let note_anchor = self.camera.world_to_screen(*position, viewport);
                        self.camera
                            .zoom_at(note_anchor, target_scale / magnet_scale, viewport);
                    } else {
                        let anchor = if current_scale >= magnet_scale || was_snapped {
                            self.camera.world_to_screen(*position, viewport)
                        } else {
                            pointer_anchor
                        };
                        self.camera
                            .zoom_at(anchor, target_scale / current_scale, viewport);
                    }
                } else {
                    self.camera
                        .zoom_at(pointer_anchor, target_scale / current_scale, viewport);
                }

                if let Some((_, position)) = zoom_focus.as_ref() {
                    let progress_after = snap_magnet_progress(self.camera.scale, viewport);
                    if progress_after > progress_before {
                        let pull = ((progress_after - progress_before)
                            / (1.0 - progress_before).max(f32::EPSILON))
                        .clamp(0.0, 1.0);
                        self.camera.center = self.camera.center.lerp(*position, pull);
                    }
                }
            }
        }

        if pointer_inside {
            let scroll_delta = ui.input(|input| input.smooth_scroll_delta);
            if scroll_delta != Vec2::ZERO {
                if was_snapped && !exited_snap {
                    self.snapped_scroll = (self.snapped_scroll - scroll_delta.y).max(0.0);
                    ui.ctx().request_repaint();
                } else if !exited_snap {
                    self.camera.pan_by_screen_delta(scroll_delta);
                }
            }
        }

        if !was_snapped && response.dragged_by(egui::PointerButton::Primary) {
            self.camera
                .pan_by_screen_delta(ui.input(|input| input.pointer.delta()));
        }

        let pointer_pressed =
            ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary));
        let pointer_released =
            ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary));
        if pointer_pressed
            && let Some(pointer) = pointer_position.filter(|pointer| viewport.contains(*pointer))
        {
            self.click_origin = Some(pointer);
        }
        if let (Some(origin), Some(pointer)) = (self.click_origin, pointer_position)
            && origin.distance(pointer) > MAX_CLICK_DISTANCE
        {
            self.click_origin = None;
        }
        let clicked_at = pointer_released
            .then(|| self.click_origin.take())
            .flatten()
            .zip(pointer_position)
            .filter(|(origin, pointer)| {
                origin.distance(*pointer) <= MAX_CLICK_DISTANCE && viewport.contains(*pointer)
            })
            .map(|(_, pointer)| pointer);

        let level = self.detail_level();
        let selected_state = selected_note.and_then(|selected_id| {
            let note = note_by_id(index, selected_id)?;
            let position = self.positions.get(selected_id).copied()?;
            let rect = self.note_screen_rect(position, viewport, level);
            Some((note, rect))
        });
        let focal_state = index
            .notes
            .iter()
            .filter_map(|note| {
                let position = self.positions.get(&note.id).copied()?;
                let natural_rect = self.note_screen_rect(position, viewport, level);
                let rect = magnetized_note_rect(
                    natural_rect,
                    viewport,
                    snap_magnet_progress(self.camera.scale, viewport),
                );
                viewport.intersects(natural_rect).then_some((
                    note,
                    rect,
                    position.distance(self.camera.center),
                    note_viewport_coverage(self.camera.scale, viewport),
                    card_width_ratio(self.camera.scale, viewport),
                ))
            })
            .min_by(|left, right| left.2.total_cmp(&right.2));
        let isolated = focal_state
            .as_ref()
            .is_some_and(|(_, _, _, coverage, _)| *coverage >= ISOLATION_THRESHOLD);
        let related = focal_state
            .as_ref()
            .map_or_else(HashSet::new, |(note, _, _, _, _)| {
                if selected_note == Some(&note.id) {
                    related_note_ids(note)
                } else {
                    std::iter::once(note.id.clone()).collect()
                }
            });
        let unrelated_opacity = focal_state
            .as_ref()
            .map_or(1.0, |(_, _, _, coverage, _)| unrelated_opacity(*coverage));

        painter.rect_filled(viewport, 0.0, Color32::from_rgb(244, 245, 240));
        self.paint_grid(&painter, viewport);
        self.paint_edges(
            &painter,
            viewport,
            focal_state.as_ref().map(|(note, _, _, _, _)| &note.id),
            isolated,
            &related,
            unrelated_opacity,
        );

        let mut visible = Vec::new();
        for note in &index.notes {
            if isolated && !related.contains(&note.id) {
                continue;
            }
            let Some(world_position) = self.positions.get(&note.id).copied() else {
                continue;
            };
            let natural_rect = self.note_screen_rect(world_position, viewport, level);
            let rect = if focal_state
                .as_ref()
                .is_some_and(|(focal, _, _, _, _)| focal.id == note.id)
            {
                magnetized_note_rect(
                    natural_rect,
                    viewport,
                    snap_magnet_progress(self.camera.scale, viewport),
                )
            } else {
                natural_rect
            };
            if viewport.intersects(rect) {
                visible.push((note, rect, world_position));
            }
        }
        self.visible_notes = visible.len();

        for (note, rect, _) in &visible {
            let opacity = if focal_state.is_some() && !related.contains(&note.id) {
                unrelated_opacity
            } else {
                1.0
            };
            let is_snapped_note = self.snapped_note.as_ref() == Some(&note.id);
            self.paint_note(
                &painter,
                note,
                *rect,
                level,
                NotePaintOptions {
                    selected: selected_note == Some(&note.id),
                    opacity,
                    full_body: is_snapped_note,
                    body_scroll: if is_snapped_note {
                        self.snapped_scroll
                    } else {
                        0.0
                    },
                },
            );
        }

        let edit_rect = focal_state
            .as_ref()
            .filter(|(_, _, _, coverage, _)| *coverage >= ISOLATION_THRESHOLD)
            .map(|(_, rect, _, _, _)| paint_inactive_edit_button(&painter, *rect));
        let relationship_tray = selected_state
            .as_ref()
            .map(|(note, _)| paint_relationship_tray(&painter, viewport, index, note));

        let mut output = BoardOutput::default();
        if let Some(pointer) = clicked_at {
            if let Some(target) = relationship_tray
                .as_ref()
                .and_then(|tray| tray.navigation_at(pointer))
            {
                output.selection_request = Some(Some(target));
            } else if edit_rect.is_some_and(|rect| rect.contains(pointer))
                || relationship_tray
                    .as_ref()
                    .is_some_and(|tray| tray.bounds.contains(pointer))
            {
                // The inactive Edit button and non-navigable relationship content keep the
                // current selection without starting a separate interaction surface.
            } else {
                let hit = hit_test(pointer, &visible);
                output.selection_request = Some(hit);
            }
        }

        if !exited_snap
            && self.snapped_note.is_none()
            && let Some((note, _, _, _, width_ratio)) = focal_state.as_ref()
            && *width_ratio >= SNAP_THRESHOLD
        {
            self.snapped_note = Some(note.id.clone());
            self.snapped_scroll = 0.0;
            ui.ctx().request_repaint();
        }

        if self.debug_open {
            let frame_ms = ui.input(|input| input.stable_dt * 1_000.0);
            painter.text(
                viewport.left_top() + Vec2::new(10.0, 10.0),
                Align2::LEFT_TOP,
                format!(
                    "{frame_ms:.1} ms · {} / {} visible · {:.2}× · {:?} · {:.0}% width",
                    self.visible_notes,
                    index.notes.len(),
                    self.camera.scale,
                    level,
                    card_width_ratio(self.camera.scale, viewport) * 100.0,
                ),
                FontId::monospace(11.0),
                Color32::from_rgb(77, 83, 75),
            );
        }

        output
    }

    fn exit_snapped(&mut self, viewport_width: f32) {
        self.snapped_note = None;
        self.snapped_scroll = 0.0;
        let return_scale = viewport_width * SNAP_EXIT_SCALE / CARD_SIZE.x;
        self.camera.scale = self
            .camera
            .scale
            .min(return_scale)
            .clamp(MIN_SCALE, MAX_SCALE);
    }

    fn zoom_focus(&self, index: &VaultIndex, viewport: Rect) -> Option<(NoteId, Pos2)> {
        if let Some(snapped) = self.snapped_note.as_ref()
            && let Some(position) = self.positions.get(snapped).copied()
        {
            return Some((snapped.clone(), position));
        }

        let level = self.detail_level();
        index
            .notes
            .iter()
            .filter_map(|note| {
                let position = self.positions.get(&note.id).copied()?;
                let rect = self.note_screen_rect(position, viewport, level);
                viewport
                    .intersects(rect)
                    .then_some((note.id.clone(), position))
            })
            .min_by(|left, right| {
                left.1
                    .distance(self.camera.center)
                    .total_cmp(&right.1.distance(self.camera.center))
            })
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
            DetailLevel::Titles | DetailLevel::Content => {
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

    fn paint_edges(
        &self,
        painter: &egui::Painter,
        viewport: Rect,
        focus_note: Option<&NoteId>,
        isolated: bool,
        related: &HashSet<NoteId>,
        unrelated_opacity: f32,
    ) {
        let color = Color32::from_rgba_unmultiplied(71, 104, 88, 46);
        for edge in &self.edges {
            if isolated
                && !focus_note.is_some_and(|selected| {
                    (&edge.source == selected && related.contains(&edge.target))
                        || (&edge.target == selected && related.contains(&edge.source))
                })
            {
                continue;
            }
            let source = edge.source_position;
            let target = edge.target_position;
            let source = self.camera.world_to_screen(source, viewport);
            let target = self.camera.world_to_screen(target, viewport);
            if viewport.intersects(Rect::from_two_pos(source, target).expand(2.0)) {
                let incident_to_selection = focus_note
                    .is_some_and(|selected| &edge.source == selected || &edge.target == selected);
                let mut edge_painter = painter.clone();
                if focus_note.is_some() && !incident_to_selection {
                    edge_painter.set_opacity(unrelated_opacity);
                }
                edge_painter.line_segment([source, target], Stroke::new(1.0, color));
            }
        }
    }

    fn paint_note(
        &mut self,
        painter: &egui::Painter,
        note: &NoteRecord,
        rect: Rect,
        level: DetailLevel,
        options: NotePaintOptions,
    ) {
        let mut painter = painter.clone();
        painter.set_opacity(options.opacity);
        let accent = note_color(note);
        if level == DetailLevel::Markers {
            painter.circle_filled(
                rect.center(),
                if options.selected { 6.5 } else { 4.5 },
                accent,
            );
            if options.selected {
                painter.circle_stroke(rect.center(), 8.0, Stroke::new(2.0, Color32::WHITE));
            }
            return;
        }

        let fill = if options.selected {
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
                if options.selected { 2.0 } else { 1.0 },
                if options.selected {
                    Color32::from_rgb(65, 104, 86)
                } else {
                    Color32::from_rgba_unmultiplied(72, 79, 69, 42)
                },
            ),
            StrokeKind::Inside,
        );

        let padding = if level == DetailLevel::Content {
            14.0 * self.camera.scale
        } else {
            (14.0 * self.camera.scale).clamp(8.0, 18.0)
        };
        let font_sizes = font_sizes(level, self.camera.scale);
        let content_painter = painter.with_clip_rect(rect.shrink(padding.min(rect.width() / 4.0)));
        content_painter.text(
            rect.left_top() + Vec2::new(padding, padding),
            Align2::LEFT_TOP,
            &note.title,
            FontId::proportional(font_sizes.title),
            Color32::from_rgb(35, 39, 34),
        );

        if level == DetailLevel::Content {
            let body_top = rect.top() + 38.0 * self.camera.scale;
            let body_bottom = rect.bottom() - 30.0 * self.camera.scale;
            if body_bottom > body_top {
                let body_rect = Rect::from_min_max(
                    Pos2::new(rect.left() + padding, body_top),
                    Pos2::new(
                        rect.left() + padding + CARD_BODY_WIDTH_WORLD * self.camera.scale,
                        body_bottom,
                    ),
                );
                let galley = self.markdown_cache.card_galley(
                    &painter,
                    note,
                    self.camera.scale,
                    options.full_body,
                    painter.ctx().pixels_per_point(),
                );
                let body_scroll = options
                    .body_scroll
                    .min((galley.size().y - body_rect.height()).max(0.0));
                if options.full_body {
                    self.snapped_scroll = body_scroll;
                }
                content_painter.with_clip_rect(body_rect).galley(
                    body_rect.min - Vec2::new(0.0, body_scroll),
                    galley,
                    Color32::from_rgb(45, 49, 43),
                );
            }

            let tag_text = note
                .tags
                .iter()
                .take(3)
                .map(|tag| format!("#{tag}"))
                .collect::<Vec<_>>()
                .join("  ");
            if !tag_text.is_empty() {
                content_painter.text(
                    rect.left_bottom() + Vec2::new(padding, -padding - 4.0 * self.camera.scale),
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

#[derive(Debug)]
struct RelationshipTray {
    bounds: Rect,
    navigation: Vec<(Rect, NoteId)>,
}

impl RelationshipTray {
    fn navigation_at(&self, pointer: Pos2) -> Option<NoteId> {
        self.navigation
            .iter()
            .find(|(rect, _)| rect.contains(pointer))
            .map(|(_, note_id)| note_id.clone())
    }
}

fn paint_inactive_edit_button(painter: &egui::Painter, note_rect: Rect) -> Rect {
    let rect = Rect::from_min_size(
        note_rect.right_top() + Vec2::new(-72.0, 14.0),
        Vec2::new(56.0, 25.0),
    );
    painter.rect_filled(rect, 5.0, Color32::from_rgb(232, 232, 226));
    painter.rect_stroke(
        rect,
        5.0,
        Stroke::new(1.0, Color32::from_rgb(196, 199, 191)),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "Edit",
        FontId::proportional(11.0),
        Color32::from_rgb(143, 147, 139),
    );
    rect
}

fn paint_relationship_tray(
    painter: &egui::Painter,
    viewport: Rect,
    index: &VaultIndex,
    note: &NoteRecord,
) -> RelationshipTray {
    let tray_height = RELATIONSHIP_TRAY_HEIGHT.min((viewport.height() - 24.0).max(72.0));
    let bounds = Rect::from_min_max(
        Pos2::new(
            viewport.left() + 12.0,
            viewport.bottom() - tray_height - 12.0,
        ),
        Pos2::new(viewport.right() - 12.0, viewport.bottom() - 12.0),
    );
    painter.rect_filled(
        bounds,
        8.0,
        Color32::from_rgba_unmultiplied(250, 249, 245, 244),
    );
    painter.rect_stroke(
        bounds,
        8.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(75, 85, 75, 52)),
        StrokeKind::Inside,
    );

    let column_width = bounds.width() / 3.0;
    let mut navigation = Vec::new();
    paint_note_relationship_column(
        painter,
        Rect::from_min_size(bounds.min, Vec2::new(column_width, bounds.height())),
        "References",
        &note.references,
        index,
        &mut navigation,
    );
    paint_note_relationship_column(
        painter,
        Rect::from_min_size(
            Pos2::new(bounds.left() + column_width, bounds.top()),
            Vec2::new(column_width, bounds.height()),
        ),
        "Backlinks",
        &note.backlinks,
        index,
        &mut navigation,
    );
    paint_citation_tray_column(
        painter,
        Rect::from_min_size(
            Pos2::new(bounds.left() + column_width * 2.0, bounds.top()),
            Vec2::new(column_width, bounds.height()),
        ),
        &note.citations,
    );

    RelationshipTray { bounds, navigation }
}

fn paint_note_relationship_column(
    painter: &egui::Painter,
    column: Rect,
    heading: &str,
    ids: &[NoteId],
    index: &VaultIndex,
    navigation: &mut Vec<(Rect, NoteId)>,
) {
    let clipped = painter.with_clip_rect(column.shrink(8.0));
    clipped.text(
        column.left_top() + Vec2::new(10.0, 9.0),
        Align2::LEFT_TOP,
        format!("{heading} · {}", ids.len()),
        FontId::proportional(10.0),
        Color32::from_rgb(103, 109, 100),
    );
    for (row, note_id) in ids.iter().take(4).enumerate() {
        let top = column.top() + 30.0 + row as f32 * 21.0;
        let rect = Rect::from_min_size(
            Pos2::new(column.left() + 8.0, top),
            Vec2::new((column.width() - 16.0).max(1.0), 18.0),
        );
        clipped.rect_filled(rect, 4.0, Color32::from_rgb(235, 239, 233));
        clipped.text(
            rect.left_center() + Vec2::new(6.0, 0.0),
            Align2::LEFT_CENTER,
            note_title(index, note_id),
            FontId::proportional(10.0),
            Color32::from_rgb(54, 91, 73),
        );
        navigation.push((rect, note_id.clone()));
    }
}

fn paint_citation_tray_column(painter: &egui::Painter, column: Rect, citations: &[String]) {
    let clipped = painter.with_clip_rect(column.shrink(8.0));
    clipped.text(
        column.left_top() + Vec2::new(10.0, 9.0),
        Align2::LEFT_TOP,
        format!("Citations · {}", citations.len()),
        FontId::proportional(10.0),
        Color32::from_rgb(103, 109, 100),
    );
    for (row, citation) in citations.iter().take(4).enumerate() {
        clipped.text(
            Pos2::new(
                column.left() + 10.0,
                column.top() + 33.0 + row as f32 * 21.0,
            ),
            Align2::LEFT_TOP,
            format!("@{citation}"),
            FontId::monospace(10.0),
            Color32::from_rgb(113, 82, 57),
        );
    }
}

fn note_by_id<'a>(index: &'a VaultIndex, note_id: &NoteId) -> Option<&'a NoteRecord> {
    index.notes.iter().find(|note| &note.id == note_id)
}

fn note_title<'a>(index: &'a VaultIndex, note_id: &NoteId) -> &'a str {
    note_by_id(index, note_id).map_or("Untitled", |note| note.title.as_str())
}

fn related_note_ids(note: &NoteRecord) -> HashSet<NoteId> {
    std::iter::once(note.id.clone())
        .chain(note.references.iter().cloned())
        .chain(note.backlinks.iter().cloned())
        .collect()
}

fn card_width_ratio(scale: f32, viewport: Rect) -> f32 {
    CARD_SIZE.x * scale / viewport.width().max(1.0)
}

fn snap_magnet_progress(scale: f32, viewport: Rect) -> f32 {
    ((card_width_ratio(scale, viewport) - SNAP_MAGNET_THRESHOLD)
        / (SNAP_THRESHOLD - SNAP_MAGNET_THRESHOLD))
        .clamp(0.0, 1.0)
}

fn magnetized_note_rect(natural: Rect, viewport: Rect, progress: f32) -> Rect {
    if progress <= 0.0 {
        return natural;
    }
    if progress >= 1.0 {
        return viewport;
    }
    Rect::from_min_max(
        Pos2::new(
            natural.left(),
            natural.top() + (viewport.top() - natural.top()) * progress,
        ),
        Pos2::new(
            natural.right(),
            natural.bottom() + (viewport.bottom() - natural.bottom()) * progress,
        ),
    )
}

fn note_viewport_coverage(scale: f32, viewport: Rect) -> f32 {
    card_width_ratio(scale, viewport).max(CARD_SIZE.y * scale / viewport.height().max(1.0))
}

fn unrelated_opacity(coverage: f32) -> f32 {
    ((ISOLATION_THRESHOLD - coverage) / (ISOLATION_THRESHOLD - FADE_START_THRESHOLD))
        .clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FontSizes {
    title: f32,
    metadata: f32,
    counts: f32,
}

#[derive(Clone, Copy, Debug)]
struct NotePaintOptions {
    selected: bool,
    opacity: f32,
    full_body: bool,
    body_scroll: f32,
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
        DetailLevel::Content => FontSizes {
            title: 6.0 * scale,
            metadata: 2.8 * scale,
            counts: 2.8 * scale,
        },
    }
}

pub fn detail_level(scale: f32) -> DetailLevel {
    if scale < MARKER_THRESHOLD {
        DetailLevel::Markers
    } else if scale < TITLE_THRESHOLD {
        DetailLevel::Titles
    } else {
        DetailLevel::Content
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

fn resolved_edges(index: &VaultIndex, positions: &HashMap<NoteId, Pos2>) -> Vec<BoardEdge> {
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
struct BoardEdge {
    source: NoteId,
    target: NoteId,
    source_position: Pos2,
    target_position: Pos2,
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

    use eframe::egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};

    use super::{
        CARD_SIZE, Camera, DetailLevel, FADE_START_THRESHOLD, ISOLATION_THRESHOLD,
        SNAP_MAGNET_THRESHOLD, SNAP_THRESHOLD, card_width_ratio, detail_level, font_sizes,
        grid_layout, magnetized_note_rect, note_viewport_coverage, related_note_ids,
        resolved_edges, snap_magnet_progress, unrelated_opacity,
    };
    use crate::vault::{NoteId, NoteRecord, VaultIndex};

    fn note(name: &str) -> NoteRecord {
        let path = PathBuf::from(format!("{name}.md"));
        NoteRecord {
            id: NoteId(path.clone()),
            relative_path: path,
            title: name.to_owned(),
            markdown_body: format!("# {name}\n\nBody for {name}."),
            aliases: Vec::new(),
            tags: Vec::new(),
            references: Vec::new(),
            backlinks: Vec::new(),
            citations: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn pointer_input(screen: Rect, position: Pos2, pressed: bool) -> RawInput {
        RawInput {
            screen_rect: Some(screen),
            events: vec![
                Event::PointerMoved(position),
                Event::PointerButton {
                    pos: position,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                },
            ],
            ..RawInput::default()
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
        assert_eq!(detail_level(1.0), DetailLevel::Content);
    }

    #[test]
    fn content_chrome_scales_proportionally_with_the_card() {
        assert_eq!(
            font_sizes(DetailLevel::Titles, 0.3),
            font_sizes(DetailLevel::Titles, 0.7)
        );
        let at_one = font_sizes(DetailLevel::Content, 1.0);
        let at_two = font_sizes(DetailLevel::Content, 2.0);
        assert_eq!(at_two.title, at_one.title * 2.0);
        assert_eq!(at_two.metadata, at_one.metadata * 2.0);
        assert_eq!(at_two.counts, at_one.counts * 2.0);
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
        assert_eq!(
            edges[0].source_position,
            positions[&NoteId(PathBuf::from("Source.md"))]
        );
        assert_eq!(
            edges[0].target_position,
            positions[&NoteId(PathBuf::from("Target.md"))]
        );
    }

    #[test]
    fn selected_note_width_controls_isolation_and_snap_thresholds() {
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
        let isolated_scale = viewport.width() * ISOLATION_THRESHOLD / CARD_SIZE.x;
        let snapped_scale = viewport.width() * SNAP_THRESHOLD / CARD_SIZE.x;
        assert_eq!(
            card_width_ratio(isolated_scale, viewport),
            ISOLATION_THRESHOLD
        );
        assert_eq!(card_width_ratio(snapped_scale, viewport), SNAP_THRESHOLD);
    }

    #[test]
    fn magnetic_phase_expands_the_note_to_the_exact_viewport() {
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
        let natural = Rect::from_center_size(viewport.center(), Vec2::new(850.0, 414.0));
        let magnet_scale = viewport.width() * SNAP_MAGNET_THRESHOLD / CARD_SIZE.x;
        let snap_scale = viewport.width() * SNAP_THRESHOLD / CARD_SIZE.x;

        assert!(snap_magnet_progress(magnet_scale, viewport).abs() < 0.001);
        assert!((snap_magnet_progress(snap_scale, viewport) - 1.0).abs() < 0.001);
        assert_eq!(magnetized_note_rect(natural, viewport, 1.0), viewport);
    }

    #[test]
    fn note_coverage_uses_whichever_viewport_dimension_is_reached_first() {
        let wide_viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_600.0, 600.0));
        let scale = wide_viewport.height() * ISOLATION_THRESHOLD / CARD_SIZE.y;
        assert_eq!(
            note_viewport_coverage(scale, wide_viewport),
            ISOLATION_THRESHOLD
        );
        assert!(card_width_ratio(scale, wide_viewport) < ISOLATION_THRESHOLD);
    }

    #[test]
    fn unrelated_notes_fade_before_becoming_hidden() {
        assert_eq!(unrelated_opacity(FADE_START_THRESHOLD), 1.0);
        assert!((unrelated_opacity(0.65) - 0.5).abs() < 0.001);
        assert_eq!(unrelated_opacity(ISOLATION_THRESHOLD), 0.0);
    }

    #[test]
    fn isolation_keeps_only_direct_note_relationships() {
        let mut selected = note("Selected");
        selected
            .references
            .push(NoteId(PathBuf::from("Reference.md")));
        selected
            .backlinks
            .push(NoteId(PathBuf::from("Backlink.md")));
        let related = related_note_ids(&selected);
        assert!(related.contains(&selected.id));
        assert!(related.contains(&NoteId(PathBuf::from("Reference.md"))));
        assert!(related.contains(&NoteId(PathBuf::from("Backlink.md"))));
        assert!(!related.contains(&NoteId(PathBuf::from("Unrelated.md"))));
    }

    #[test]
    fn snapping_keeps_the_camera_transform_and_zooming_out_restores_the_board() {
        let note = note("Selected");
        let selected_id = note.id.clone();
        let index = VaultIndex {
            root: PathBuf::from("/vault"),
            notes: vec![note],
            diagnostics: Vec::new(),
            scan_duration: std::time::Duration::ZERO,
        };
        let mut board = super::BoardState::default();
        board.rebuild(&index);
        board.needs_fit = false;
        board.select_note(Some(&selected_id));
        board.camera.scale = 1_000.0 * SNAP_THRESHOLD / CARD_SIZE.x;
        let pre_snap_camera = board.camera;

        let context = eframe::egui::Context::default();
        let input = eframe::egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0))),
            ..eframe::egui::RawInput::default()
        };
        let _ = context.run_ui(input, |ui| {
            board.show(ui, &index, Some(&selected_id));
        });
        assert_eq!(board.snapped_note.as_ref(), Some(&selected_id));
        assert_eq!(board.camera.center, pre_snap_camera.center);
        assert_eq!(board.camera.scale, pre_snap_camera.scale);

        board.select_note(None);
        assert_eq!(board.snapped_note.as_ref(), Some(&selected_id));

        let _ = context.run_ui(eframe::egui::RawInput::default(), |ui| {
            board.show(ui, &index, Some(&selected_id));
        });

        let snapped_scale = board.camera.scale;
        let zoom_out = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0))),
            events: vec![Event::Zoom(0.8)],
            ..RawInput::default()
        };
        let _ = context.run_ui(zoom_out, |ui| {
            board.show(ui, &index, Some(&selected_id));
        });
        assert!(board.snapped_note.is_none());
        assert!(board.camera.scale < snapped_scale);
    }

    #[test]
    fn clicking_a_note_emits_a_selection_request() {
        let selected = note("Selected");
        let selected_id = selected.id.clone();
        let index = VaultIndex {
            root: PathBuf::from("/vault"),
            notes: vec![selected],
            diagnostics: Vec::new(),
            scan_duration: std::time::Duration::ZERO,
        };
        let mut board = super::BoardState::default();
        board.rebuild(&index);
        board.needs_fit = false;

        let context = eframe::egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
        let pointer = screen.center();
        let mut selection = None;
        let _ = context.run_ui(pointer_input(screen, pointer, true), |ui| {
            board.show(ui, &index, None);
        });
        let _ = context.run_ui(pointer_input(screen, pointer, false), |ui| {
            selection = board.show(ui, &index, None).selection_request;
        });

        assert_eq!(selection, Some(Some(selected_id)));
    }

    #[test]
    fn clicking_a_reference_in_the_tray_navigates_to_it() {
        let mut source = note("Source");
        let target = note("Target");
        let target_id = target.id.clone();
        source.references.push(target_id.clone());
        let source_id = source.id.clone();
        let index = VaultIndex {
            root: PathBuf::from("/vault"),
            notes: vec![source, target],
            diagnostics: Vec::new(),
            scan_duration: std::time::Duration::ZERO,
        };
        let mut board = super::BoardState::default();
        board.rebuild(&index);
        board.needs_fit = false;
        board.select_note(Some(&source_id));

        let context = eframe::egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
        let pointer = Pos2::new(50.0, 595.0);
        let mut selection = None;
        let _ = context.run_ui(pointer_input(screen, pointer, true), |ui| {
            board.show(ui, &index, Some(&source_id));
        });
        let _ = context.run_ui(pointer_input(screen, pointer, false), |ui| {
            selection = board.show(ui, &index, Some(&source_id)).selection_request;
        });

        assert_eq!(selection, Some(Some(target_id)));
    }

    #[test]
    fn magnetic_zoom_centers_and_snaps_a_note_without_selection() {
        let selected = note("Selected");
        let selected_id = selected.id.clone();
        let index = VaultIndex {
            root: PathBuf::from("/vault"),
            notes: vec![selected],
            diagnostics: Vec::new(),
            scan_duration: std::time::Duration::ZERO,
        };
        let mut board = super::BoardState::default();
        board.rebuild(&index);
        board.needs_fit = false;

        let context = eframe::egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
        board.camera.scale = screen.width() * SNAP_MAGNET_THRESHOLD / CARD_SIZE.x;
        board.camera.center = Pos2::new(-20.0, 0.0);
        let input = RawInput {
            screen_rect: Some(screen),
            events: vec![
                Event::PointerMoved(Pos2::new(900.0, 350.0)),
                Event::Zoom(2.0),
            ],
            ..RawInput::default()
        };
        let _ = context.run_ui(input, |ui| {
            board.show(ui, &index, None);
        });

        assert_eq!(board.snapped_note.as_ref(), Some(&selected_id));
        assert_eq!(board.camera.center, Pos2::ZERO);
        assert!((card_width_ratio(board.camera.scale, screen) - SNAP_THRESHOLD).abs() < 0.001);
    }
}
