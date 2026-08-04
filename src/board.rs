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
use inspector::{RelationshipPanel, paint_relationship_panel};
pub(crate) use layout::BoardLayout;
#[cfg(test)]
use layout::resolved_edges;
use layout::{BoardEdge, ClusterRegion, LayoutSeed, PlacementFingerprint};
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
const HOVER_PADDING: f32 = 6.0;
const HIT_PADDING: f32 = 3.0;
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

struct Pointer {
    position: Option<Pos2>,
    inside: bool,
    zoom_delta: f32,
    scroll_delta: Vec2,
    click: Option<Pos2>,
}

impl Pointer {
    fn zooming(&self) -> bool {
        (self.zoom_delta - 1.0).abs() > f32::EPSILON
    }

    fn hovers(&self, rect: Rect) -> bool {
        self.position.is_some_and(|pointer| hovers(rect, pointer))
    }
}

struct FocalNote<'a> {
    note: &'a NoteRecord,
    rect: Rect,
    distance: f32,
    coverage: f32,
    width_ratio: f32,
}

struct Focus<'a> {
    focal: Option<FocalNote<'a>>,
    related: HashSet<NoteId>,
    isolated: bool,
    unrelated_opacity: f32,
}

struct VisibleNote<'a> {
    note: &'a NoteRecord,
    rect: Rect,
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
        let visible_world = preserve_view.then(|| self.visible_world_rect()).flatten();
        let installed = Self {
            camera: self.camera,
            last_viewport: self.last_viewport,
            positions: layout.positions,
            clusters: layout.clusters,
            edges: layout.edges,
            content_bounds: layout.content_bounds,
            fingerprints: layout.fingerprints,
            layout_duration,
            layout_root: Some(root),
            ..Self::default()
        };
        let retired = std::mem::replace(self, installed);
        if !retired.positions.is_empty() {
            std::thread::spawn(move || drop(retired));
        }

        if preserve_view {
            self.needs_fit = match (visible_world, self.content_bounds) {
                (Some(visible), Some(content)) => !visible.intersects(content),
                (None, Some(_)) => true,
                (_, None) => false,
            };
        } else {
            self.camera = Camera::default();
            self.last_viewport = None;
        }
    }

    fn visible_world_rect(&self) -> Option<Rect> {
        let viewport = self.last_viewport?;
        Some(Rect::from_two_pos(
            self.camera.screen_to_world(viewport.min, viewport),
            self.camera.screen_to_world(viewport.max, viewport),
        ))
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

        let pointer = self.read_pointer(ui, viewport);
        let exited_snap = self.navigate(ui, &response, &pointer, index, viewport);

        let focus = self.focus(index, viewport, selected_note);
        let visible = self.collect_visible(index, viewport, &focus);
        self.visible_notes = visible.len();

        self.paint_background(&painter, viewport, &focus);
        let snapped_counts = self.paint_notes(&painter, viewport, &visible, &focus, selected_note);
        let panel = self.paint_relationship_panel(&painter, viewport, index, selected_note);

        if snapped_counts
            .as_ref()
            .is_some_and(|(rect, _)| pointer.hovers(*rect))
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let mut output = BoardOutput::default();
        if let Some(click) = pointer.click {
            output.selection_request = self.resolve_click(
                ui.ctx(),
                click,
                panel.as_ref(),
                snapped_counts.as_ref(),
                &visible,
                selected_note,
            );
        }

        if !exited_snap
            && self.snapped_note.is_none()
            && let Some(focal) = focus.focal.as_ref()
            && focal.width_ratio >= SNAP_THRESHOLD
        {
            self.enter_snap(focal.note.id.clone());
            ui.ctx().request_repaint();
        }

        output
    }

    fn read_pointer(&mut self, ui: &egui::Ui, viewport: Rect) -> Pointer {
        let (position, zoom_delta, scroll_delta, pressed, released) = ui.input(|input| {
            (
                input.pointer.latest_pos(),
                input.zoom_delta(),
                input.smooth_scroll_delta,
                input.pointer.button_pressed(egui::PointerButton::Primary),
                input.pointer.button_released(egui::PointerButton::Primary),
            )
        });
        let inside = position.is_some_and(|position| viewport.contains(position));

        if pressed && inside {
            self.click_origin = position;
        }
        if let (Some(origin), Some(position)) = (self.click_origin, position)
            && origin.distance(position) > MAX_CLICK_DISTANCE
        {
            self.click_origin = None;
        }
        let click = released
            .then(|| self.click_origin.take())
            .flatten()
            .and(position)
            .filter(|_| inside);

        Pointer {
            position,
            inside,
            zoom_delta,
            scroll_delta,
            click,
        }
    }

    fn navigate(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        pointer: &Pointer,
        index: &VaultIndex,
        viewport: Rect,
    ) -> bool {
        let was_snapped = self.snapped_note.is_some();
        let zoom_focus = self.zoom_focus(index, viewport);
        let mut exited_snap = false;

        if was_snapped && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.exit_snapped(viewport.width());
            exited_snap = true;
        } else if (pointer.inside || was_snapped) && pointer.zooming() {
            if was_snapped && pointer.zoom_delta < 1.0 {
                self.clear_snap();
                exited_snap = true;
            }
            if !was_snapped || exited_snap {
                self.zoom(pointer, zoom_focus, viewport, was_snapped);
            }
        }

        if pointer.inside && pointer.scroll_delta != Vec2::ZERO && !exited_snap {
            if was_snapped {
                self.snapped_scroll = (self.snapped_scroll - pointer.scroll_delta.y).max(0.0);
                ui.ctx().request_repaint();
            } else {
                self.camera.pan_by_screen_delta(pointer.scroll_delta);
            }
        }

        if !was_snapped && response.dragged_by(egui::PointerButton::Primary) {
            self.camera
                .pan_by_screen_delta(ui.input(|input| input.pointer.delta()));
        }

        exited_snap
    }

    fn zoom(
        &mut self,
        pointer: &Pointer,
        zoom_focus: Option<Pos2>,
        viewport: Rect,
        was_snapped: bool,
    ) {
        let progress_before = snap_magnet_progress(self.camera.scale, viewport);
        let snap_scale = card_ratio_scale(viewport.width(), SNAP_THRESHOLD);
        let magnet_scale = card_ratio_scale(viewport.width(), SNAP_MAGNET_THRESHOLD);
        let current_scale = self.camera.scale;
        let target_scale = if pointer.zoom_delta > 1.0 {
            (current_scale * pointer.zoom_delta).min(snap_scale)
        } else {
            current_scale * pointer.zoom_delta
        };
        let pointer_anchor = pointer.position.unwrap_or(viewport.center());

        let Some(focus) = zoom_focus else {
            self.camera
                .zoom_at(pointer_anchor, target_scale / current_scale, viewport);
            return;
        };

        if current_scale < magnet_scale && target_scale > magnet_scale {
            self.camera
                .zoom_at(pointer_anchor, magnet_scale / current_scale, viewport);
            let focus_anchor = self.camera.world_to_screen(focus, viewport);
            self.camera
                .zoom_at(focus_anchor, target_scale / magnet_scale, viewport);
        } else {
            let anchor = if current_scale >= magnet_scale || was_snapped {
                self.camera.world_to_screen(focus, viewport)
            } else {
                pointer_anchor
            };
            self.camera
                .zoom_at(anchor, target_scale / current_scale, viewport);
        }

        let progress_after = snap_magnet_progress(self.camera.scale, viewport);
        if progress_after > progress_before {
            let pull = ((progress_after - progress_before)
                / (1.0 - progress_before).max(f32::EPSILON))
            .clamp(0.0, 1.0);
            self.camera.center = self.camera.center.lerp(focus, pull);
        }
    }

    fn focus<'a>(
        &self,
        index: &'a VaultIndex,
        viewport: Rect,
        selected_note: Option<&NoteId>,
    ) -> Focus<'a> {
        let level = self.detail_level();
        let magnet_progress = snap_magnet_progress(self.camera.scale, viewport);
        let focal = index
            .notes
            .iter()
            .filter_map(|note| {
                let position = self.positions.get(&note.id).copied()?;
                let natural_rect = self.note_screen_rect(position, viewport, level);
                viewport.intersects(natural_rect).then(|| FocalNote {
                    note,
                    rect: magnetized_note_rect(natural_rect, viewport, magnet_progress),
                    distance: position.distance(self.camera.center),
                    coverage: note_viewport_coverage(self.camera.scale, viewport),
                    width_ratio: card_width_ratio(self.camera.scale, viewport),
                })
            })
            .min_by(|left, right| left.distance.total_cmp(&right.distance));

        let related = focal.as_ref().map_or_else(HashSet::new, |focal| {
            if selected_note == Some(&focal.note.id) {
                related_note_ids(focal.note)
            } else {
                std::iter::once(focal.note.id.clone()).collect()
            }
        });
        Focus {
            isolated: focal
                .as_ref()
                .is_some_and(|focal| focal.coverage >= ISOLATION_THRESHOLD),
            unrelated_opacity: focal
                .as_ref()
                .map_or(1.0, |focal| unrelated_opacity(focal.coverage)),
            focal,
            related,
        }
    }

    fn collect_visible<'a>(
        &self,
        index: &'a VaultIndex,
        viewport: Rect,
        focus: &Focus<'a>,
    ) -> Vec<VisibleNote<'a>> {
        let level = self.detail_level();
        let magnet_progress = snap_magnet_progress(self.camera.scale, viewport);
        index
            .notes
            .iter()
            .filter(|note| !focus.isolated || focus.related.contains(&note.id))
            .filter_map(|note| {
                let position = self.positions.get(&note.id).copied()?;
                let natural_rect = self.note_screen_rect(position, viewport, level);
                let rect = match focus.focal.as_ref() {
                    Some(focal) if focal.note.id == note.id => {
                        magnetized_note_rect(natural_rect, viewport, magnet_progress)
                    }
                    Some(focal) if focal.rect.intersects(natural_rect) => return None,
                    _ => natural_rect,
                };
                viewport
                    .intersects(rect)
                    .then_some(VisibleNote { note, rect })
            })
            .collect()
    }

    fn paint_background(&self, painter: &egui::Painter, viewport: Rect, focus: &Focus<'_>) {
        painter.rect_filled(viewport, 0.0, theme::CANVAS);
        self.paint_grid(painter, viewport);
        self.paint_cluster_regions(painter, viewport);
        self.paint_edges(
            painter,
            viewport,
            focus.focal.as_ref().map(|focal| &focal.note.id),
            focus.isolated,
            &focus.related,
            focus.unrelated_opacity,
        );
    }

    fn paint_notes(
        &mut self,
        painter: &egui::Painter,
        viewport: Rect,
        visible: &[VisibleNote<'_>],
        focus: &Focus<'_>,
        selected_note: Option<&NoteId>,
    ) -> Option<(Rect, NoteId)> {
        let level = self.detail_level();
        let show_body = body_visible(card_width_ratio(self.camera.scale, viewport));
        let typography_scale = note_typography_scale(self.camera.scale, viewport);
        let horizontal_padding = note_padding(level, self.camera.scale).x;
        let mut snapped_counts = None;

        for VisibleNote { note, rect } in visible {
            let snapped = self.snapped_note.as_ref() == Some(&note.id);
            let counts_rect = self.paint_note(
                painter,
                note,
                *rect,
                level,
                NotePaintOptions {
                    selected: selected_note == Some(&note.id),
                    opacity: if focus.focal.is_some() && !focus.related.contains(&note.id) {
                        focus.unrelated_opacity
                    } else {
                        1.0
                    },
                    snapped,
                    body_scroll: if snapped { self.snapped_scroll } else { 0.0 },
                    typography_scale,
                    body_typography_scale: body_typography_scale(
                        rect.width(),
                        horizontal_padding,
                        viewport,
                    ),
                    show_body,
                },
            );
            if snapped {
                snapped_counts = counts_rect.map(|rect| (rect, note.id.clone()));
            }
        }
        snapped_counts
    }

    fn paint_relationship_panel(
        &self,
        painter: &egui::Painter,
        viewport: Rect,
        index: &VaultIndex,
        selected_note: Option<&NoteId>,
    ) -> Option<RelationshipPanel> {
        if !self.relationship_panel_open {
            return None;
        }
        let note = index.note(selected_note?)?;
        self.snapped_note
            .as_ref()
            .is_none_or(|snapped| snapped == &note.id)
            .then(|| paint_relationship_panel(painter, viewport, index, note))
    }

    fn resolve_click(
        &mut self,
        context: &egui::Context,
        pointer: Pos2,
        panel: Option<&RelationshipPanel>,
        snapped_counts: Option<&(Rect, NoteId)>,
        visible: &[VisibleNote<'_>],
        selected_note: Option<&NoteId>,
    ) -> Option<Option<NoteId>> {
        if let Some(target) = panel.and_then(|panel| panel.navigation_at(pointer)) {
            return Some(Some(target));
        }
        if panel.is_some_and(|panel| panel.bounds.contains(pointer)) {
            return None;
        }
        if let Some((_, note_id)) = snapped_counts.filter(|(rect, _)| hovers(*rect, pointer)) {
            self.relationship_panel_open = true;
            context.request_repaint();
            return (selected_note != Some(note_id)).then(|| Some(note_id.clone()));
        }

        self.relationship_panel_open = false;
        if self.snapped_note.is_some() {
            return None;
        }
        Some(toggled_selection(hit_test(pointer, visible), selected_note))
    }

    fn enter_snap(&mut self, note_id: NoteId) {
        self.snapped_note = Some(note_id);
        self.snapped_scroll = 0.0;
        self.relationship_panel_open = false;
    }

    fn clear_snap(&mut self) {
        self.snapped_note = None;
        self.snapped_scroll = 0.0;
    }

    fn exit_snapped(&mut self, viewport_width: f32) {
        self.clear_snap();
        self.relationship_panel_open = false;
        let return_scale = card_ratio_scale(viewport_width, SNAP_EXIT_SCALE);
        self.camera.scale = self
            .camera
            .scale
            .min(return_scale)
            .clamp(MIN_SCALE, MAX_SCALE);
    }

    fn zoom_focus(&self, index: &VaultIndex, viewport: Rect) -> Option<Pos2> {
        if let Some(position) = self
            .snapped_note
            .as_ref()
            .and_then(|snapped| self.positions.get(snapped))
        {
            return Some(*position);
        }

        let level = self.detail_level();
        index
            .notes
            .iter()
            .filter_map(|note| {
                let position = self.positions.get(&note.id).copied()?;
                let rect = self.note_screen_rect(position, viewport, level);
                viewport.intersects(rect).then_some(position)
            })
            .min_by(|left, right| {
                left.distance(self.camera.center)
                    .total_cmp(&right.distance(self.camera.center))
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

fn hovers(rect: Rect, pointer: Pos2) -> bool {
    rect.expand(HOVER_PADDING).contains(pointer)
}

fn card_ratio_scale(viewport_width: f32, card_width_ratio: f32) -> f32 {
    viewport_width * card_width_ratio / CARD_SIZE.x
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

fn hit_test(pointer: Pos2, visible: &[VisibleNote<'_>]) -> Option<NoteId> {
    visible
        .iter()
        .rev()
        .find(|visible| visible.rect.expand(HIT_PADDING).contains(pointer))
        .map(|visible| visible.note.id.clone())
}

#[cfg(test)]
mod tests;
