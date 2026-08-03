use eframe::egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2};

use super::RELATIONSHIP_PANEL_WIDTH;
use crate::theme;
use crate::vault::{NoteId, NoteRecord, VaultIndex};

#[derive(Debug)]
pub(super) struct RelationshipPanel {
    pub(super) bounds: Rect,
    navigation: Vec<(Rect, NoteId)>,
}

impl RelationshipPanel {
    pub(super) fn navigation_at(&self, pointer: Pos2) -> Option<NoteId> {
        self.navigation
            .iter()
            .find(|(rect, _)| rect.contains(pointer))
            .map(|(_, note_id)| note_id.clone())
    }
}

pub(super) fn paint_relationship_panel(
    painter: &eframe::egui::Painter,
    viewport: Rect,
    index: &VaultIndex,
    note: &NoteRecord,
) -> RelationshipPanel {
    let panel_width = RELATIONSHIP_PANEL_WIDTH.min((viewport.width() - 24.0).max(160.0));
    let bounds = Rect::from_min_size(
        Pos2::new(viewport.right() - panel_width - 12.0, viewport.top() + 12.0),
        Vec2::new(panel_width, (viewport.height() - 24.0).max(72.0)),
    );
    painter.rect_filled(
        bounds.translate(Vec2::new(3.0, 4.0)),
        10.0,
        Color32::from_rgba_unmultiplied(43, 49, 43, 22),
    );
    painter.rect_filled(bounds, 10.0, Color32::from_rgb(251, 250, 246));
    painter.rect_stroke(
        bounds,
        10.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(75, 85, 75, 52)),
        StrokeKind::Inside,
    );

    let section_height = bounds.height() / 3.0;
    let mut navigation = Vec::new();
    paint_note_relationship_section(
        painter,
        Rect::from_min_size(bounds.min, Vec2::new(bounds.width(), section_height)),
        "References",
        &note.references,
        index,
        &mut navigation,
    );
    paint_note_relationship_section(
        painter,
        Rect::from_min_size(
            Pos2::new(bounds.left(), bounds.top() + section_height),
            Vec2::new(bounds.width(), section_height),
        ),
        "Backlinks",
        &note.backlinks,
        index,
        &mut navigation,
    );
    paint_citation_section(
        painter,
        Rect::from_min_size(
            Pos2::new(bounds.left(), bounds.top() + section_height * 2.0),
            Vec2::new(bounds.width(), section_height),
        ),
        &note.citations,
    );

    for separator in [
        bounds.top() + section_height,
        bounds.top() + section_height * 2.0,
    ] {
        painter.line_segment(
            [
                Pos2::new(bounds.left() + 12.0, separator),
                Pos2::new(bounds.right() - 12.0, separator),
            ],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(75, 85, 75, 32)),
        );
    }

    RelationshipPanel { bounds, navigation }
}

fn paint_note_relationship_section(
    painter: &eframe::egui::Painter,
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
        theme::MUTED,
    );
    for (row, note_id) in ids.iter().take(max_visible_rows(column)).enumerate() {
        let top = column.top() + 30.0 + row as f32 * 30.0;
        let rect = Rect::from_min_size(
            Pos2::new(column.left() + 8.0, top),
            Vec2::new((column.width() - 16.0).max(1.0), 26.0),
        );
        let hovered = painter
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|pointer| rect.contains(pointer));
        if hovered {
            painter
                .ctx()
                .set_cursor_icon(eframe::egui::CursorIcon::PointingHand);
        }
        clipped.rect_filled(
            rect,
            5.0,
            if hovered {
                Color32::from_rgb(222, 231, 220)
            } else {
                Color32::from_rgb(235, 239, 233)
            },
        );
        clipped.text(
            rect.left_center() + Vec2::new(6.0, 0.0),
            Align2::LEFT_CENTER,
            note_title(index, note_id),
            FontId::proportional(10.0),
            theme::SAGE_DARK,
        );
        navigation.push((rect, note_id.clone()));
    }
}

fn paint_citation_section(painter: &eframe::egui::Painter, column: Rect, citations: &[String]) {
    let clipped = painter.with_clip_rect(column.shrink(8.0));
    clipped.text(
        column.left_top() + Vec2::new(10.0, 9.0),
        Align2::LEFT_TOP,
        format!("Citations · {}", citations.len()),
        FontId::proportional(10.0),
        theme::MUTED,
    );
    for (row, citation) in citations.iter().take(max_visible_rows(column)).enumerate() {
        clipped.text(
            Pos2::new(
                column.left() + 10.0,
                column.top() + 33.0 + row as f32 * 21.0,
            ),
            Align2::LEFT_TOP,
            format!("@{citation}"),
            FontId::monospace(10.0),
            theme::AMBER,
        );
    }
}

fn max_visible_rows(section: Rect) -> usize {
    ((section.height() - 34.0).max(0.0) / 30.0).floor() as usize
}

fn note_title<'a>(index: &'a VaultIndex, note_id: &NoteId) -> &'a str {
    index
        .notes
        .iter()
        .find(|note| &note.id == note_id)
        .map_or("Untitled", |note| note.title.as_str())
}
