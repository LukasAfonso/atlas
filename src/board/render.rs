use std::sync::Arc;

use eframe::egui::{
    Align2, Color32, FontId, Galley, Pos2, Rect, Stroke, StrokeKind, Vec2, epaint::PathShape,
};

use super::{
    BoardState, DetailLevel, Focus, NOTE_HEADER_RESERVED_HEIGHT, NOTE_PADDING_WORLD,
    title_lod_progress,
};
use crate::theme;
use crate::vault::NoteRecord;

pub(super) const READABLE_TITLE_FONT_SIZE: f32 = 14.0;
const NOTE_BADGE_SIZE: f32 = 34.0;
const NOTE_EYEBROW_FONT_SIZE: f32 = 12.0;
const NOTE_HEADER_GAP: f32 = NOTE_HEADER_RESERVED_HEIGHT - NOTE_BADGE_SIZE;
const NOTE_HEADER_MIN_HEIGHT: f32 = 120.0;
const FOOTER_RESERVE: f32 = 30.0;
const MIN_FOOTER_FONT_SIZE: f32 = 8.0;
const OUTER_SHADOW: Color32 = Color32::from_rgba_unmultiplied_const(47, 66, 53, 20);
const INNER_SHADOW: Color32 = Color32::from_rgba_unmultiplied_const(47, 66, 53, 16);

#[derive(Clone, Copy, Debug)]
pub(super) struct NotePaintOptions {
    pub(super) selected: bool,
    pub(super) opacity: f32,
    pub(super) snapped: bool,
    pub(super) body_scroll: f32,
    pub(super) typography_scale: f32,
    pub(super) body_typography_scale: f32,
    pub(super) show_body: bool,
}

impl BoardState {
    pub(super) fn paint_grid(&self, painter: &eframe::egui::Painter, viewport: Rect) {
        let spacing = (48.0 * self.camera.scale).clamp(20.0, 32.0);
        let origin = self.camera.world_to_screen(Pos2::ZERO, viewport);
        let start_x = viewport.left() + (origin.x - viewport.left()).rem_euclid(spacing);
        let start_y = viewport.top() + (origin.y - viewport.top()).rem_euclid(spacing);
        let color = Color32::from_rgba_unmultiplied(67, 94, 78, 52);
        let mut x = start_x;
        while x <= viewport.right() {
            let mut y = start_y;
            while y <= viewport.bottom() {
                painter.circle_filled(Pos2::new(x, y), 1.05, color);
                y += spacing;
            }
            x += spacing;
        }
    }

