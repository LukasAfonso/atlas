use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use eframe::egui::{self, Pos2, Rect, Sense, Vec2};

mod inspector;
mod layout;
mod metaballs;
mod placement;
mod render;

use crate::markdown::{CARD_BODY_FONT_WORLD, MarkdownCache};
use crate::theme;
use crate::vault::{NoteId, NoteRecord, VaultIndex};
use inspector::paint_relationship_panel;
pub(crate) use layout::BoardLayout;
#[cfg(test)]
use layout::resolved_edges;
use layout::{BoardEdge, ClusterRegion, LayoutSeed, LayoutStats, PlacementFingerprint};
pub(crate) use placement::prepare_board_layout;
use render::{NotePaintOptions, note_padding};

#[cfg(test)]
fn clustered_layout(index: &VaultIndex) -> BoardLayout {
    prepare_board_layout(index, None)
}
#[cfg(test)]
use render::{
    READABLE_TITLE_FONT_SIZE, font_sizes, note_body_rect, note_footer_visible, note_title_position,
};

const CARD_SIZE: Vec2 = Vec2::new(230.0, 112.0);
const GRID_SPACING: Vec2 = Vec2::new(470.0, 195.0);
const MIN_SCALE: f32 = 0.08;
const MAX_SCALE: f32 = 32.0;
const MARKER_THRESHOLD: f32 = 0.28;
const TITLE_THRESHOLD: f32 = 0.74;
const FADE_START_THRESHOLD: f32 = 0.55;
const ISOLATION_THRESHOLD: f32 = 0.75;
const SNAP_MAGNET_THRESHOLD: f32 = 0.85;
const SNAP_THRESHOLD: f32 = 1.0;
const SNAP_EXIT_SCALE: f32 = 0.85;
const RELATIONSHIP_PANEL_WIDTH: f32 = 280.0;
const MAX_CLICK_DISTANCE: f32 = 6.0;
const NOTE_HEADER_RESERVED_HEIGHT: f32 = 46.0;
const NOTE_PADDING_WORLD: f32 = 14.0;
const BODY_PREVIEW_START: f32 = 0.18;
const SNAPPED_TYPOGRAPHY_SCALE: f32 = theme::BODY_FONT_SIZE / CARD_BODY_FONT_WORLD;

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
    positions: HashMap<NoteId, Pos2>,
    clusters: Vec<ClusterRegion>,
    edges: Vec<BoardEdge>,
    content_bounds: Option<Rect>,
    needs_fit: bool,
    click_origin: Option<Pos2>,
    snapped_note: Option<NoteId>,
    snapped_scroll: f32,
    relationship_panel_open: bool,
    markdown_cache: MarkdownCache,
    layout_root: Option<PathBuf>,
    fingerprints: HashMap<NoteId, PlacementFingerprint>,
    layout_stats: LayoutStats,
    layout_duration: Duration,
    last_viewport: Option<Rect>,
    pub visible_notes: usize,
}

impl Default for BoardState {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            positions: HashMap::new(),
            clusters: Vec::new(),
            edges: Vec::new(),
            content_bounds: None,
            needs_fit: true,
            click_origin: None,
            snapped_note: None,
            snapped_scroll: 0.0,
            relationship_panel_open: false,
            markdown_cache: MarkdownCache::default(),
            layout_root: None,
            fingerprints: HashMap::new(),
            layout_stats: LayoutStats::default(),
            layout_duration: Duration::ZERO,
            last_viewport: None,
            visible_notes: 0,
        }
    }
}

#[derive(Debug, Default)]
pub struct BoardOutput {
    pub selection_request: Option<Option<NoteId>>,
}

impl BoardState {
    pub(crate) fn layout_seed(&self, root: &Path) -> Option<LayoutSeed> {
        (self.layout_root.as_deref() == Some(root)).then(|| LayoutSeed {
            root: root.to_path_buf(),
            positions: self.positions.clone(),
            fingerprints: self.fingerprints.clone(),
        })
    }

