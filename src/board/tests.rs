use std::path::PathBuf;

use eframe::egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};

use super::{
    CARD_SIZE, Camera, DetailLevel, FADE_START_THRESHOLD, ISOLATION_THRESHOLD,
    READABLE_TITLE_FONT_SIZE, SNAP_MAGNET_THRESHOLD, SNAP_THRESHOLD, body_typography_scale,
    body_visible, card_width_ratio, detail_level, font_sizes, grid_layout, magnetized_note_rect,
    note_body_rect, note_footer_visible, note_padding, note_title_position, note_typography_scale,
    note_viewport_coverage, related_note_ids, resolved_edges, snap_magnet_progress,
    title_card_size, toggled_selection, unrelated_opacity,
};
use crate::markdown::CARD_BODY_FONT_WORLD;
use crate::theme;
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
fn title_lod_enters_wide_and_converges_to_the_content_card_size() {
    let entering = title_card_size(super::MARKER_THRESHOLD);
    let leaving = title_card_size(super::TITLE_THRESHOLD);
    let natural_entering = CARD_SIZE * super::MARKER_THRESHOLD;
    let natural_leaving = CARD_SIZE * super::TITLE_THRESHOLD;
    let content_leaving = super::content_card_size(super::TITLE_THRESHOLD);

    assert!((entering.x - natural_entering.x * 2.0).abs() < f32::EPSILON);
    assert!((entering.y - natural_entering.y * 1.65).abs() < f32::EPSILON);
    assert!((leaving.x - natural_leaving.x).abs() < f32::EPSILON);
    assert!((leaving.y - content_leaving.y).abs() < f32::EPSILON);
    assert!(
        (content_leaving.y - natural_leaving.y - super::NOTE_HEADER_RESERVED_HEIGHT).abs()
            < f32::EPSILON
    );
}

#[test]
fn title_only_lod_moves_from_the_top_to_its_content_position() {
    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(240.0, 120.0));
    let padding = Vec2::splat(12.0);
    let entering = note_title_position(rect, padding, DetailLevel::Titles, 46.0, 0.0);
    let halfway = note_title_position(rect, padding, DetailLevel::Titles, 46.0, 0.5);
    let leaving = note_title_position(rect, padding, DetailLevel::Titles, 46.0, 1.0);
    let content_title = note_title_position(rect, padding, DetailLevel::Content, 46.0, 1.0);

    assert_eq!(entering.y, rect.top() + 12.0);
    assert_eq!(halfway.y, rect.top() + 12.0 + 23.0);
    assert_eq!(leaving.y, rect.top() + 12.0 + 46.0);
    assert_eq!(content_title.y, rect.top() + 12.0 + 46.0);
}

#[test]
fn content_padding_keeps_full_width_while_capping_vertical_insets() {
    let padding = note_padding(DetailLevel::Content, 8.0);

    assert_eq!(padding.x, 112.0);
    assert_eq!(padding.y, 32.0);
}

#[test]
fn visible_titles_start_readable_then_scale_toward_snap() {
    let title_at_zero = font_sizes(DetailLevel::Titles, 0.0);
    let title_at_one = font_sizes(DetailLevel::Titles, 1.0);
    let title_at_three = font_sizes(DetailLevel::Titles, 3.0);
    assert_eq!(title_at_zero.title, READABLE_TITLE_FONT_SIZE);
    assert_eq!(title_at_one.title, READABLE_TITLE_FONT_SIZE);
    assert!(title_at_three.title > title_at_one.title);
    assert_eq!(
        title_at_one.title,
        font_sizes(DetailLevel::Content, 1.0).title
    );
}

#[test]
fn content_metadata_scales_proportionally_with_the_card() {
    let at_one = font_sizes(DetailLevel::Content, 1.0);
    let at_two = font_sizes(DetailLevel::Content, 2.0);
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
fn snapped_markdown_uses_the_application_body_font_size() {
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
    let snap_scale = viewport.width() * SNAP_THRESHOLD / CARD_SIZE.x;
    let typography_scale = note_typography_scale(snap_scale, viewport);

    assert!((CARD_BODY_FONT_WORLD * typography_scale - theme::BODY_FONT_SIZE).abs() < f32::EPSILON);
}

#[test]
fn markdown_body_uses_the_note_width_between_its_padding() {
    let note_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
    let padding = 56.0;
    let body = note_body_rect(note_rect, padding, 120.0, 620.0).expect("valid body rect");

    assert_eq!(body.left(), note_rect.left() + padding);
    assert_eq!(body.right(), note_rect.right() - padding);
    assert_eq!(body.width(), note_rect.width() - padding * 2.0);
}

#[test]
fn typography_interpolates_from_zero_to_the_snap_font_size() {
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_600.0, 900.0));
    let half_scale = viewport.width() * 0.5 / CARD_SIZE.x;
    let magnet_scale = viewport.width() * SNAP_MAGNET_THRESHOLD / CARD_SIZE.x;
    let snap_scale = viewport.width() * SNAP_THRESHOLD / CARD_SIZE.x;
    let at_zero = note_typography_scale(0.0, viewport);
    let at_half = note_typography_scale(half_scale, viewport);
    let at_magnet = note_typography_scale(magnet_scale, viewport);
    let at_snap = note_typography_scale(snap_scale, viewport);

    assert_eq!(at_zero, 0.0);
    assert!((at_half - at_snap * 0.5).abs() < f32::EPSILON);
    assert!((at_magnet - at_snap * SNAP_MAGNET_THRESHOLD).abs() < f32::EPSILON);
    assert!((CARD_BODY_FONT_WORLD * at_snap - theme::BODY_FONT_SIZE).abs() < f32::EPSILON);
}