    pub(super) fn paint_cluster_regions(&self, painter: &eframe::egui::Painter, viewport: Rect) {
        self.paint_cluster_fill(painter, viewport);
        for cluster in &self.clusters {
            let bounds = Rect::from_min_max(
                self.camera.world_to_screen(cluster.bounds().min, viewport),
                self.camera.world_to_screen(cluster.bounds().max, viewport),
            );
            if !viewport.intersects(bounds) {
                continue;
            }
            let color = cluster_color(&cluster.key);
            let outline = Stroke::new(
                1.5,
                Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 92),
            );
            for contour in &cluster.geometry.contours {
                if contour.len() >= 3 {
                    painter.add(PathShape::closed_line(
                        contour
                            .iter()
                            .copied()
                            .map(|position| self.camera.world_to_screen(position, viewport))
                            .collect(),
                        outline,
                    ));
                }
            }

            let label = if cluster.name == "Untagged" {
                cluster.name.clone()
            } else {
                format!("#{}", cluster.name)
            };
            let label = painter.layout_no_wrap(
                format!("{label}  ·  {} notes", cluster.note_count),
                FontId::proportional(11.0),
                Color32::WHITE,
            );
            let natural_label = self.camera.world_to_screen(cluster.label_anchor, viewport);
            let label_position = Pos2::new(
                natural_label.x.max(viewport.left() + 12.0),
                natural_label.y.max(viewport.top() + 12.0),
            );
            let label_rect =
                Rect::from_min_size(label_position, label.size()).expand2(Vec2::new(9.0, 6.0));
            painter.rect_filled(
                label_rect,
                6.0,
                Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 238),
            );
            painter.rect_stroke(
                label_rect,
                6.0,
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 72)),
                StrokeKind::Inside,
            );
            painter.galley(label_position, label, Color32::WHITE);
        }
    }

    pub(super) fn paint_edges(
        &self,
        painter: &eframe::egui::Painter,
        viewport: Rect,
        focus: &Focus<'_>,
    ) {
        let color = Color32::from_rgba_unmultiplied(71, 104, 88, 46);
        let focus_note = focus.focal.as_ref().map(|focal| &focal.note.id);
        for edge in &self.edges {
            if focus.isolated
                && !focus_note.is_some_and(|selected| {
                    (&edge.source == selected && focus.related.contains(&edge.target))
                        || (&edge.target == selected && focus.related.contains(&edge.source))
                })
            {
                continue;
            }
            let source = self.camera.world_to_screen(edge.source_position, viewport);
            let target = self.camera.world_to_screen(edge.target_position, viewport);
            if viewport.intersects(Rect::from_two_pos(source, target).expand(2.0)) {
                let incident_to_selection = focus_note
                    .is_some_and(|selected| &edge.source == selected || &edge.target == selected);
                let mut edge_painter = painter.clone();
                if focus_note.is_some() && !incident_to_selection {
                    edge_painter.set_opacity(focus.unrelated_opacity);
                }
                edge_painter.line_segment([source, target], Stroke::new(1.0, color));
            }
        }
    }

    pub(super) fn paint_note(
        &mut self,
        painter: &eframe::egui::Painter,
        note: &NoteRecord,
        rect: Rect,
        level: DetailLevel,
        options: NotePaintOptions,
    ) -> Option<Rect> {
        let mut painter = painter.clone();
        painter.set_opacity(options.opacity);
        let accent = note_color(note);
        let scale = self.camera.scale;

        if level == DetailLevel::Markers {
            paint_note_marker(&painter, rect.center(), accent, options.selected);
            return None;
        }
        paint_note_chrome(&painter, rect, accent, scale, options);

        let padding = note_padding(level, scale);
        let font_sizes = font_sizes(level, options.typography_scale);
        let content_painter = clipped_content_painter(&painter, rect, padding);
        let title_offset = paint_note_eyebrow(
            &content_painter,
            rect,
            padding,
            accent,
            font_sizes.metadata.max(NOTE_EYEBROW_FONT_SIZE),
            level == DetailLevel::Content && rect.height() > NOTE_HEADER_MIN_HEIGHT,
        );
        let title_bottom = paint_note_title(
            &content_painter,
            note,
            rect,
            padding,
            font_sizes.title,
            note_title_position(
                rect,
                padding,
                level,
                title_offset,
                title_lod_progress(scale),
            ),
        );

        if level != DetailLevel::Content {
            return None;
        }

        let footer_visible = note_footer_visible(rect, options.show_body);
        let body_top = title_bottom + (font_sizes.title * 0.55).clamp(8.0, 16.0);
        let body_bottom =
            rect.bottom() - padding.y - if footer_visible { FOOTER_RESERVE } else { 0.0 };
        if options.show_body
            && let Some(body_rect) = note_body_rect(rect, padding.x, body_top, body_bottom)
        {
            self.paint_note_body(&painter, &content_painter, note, body_rect, options);
        }

        footer_visible.then(|| {
            paint_note_footer(
                &content_painter,
                rect,
                padding,
                note,
                accent,
                font_sizes.metadata.max(MIN_FOOTER_FONT_SIZE),
                font_sizes.counts.max(MIN_FOOTER_FONT_SIZE),
            )
        })
    }

    fn paint_note_body(
        &mut self,
        painter: &eframe::egui::Painter,
        content_painter: &eframe::egui::Painter,
        note: &NoteRecord,
        body_rect: Rect,
        options: NotePaintOptions,
    ) {
        let galley = self.markdown_cache.card_galley(
            painter,
            note,
            options.body_typography_scale,
            body_rect.width(),
            painter.ctx().pixels_per_point(),
        );
        let body_scroll = options
            .body_scroll
            .min((galley.size().y - body_rect.height()).max(0.0));
        if options.snapped {
            self.snapped_scroll = body_scroll;
        }
        content_painter.with_clip_rect(body_rect).galley(
            body_rect.min - Vec2::new(0.0, body_scroll),
            galley,
            theme::INK,
        );
    }
}

fn paint_note_marker(
    painter: &eframe::egui::Painter,
    center: Pos2,
    accent: Color32,
    selected: bool,
) {
    painter.circle_filled(center, if selected { 6.5 } else { 4.5 }, accent);
    if selected {
        painter.circle_stroke(center, 8.0, Stroke::new(2.0, Color32::WHITE));
    }
}