    pub(crate) fn install_layout(
        &mut self,
        root: PathBuf,
        layout: BoardLayout,
        preserve_view: bool,
        layout_duration: Duration,
    ) {
        let visible_world = preserve_view.then(|| {
            self.last_viewport.map(|viewport| {
                Rect::from_two_pos(
                    self.camera.screen_to_world(viewport.min, viewport),
                    self.camera.screen_to_world(viewport.max, viewport),
                )
            })
        });
        let BoardLayout {
            positions,
            clusters,
            edges,
            stats,
            fingerprints,
            content_bounds,
        } = layout;
        let retired = (
            std::mem::replace(&mut self.positions, positions),
            std::mem::replace(&mut self.clusters, clusters),
            std::mem::replace(&mut self.edges, edges),
            std::mem::replace(&mut self.fingerprints, fingerprints),
            std::mem::take(&mut self.markdown_cache),
        );
        self.content_bounds = content_bounds;
        self.layout_stats = stats;
        self.layout_duration = layout_duration;
        self.layout_root = Some(root);
        self.click_origin = None;
        self.snapped_note = None;
        self.snapped_scroll = 0.0;
        self.relationship_panel_open = false;
        self.visible_notes = 0;
        if !retired.0.is_empty()
            || !retired.1.is_empty()
            || !retired.2.is_empty()
            || !retired.3.is_empty()
        {
            std::thread::spawn(move || drop(retired));
        }

        if preserve_view {
            self.needs_fit = match (visible_world.flatten(), self.content_bounds) {
                (Some(visible), Some(content)) => !visible.intersects(content),
                (None, Some(_)) => true,
                (_, None) => false,
            };
        } else {
            self.camera = Camera::default();
            self.needs_fit = true;
            self.last_viewport = None;
        }
    }

    pub(crate) fn layout_duration(&self) -> Duration {
        self.layout_duration
    }

    pub fn request_fit(&mut self) {
        self.snapped_note = None;
        self.snapped_scroll = 0.0;
        self.relationship_panel_open = false;
        self.needs_fit = true;
    }

    pub fn select_note(&mut self, note_id: Option<&NoteId>) {
        let Some(note_id) = note_id else {
            self.relationship_panel_open = false;
            return;
        };
        let selecting_current_snap = self.snapped_note.as_ref() == Some(note_id);
        if !selecting_current_snap && let Some(position) = self.positions.get(note_id).copied() {
            self.camera.center = position;
        }
        if self.snapped_note.is_some() {
            self.snapped_note = Some(note_id.clone());
            self.snapped_scroll = 0.0;
        } else {
            self.relationship_panel_open = true;
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
        self.last_viewport = Some(viewport);

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
        let zoom_progress = card_width_ratio(self.camera.scale, viewport).clamp(0.0, 1.0);
        let show_body = body_visible(zoom_progress);
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

        painter.rect_filled(viewport, 0.0, theme::CANVAS);
        self.paint_grid(&painter, viewport);
        self.paint_cluster_regions(&painter, viewport);
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
            if focal_state
                .as_ref()
                .is_some_and(|(focal, focal_rect, _, _, _)| {
                    focal.id != note.id && focal_rect.intersects(natural_rect)
                })
            {
                continue;
            }
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

        let mut snapped_relationship_counts = None;
        for (note, rect, _) in &visible {
            let opacity = if focal_state.is_some() && !related.contains(&note.id) {
                unrelated_opacity
            } else {
                1.0
            };
            let is_snapped_note = self.snapped_note.as_ref() == Some(&note.id);
            let typography_scale = note_typography_scale(self.camera.scale, viewport);
            let horizontal_padding = note_padding(level, self.camera.scale).x;
            let body_typography_scale =
                body_typography_scale(rect.width(), horizontal_padding, viewport);
            let relationship_counts = self.paint_note(
                &painter,
                note,
                *rect,
                level,
                NotePaintOptions {
                    selected: selected_note == Some(&note.id),
                    opacity,
                    snapped: is_snapped_note,
                    body_scroll: if is_snapped_note {
                        self.snapped_scroll
                    } else {
                        0.0
                    },
                    typography_scale,
                    body_typography_scale,
                    show_body,
                },
            );
            if is_snapped_note {
                snapped_relationship_counts =
                    relationship_counts.map(|rect| (rect, note.id.clone()));
            }
        }

        if snapped_relationship_counts
            .as_ref()
            .is_some_and(|(rect, _)| {
                pointer_position.is_some_and(|pointer| rect.expand(6.0).contains(pointer))
            })
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let relationship_panel = if self.relationship_panel_open {
            selected_state
                .as_ref()
                .filter(|(note, _)| {
                    self.snapped_note
                        .as_ref()
                        .is_none_or(|snapped| snapped == &note.id)
                })
                .map(|(note, _)| paint_relationship_panel(&painter, viewport, index, note))
        } else {
            None
        };

        let mut output = BoardOutput::default();
        if let Some(pointer) = clicked_at {
            if let Some(target) = relationship_panel
                .as_ref()
                .and_then(|panel| panel.navigation_at(pointer))
            {
                output.selection_request = Some(Some(target));
            } else if relationship_panel
                .as_ref()
                .is_some_and(|panel| panel.bounds.contains(pointer))
            {
                // Non-navigable relationship content keeps the current selection without
                // starting a separate interaction surface.
            } else if let Some((_, note_id)) = snapped_relationship_counts
                .as_ref()
                .filter(|(rect, _)| rect.expand(6.0).contains(pointer))
            {
                self.relationship_panel_open = true;
                if selected_note != Some(note_id) {
                    output.selection_request = Some(Some(note_id.clone()));
                }
                ui.ctx().request_repaint();
            } else {
                self.relationship_panel_open = false;
                if self.snapped_note.is_none() {
                    let hit = hit_test(pointer, &visible);
                    output.selection_request = Some(toggled_selection(hit, selected_note));
                }
            }
        }

        if !exited_snap
            && self.snapped_note.is_none()
            && let Some((note, _, _, _, width_ratio)) = focal_state.as_ref()
            && *width_ratio >= SNAP_THRESHOLD
        {
            self.snapped_note = Some(note.id.clone());
            self.snapped_scroll = 0.0;
            self.relationship_panel_open = false;
            ui.ctx().request_repaint();
        }

        output
    }

    fn exit_snapped(&mut self, viewport_width: f32) {
        self.snapped_note = None;
        self.snapped_scroll = 0.0;
        self.relationship_panel_open = false;
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
            DetailLevel::Titles => {
                Rect::from_center_size(center, title_card_size(self.camera.scale))
            }
            DetailLevel::Content => {
                Rect::from_center_size(center, content_card_size(self.camera.scale))
            }
        }
    }
}