#[test]
fn typography_progress_is_independent_of_viewport_size() {
    let small = Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 480.0));
    let wide = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_600.0, 900.0));
    let progress = 0.63;
    let small_scale = small.width() * progress / CARD_SIZE.x;
    let wide_scale = wide.width() * progress / CARD_SIZE.x;

    assert!(
        (note_typography_scale(small_scale, small) - note_typography_scale(wide_scale, wide)).abs()
            < 0.000_001
    );
}

#[test]
fn markdown_body_appears_at_the_preview_threshold_without_a_line_limit() {
    assert!(!body_visible(0.10));
    assert!(!body_visible(super::BODY_PREVIEW_START - 0.001));
    assert!(body_visible(super::BODY_PREVIEW_START));
    assert!(body_visible(1.00));
}

#[test]
fn body_typography_tracks_the_rendered_width_without_reflowing() {
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
    let mut wrap_ratios = Vec::new();

    for progress in [0.20, 0.50, 0.85, 0.90, 1.00] {
        let camera_scale = viewport.width() * progress / CARD_SIZE.x;
        let natural =
            Rect::from_center_size(viewport.center(), super::content_card_size(camera_scale));
        let rect = super::magnetized_note_rect(
            natural,
            viewport,
            snap_magnet_progress(camera_scale, viewport),
        );
        let horizontal_padding = note_padding(DetailLevel::Content, camera_scale).x;
        let typography = body_typography_scale(rect.width(), horizontal_padding, viewport);
        let body_width = rect.width() - horizontal_padding * 2.0;
        wrap_ratios.push(body_width / typography);
    }

    for pair in wrap_ratios.windows(2) {
        assert!((pair[0] - pair[1]).abs() < 0.001);
    }

    let snapped_padding = note_padding(DetailLevel::Content, viewport.width() / CARD_SIZE.x).x;
    let snapped = body_typography_scale(viewport.width(), snapped_padding, viewport);
    assert!((CARD_BODY_FONT_WORLD * snapped - theme::BODY_FONT_SIZE).abs() < f32::EPSILON);
}

#[test]
fn note_footer_waits_for_enough_card_space() {
    let cramped = Rect::from_min_size(Pos2::ZERO, Vec2::new(240.0, 110.0));
    let readable = Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 140.0));

    assert!(!note_footer_visible(cramped, true));
    assert!(!note_footer_visible(readable, false));
    assert!(note_footer_visible(readable, true));
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

    assert_eq!(selection, Some(Some(selected_id.clone())));
    board.select_note(Some(&selected_id));
    assert!(board.relationship_panel_open);
}

#[test]
fn clicking_the_selected_note_requests_deselection() {
    let selected = note("Selected");
    let selected_id = selected.id.clone();

    assert_eq!(
        toggled_selection(Some(selected_id.clone()), Some(&selected_id)),
        None
    );
    assert_eq!(
        toggled_selection(Some(selected_id.clone()), None),
        Some(selected_id)
    );
}

#[test]
fn clicking_a_reference_in_the_side_panel_navigates_to_it() {
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
    board.camera.scale = screen.width() * SNAP_THRESHOLD / CARD_SIZE.x;
    board.snapped_note = Some(source_id.clone());
    board.relationship_panel_open = true;
    // Exercise the empty far-right side of the row, not its text label.
    let pointer = Pos2::new(970.0, 50.0);
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
fn snapped_relationship_counts_are_the_only_panel_trigger() {
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
    board.select_note(Some(&selected_id));

    let context = eframe::egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
    board.camera.scale = screen.width() * SNAP_THRESHOLD / CARD_SIZE.x;
    board.snapped_note = Some(selected_id.clone());
    board.relationship_panel_open = false;

    let note_body = screen.center();
    let mut body_selection = None;
    let _ = context.run_ui(pointer_input(screen, note_body, true), |ui| {
        board.show(ui, &index, None);
    });
    let _ = context.run_ui(pointer_input(screen, note_body, false), |ui| {
        body_selection = board.show(ui, &index, None).selection_request;
    });
    assert_eq!(body_selection, None);
    assert!(!board.relationship_panel_open);

    let relationship_counts = Pos2::new(900.0, 660.0);
    let _ = context.run_ui(pointer_input(screen, relationship_counts, true), |ui| {
        board.show(ui, &index, Some(&selected_id));
    });
    let _ = context.run_ui(pointer_input(screen, relationship_counts, false), |ui| {
        board.show(ui, &index, Some(&selected_id));
    });
    assert!(board.relationship_panel_open);
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