fn paint_note_chrome(
    painter: &eframe::egui::Painter,
    rect: Rect,
    accent: Color32,
    scale: f32,
    options: NotePaintOptions,
) {
    let corner_radius = (9.0 * scale).clamp(9.0, 18.0);
    let shadow_offset = (3.0 * scale).clamp(3.0, 8.0);
    painter.rect_filled(
        rect.translate(Vec2::new(0.0, shadow_offset)).expand(2.0),
        corner_radius + 2.0,
        OUTER_SHADOW,
    );
    painter.rect_filled(
        rect.translate(Vec2::new(0.0, shadow_offset * 0.35)),
        corner_radius,
        INNER_SHADOW,
    );
    painter.rect_filled(
        rect,
        corner_radius,
        if options.selected {
            theme::PAPER
        } else {
            theme::PAPER_STRONG
        },
    );
    let accent_width = (4.0 * scale).clamp(3.0, 6.0);
    painter.rect_filled(
        Rect::from_min_max(rect.min, Pos2::new(rect.min.x + accent_width, rect.max.y)),
        corner_radius,
        accent,
    );
    if !options.snapped {
        let (width, color) = if options.selected {
            (2.0, theme::SAGE)
        } else {
            (1.0, theme::LINE)
        };
        painter.rect_stroke(
            rect,
            corner_radius,
            Stroke::new(width, color),
            StrokeKind::Inside,
        );
    }
}

fn clipped_content_painter(
    painter: &eframe::egui::Painter,
    rect: Rect,
    padding: Vec2,
) -> eframe::egui::Painter {
    let clip_padding = Vec2::new(
        padding.x.min(rect.width() / 4.0),
        padding.y.min(rect.height() / 4.0),
    );
    painter.with_clip_rect(Rect::from_min_max(
        rect.min + clip_padding,
        rect.max - clip_padding,
    ))
}

fn paint_note_title(
    painter: &eframe::egui::Painter,
    note: &NoteRecord,
    rect: Rect,
    padding: Vec2,
    font_size: f32,
    position: Pos2,
) -> f32 {
    let galley = painter.layout(
        note.title.clone(),
        FontId::proportional(font_size),
        theme::INK,
        (rect.width() - padding.x * 2.0).max(1.0),
    );
    let bottom = position.y + galley.size().y;
    paint_bold_galley(painter, position, galley);
    bottom
}

fn paint_bold_galley(painter: &eframe::egui::Painter, position: Pos2, galley: Arc<Galley>) {
    painter.galley(position, galley.clone(), theme::INK);
    let pixel = 1.0 / painter.ctx().pixels_per_point().max(1.0);
    painter.galley(position + Vec2::new(pixel, 0.0), galley, theme::INK);
}

fn paint_note_eyebrow(
    painter: &eframe::egui::Painter,
    rect: Rect,
    padding: Vec2,
    accent: Color32,
    font_size: f32,
    visible: bool,
) -> f32 {
    if !visible {
        return 0.0;
    }

    let badge_size = NOTE_BADGE_SIZE;
    let badge = Rect::from_min_size(rect.left_top() + padding, Vec2::splat(badge_size));
    painter.rect_filled(
        badge,
        6.0,
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 24),
    );
    paint_note_icon(painter, badge, accent);
    painter.text(
        badge.right_center() + Vec2::new(12.0, 0.0),
        Align2::LEFT_CENTER,
        "NOTE",
        FontId::proportional(font_size),
        accent,
    );

    badge_size + NOTE_HEADER_GAP
}

fn paint_note_icon(painter: &eframe::egui::Painter, badge: Rect, color: Color32) {
    let center = badge.center();
    let left = center.x - 7.0;
    let right = center.x + 7.0;
    let top = center.y - 9.0;
    let bottom = center.y + 9.0;
    let fold = 5.0;
    let stroke = Stroke::new(1.5, color);

    for segment in [
        [Pos2::new(left, top), Pos2::new(right - fold, top)],
        [Pos2::new(right - fold, top), Pos2::new(right, top + fold)],
        [Pos2::new(right, top + fold), Pos2::new(right, bottom)],
        [Pos2::new(right, bottom), Pos2::new(left, bottom)],
        [Pos2::new(left, bottom), Pos2::new(left, top)],
        [
            Pos2::new(right - fold, top),
            Pos2::new(right - fold, top + fold),
        ],
        [
            Pos2::new(right - fold, top + fold),
            Pos2::new(right, top + fold),
        ],
        [
            Pos2::new(left + 3.0, center.y + 1.0),
            Pos2::new(right - 3.0, center.y + 1.0),
        ],
        [
            Pos2::new(left + 3.0, center.y + 5.0),
            Pos2::new(right - 5.0, center.y + 5.0),
        ],
    ] {
        painter.line_segment(segment, stroke);
    }
}