fn note_by_id<'a>(index: &'a VaultIndex, note_id: &NoteId) -> Option<&'a NoteRecord> {
    index.notes.iter().find(|note| &note.id == note_id)
}

fn toggled_selection(hit: Option<NoteId>, selected_note: Option<&NoteId>) -> Option<NoteId> {
    if hit.as_ref().is_some_and(|hit| selected_note == Some(hit)) {
        None
    } else {
        hit
    }
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

fn note_typography_scale(scale: f32, viewport: Rect) -> f32 {
    SNAPPED_TYPOGRAPHY_SCALE * card_width_ratio(scale, viewport).clamp(0.0, SNAP_THRESHOLD)
}

fn body_typography_scale(note_width: f32, horizontal_padding: f32, viewport: Rect) -> f32 {
    let body_width = (note_width - horizontal_padding * 2.0).max(0.0);
    let snapped_body_width = viewport.width()
        * ((CARD_SIZE.x - NOTE_PADDING_WORLD * 2.0) / CARD_SIZE.x).max(f32::EPSILON);
    SNAPPED_TYPOGRAPHY_SCALE * (body_width / snapped_body_width).clamp(0.0, 1.0)
}

fn title_card_size(scale: f32) -> Vec2 {
    let progress = title_lod_progress(scale);
    let width_factor = 2.0 + (1.0 - 2.0) * progress;
    let entering_height = CARD_SIZE.y * scale * 1.65;
    let content_height = content_card_size(scale).y;
    Vec2::new(
        CARD_SIZE.x * scale * width_factor,
        entering_height + (content_height - entering_height) * progress,
    )
}

fn title_lod_progress(scale: f32) -> f32 {
    ((scale - MARKER_THRESHOLD) / (TITLE_THRESHOLD - MARKER_THRESHOLD)).clamp(0.0, 1.0)
}

fn content_card_size(scale: f32) -> Vec2 {
    Vec2::new(
        CARD_SIZE.x * scale,
        CARD_SIZE.y * scale + NOTE_HEADER_RESERVED_HEIGHT,
    )
}

fn body_visible(zoom_progress: f32) -> bool {
    zoom_progress >= BODY_PREVIEW_START
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

pub fn detail_level(scale: f32) -> DetailLevel {
    if scale < MARKER_THRESHOLD {
        DetailLevel::Markers
    } else if scale < TITLE_THRESHOLD {
        DetailLevel::Titles
    } else {
        DetailLevel::Content
    }
}

fn hit_test(pointer: Pos2, visible: &[(&NoteRecord, Rect, Pos2)]) -> Option<NoteId> {
    visible
        .iter()
        .rev()
        .find(|(_, rect, _)| rect.expand(3.0).contains(pointer))
        .map(|(note, _, _)| note.id.clone())
}

#[cfg(test)]
mod tests;