fn paint_tag_pills(
    painter: &eframe::egui::Painter,
    rect: Rect,
    padding: Vec2,
    note: &NoteRecord,
    accent: Color32,
    font_size: f32,
    max_right: f32,
) {
    let mut left = rect.left() + padding.x;
    let bottom = rect.bottom() - padding.y;
    for (index, tag) in note.tags.iter().take(3).enumerate() {
        let label = format!("#{tag}");
        let galley = painter.layout_no_wrap(
            label,
            FontId::proportional(font_size),
            if index == 0 { accent } else { theme::MUTED },
        );
        let pill_size = galley.size() + Vec2::new(14.0, 8.0);
        if left + pill_size.x > max_right {
            break;
        }
        let pill = Rect::from_min_size(Pos2::new(left, bottom - pill_size.y), pill_size);
        let fill = if index == 0 {
            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 22)
        } else {
            Color32::from_rgb(237, 237, 230)
        };
        painter.rect_filled(pill, pill.height() / 2.0, fill);
        painter.galley(pill.center() - galley.size() / 2.0, galley, theme::INK);
        left = pill.right() + 6.0;
    }
}

fn paint_note_footer(
    painter: &eframe::egui::Painter,
    rect: Rect,
    padding: Vec2,
    note: &NoteRecord,
    accent: Color32,
    tag_font_size: f32,
    count_font_size: f32,
) -> Rect {
    let counts = painter.layout_no_wrap(
        format!(
            "{} references  ·  {} backlinks  ·  {} citations",
            note.references.len(),
            note.backlinks.len(),
            note.citations.len()
        ),
        FontId::proportional(count_font_size),
        theme::MUTED,
    );
    let counts_position = Pos2::new(
        rect.right() - padding.x - counts.size().x,
        rect.bottom() - padding.y - counts.size().y,
    );
    paint_tag_pills(
        painter,
        rect,
        padding,
        note,
        accent,
        tag_font_size,
        counts_position.x - 10.0,
    );
    let counts_rect = Rect::from_min_size(counts_position, counts.size());
    painter.galley(counts_position, counts, theme::MUTED);
    counts_rect
}

pub(super) fn note_footer_visible(rect: Rect, show_body: bool) -> bool {
    show_body && rect.width() >= 320.0 && rect.height() >= 140.0
}

pub(super) fn note_body_rect(
    note_rect: Rect,
    padding: f32,
    body_top: f32,
    body_bottom: f32,
) -> Option<Rect> {
    let rect = Rect::from_min_max(
        Pos2::new(note_rect.left() + padding, body_top),
        Pos2::new(note_rect.right() - padding, body_bottom),
    );
    (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect)
}

pub(super) fn note_title_position(
    note_rect: Rect,
    padding: Vec2,
    level: DetailLevel,
    header_offset: f32,
    title_progress: f32,
) -> Pos2 {
    let y = if level == DetailLevel::Titles {
        note_rect.top() + padding.y + NOTE_HEADER_RESERVED_HEIGHT * title_progress.clamp(0.0, 1.0)
    } else {
        note_rect.top() + padding.y + header_offset
    };
    Pos2::new(note_rect.left() + padding.x, y)
}

pub(super) fn note_padding(level: DetailLevel, scale: f32) -> Vec2 {
    let scaled = NOTE_PADDING_WORLD * scale;
    if level == DetailLevel::Content {
        Vec2::new(scaled, scaled.clamp(8.0, 32.0))
    } else {
        Vec2::splat(scaled.clamp(8.0, 18.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FontSizes {
    pub(super) title: f32,
    pub(super) metadata: f32,
    pub(super) counts: f32,
}

pub(super) fn font_sizes(level: DetailLevel, scale: f32) -> FontSizes {
    match level {
        DetailLevel::Markers => FontSizes {
            title: 0.0,
            metadata: 0.0,
            counts: 0.0,
        },
        DetailLevel::Titles => FontSizes {
            title: (7.0 * scale).max(READABLE_TITLE_FONT_SIZE),
            metadata: 0.0,
            counts: 0.0,
        },
        DetailLevel::Content => FontSizes {
            title: (7.0 * scale).max(READABLE_TITLE_FONT_SIZE),
            metadata: 2.8 * scale,
            counts: 2.8 * scale,
        },
    }
}

fn note_color(note: &NoteRecord) -> Color32 {
    cluster_color(note.tags.first().map_or(&note.title, String::as_str))
}

pub(super) fn cluster_color(value: &str) -> Color32 {
    const PALETTE: [Color32; 12] = [
        Color32::from_rgb(59, 113, 90),
        Color32::from_rgb(165, 102, 42),
        Color32::from_rgb(100, 81, 160),
        Color32::from_rgb(47, 112, 154),
        Color32::from_rgb(154, 72, 101),
        Color32::from_rgb(111, 119, 55),
        Color32::from_rgb(38, 126, 126),
        Color32::from_rgb(177, 76, 48),
        Color32::from_rgb(73, 83, 155),
        Color32::from_rgb(145, 67, 143),
        Color32::from_rgb(128, 90, 59),
        Color32::from_rgb(77, 99, 115),
    ];
    let hash = value.bytes().fold(0_usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    });
    PALETTE[hash % PALETTE.len()]
}
