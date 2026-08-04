use std::{path::PathBuf, time::Instant};

use eframe::egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};

use super::metaballs::{BASE_INFLUENCE_RADIUS, FIELD_CELL_BUDGET, build_geometry};
use super::{
    CARD_SIZE, Camera, DetailLevel, FADE_START_THRESHOLD, GRID_SPACING, ISOLATION_THRESHOLD,
    LayoutSeed, READABLE_TITLE_FONT_SIZE, SNAP_MAGNET_THRESHOLD, SNAP_THRESHOLD, SelectionRequest,
    body_typography_scale, body_visible, card_width_ratio, clustered_layout, detail_level,
    font_sizes, magnetized_note_rect, note_body_rect, note_footer_visible, note_padding,
    note_title_position, note_typography_scale, note_viewport_coverage, prepare_board_layout,
    related_note_ids, resolved_edges, snap_magnet_progress, title_card_size, unrelated_opacity,
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

fn tagged_note(name: &str, tags: &[&str]) -> NoteRecord {
    let mut note = note(name);
    note.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
    note
}

fn index(notes: Vec<NoteRecord>) -> VaultIndex {
    VaultIndex {
        root: PathBuf::from("/vault"),
        notes,
        diagnostics: Vec::new(),
        scan_duration: std::time::Duration::ZERO,
    }
}

fn seed_from(layout: &super::BoardLayout, root: &str) -> LayoutSeed {
    LayoutSeed {
        root: PathBuf::from(root),
        positions: layout.positions.clone(),
        fingerprints: layout.fingerprints.clone(),
    }
}

fn assert_no_card_overlaps(layout: &super::BoardLayout) {
    let positions: Vec<_> = layout.positions.values().copied().collect();
    for (index, left) in positions.iter().enumerate() {
        let left = Rect::from_center_size(*left, CARD_SIZE);
        for right in positions.iter().skip(index + 1) {
            assert!(!left.intersects(Rect::from_center_size(*right, CARD_SIZE)));
        }
    }
}

fn install_cold_layout(board: &mut super::BoardState, index: &VaultIndex) {
    let layout = prepare_board_layout(index, None);
    board.install_layout(index.root.clone(), layout, false, std::time::Duration::ZERO);
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
fn clustered_layout_is_rescan_stable_and_non_overlapping() {
    let notes = vec![
        tagged_note("A", &["methods"]),
        tagged_note("B", &["evidence"]),
        tagged_note("C", &["methods", "evidence"]),
        note("D"),
    ];
    let mut reversed = notes.clone();
    reversed.reverse();
    let first = clustered_layout(&index(notes));
    let second = clustered_layout(&index(reversed));
    assert_eq!(first.positions, second.positions);
    assert_eq!(first.clusters.len(), second.clusters.len());
    for (left, right) in first.clusters.iter().zip(&second.clusters) {
        assert_eq!(left.key, right.key);
        assert_eq!(left.name, right.name);
        assert_eq!(left.bounds(), right.bounds());
        assert_eq!(left.label_anchor, right.label_anchor);
        assert_eq!(left.note_count, right.note_count);
        assert_eq!(left.influence_radius, right.influence_radius);
        assert_eq!(left.geometry.vertices.len(), right.geometry.vertices.len());
        assert_eq!(left.geometry.indices.len(), right.geometry.indices.len());
        assert_eq!(left.geometry.contours.len(), right.geometry.contours.len());
        assert_eq!(left.geometry.vertices, right.geometry.vertices);
        assert_eq!(left.geometry.indices, right.geometry.indices);
        assert_eq!(left.geometry.contours, right.geometry.contours);
    }
    let values: Vec<_> = first.positions.values().collect();
    for (index, left) in values.iter().enumerate() {
        for right in values.iter().skip(index + 1) {
            let distance = (**left - **right).abs();
            assert!(distance.x >= GRID_SPACING.x || distance.y >= GRID_SPACING.y);
        }
    }
    assert_eq!(
        first
            .clusters
            .iter()
            .map(|cluster| cluster.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Untagged", "evidence", "methods"]
    );
}

#[test]
fn row_packing_prevents_card_overlap_at_scale() {
    for count in [10, 100, 1_000] {
        let notes = (0..count)
            .map(|note_index| tagged_note(&format!("Note {note_index:04}"), &["dense"]))
            .collect();
        let layout = clustered_layout(&index(notes));
        assert_eq!(layout.positions.len(), count);
        assert_no_card_overlaps(&layout);
        assert!(layout.stats.connectivity_passes <= 32);
    }
}

#[test]
fn co_occurring_tags_are_closer_than_unrelated_tags() {
    let layout = clustered_layout(&index(vec![
        tagged_note("Shared 1", &["alpha", "beta"]),
        tagged_note("Shared 2", &["alpha", "beta"]),
        tagged_note("Gamma", &["gamma"]),
        tagged_note("Delta", &["delta"]),
    ]));
    let center = |name| {
        layout
            .clusters
            .iter()
            .find(|cluster| cluster.name == name)
            .expect("named cluster")
            .bounds()
            .center()
    };
    assert!(center("alpha").distance(center("beta")) < center("gamma").distance(center("delta")));
}

#[test]
fn warm_rescans_preserve_unchanged_positions() {
    let original = index(vec![
        tagged_note("Alpha", &["alpha"]),
        tagged_note("Beta", &["beta"]),
        tagged_note("Bridge", &["alpha", "beta"]),
    ]);
    let cold = prepare_board_layout(&original, None);
    let seed = seed_from(&cold, "/vault");
    let unchanged = prepare_board_layout(&original, Some(&seed));
    assert_eq!(cold.positions, unchanged.positions);

    let mut added_index = original.clone();
    added_index
        .notes
        .push(tagged_note("Unrelated", &["unrelated"]));
    let added = prepare_board_layout(&added_index, Some(&seed));
    for (note_id, position) in &cold.positions {
        assert_eq!(added.positions[note_id], *position);
    }

    let mut deleted_index = original.clone();
    deleted_index.notes.retain(|note| note.title != "Beta");
    let deleted = prepare_board_layout(&deleted_index, Some(&seed));
    for note in &deleted_index.notes {
        assert_eq!(deleted.positions[&note.id], cold.positions[&note.id]);
    }
}

#[test]
fn same_vault_install_preserves_an_intersecting_camera() {
    let vault = index(vec![
        tagged_note("Alpha", &["alpha"]),
        tagged_note("Beta", &["beta"]),
    ]);
    let cold = prepare_board_layout(&vault, None);
    let mut board = super::BoardState::default();
    board.install_layout(vault.root.clone(), cold, false, std::time::Duration::ZERO);
    let focus = board.positions[&NoteId(PathBuf::from("Alpha.md"))];
    board.camera = Camera {
        center: focus,
        scale: 0.75,
    };
    board.last_viewport = Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0)));
    let seed = board.layout_seed(&vault.root).expect("same-root seed");
    let warm = prepare_board_layout(&vault, Some(&seed));
    board.install_layout(vault.root.clone(), warm, true, std::time::Duration::ZERO);

    assert_eq!(board.camera.center, focus);
    assert_eq!(board.camera.scale, 0.75);
    assert!(board.fitted);
}

#[test]
fn changed_note_only_moves_the_affected_area_when_possible() {
    let original = index(vec![
        tagged_note("Alpha 1", &["alpha"]),
        tagged_note("Alpha 2", &["alpha"]),
        tagged_note("Stable", &["stable"]),
    ]);
    let cold = prepare_board_layout(&original, None);
    let seed = seed_from(&cold, "/vault");
    let mut edited = original.clone();
    edited.notes[0].tags = vec!["changed".to_owned()];
    let warm = prepare_board_layout(&edited, Some(&seed));
    let stable = NoteId(PathBuf::from("Stable.md"));
    assert_eq!(warm.positions[&stable], cold.positions[&stable]);
}

#[test]
fn seeds_from_other_roots_are_ignored() {
    let mut other_root = index(vec![
        tagged_note("Alpha", &["alpha"]),
        tagged_note("Beta", &["beta"]),
    ]);
    other_root.root = PathBuf::from("/other-vault");
    let cold = prepare_board_layout(&other_root, None);
    let mut wrong_seed = seed_from(&cold, "/vault");
    wrong_seed.positions.insert(
        NoteId(PathBuf::from("Alpha.md")),
        Pos2::new(99_999.0, 99_999.0),
    );
    let rejected = prepare_board_layout(&other_root, Some(&wrong_seed));
    assert_eq!(rejected, cold);
}

#[test]
fn fallback_radius_growth_is_local_to_the_disconnected_tag() {
    let vault = index(vec![
        tagged_note("Alpha 1", &["alpha"]),
        tagged_note("Alpha 2", &["alpha"]),
        tagged_note("Beta", &["beta"]),
    ]);
    let cold = prepare_board_layout(&vault, None);
    let mut seed = seed_from(&cold, "/vault");
    seed.positions.insert(
        NoteId(PathBuf::from("Alpha 1.md")),
        Pos2::new(-10.0 * GRID_SPACING.x, 0.0),
    );
    seed.positions.insert(
        NoteId(PathBuf::from("Alpha 2.md")),
        Pos2::new(10.0 * GRID_SPACING.x, 0.0),
    );
    let layout = prepare_board_layout(&vault, Some(&seed));
    let cluster = |name| {
        layout
            .clusters
            .iter()
            .find(|cluster| cluster.name == name)
            .expect("named cluster")
    };
    assert!(cluster("alpha").influence_radius > BASE_INFLUENCE_RADIUS);
    assert_eq!(cluster("beta").influence_radius, BASE_INFLUENCE_RADIUS);
    assert_eq!(layout.stats.fallback_cluster_count, 1);
    assert_eq!(cluster("alpha").geometry.contours.len(), 1);
}

#[test]
fn generated_meshes_are_finite_indexed_and_near_the_cell_budget() {
    let notes = (0..1_000)
        .map(|note_index| tagged_note(&format!("Note {note_index:04}"), &["dense"]))
        .collect();
    let layout = clustered_layout(&index(notes));
    assert!(layout.stats.sampled_field_cells as f32 <= FIELD_CELL_BUDGET * 1.05);
    for cluster in &layout.clusters {
        assert!(
            cluster
                .geometry
                .vertices
                .iter()
                .all(|vertex| vertex.is_finite())
        );
        assert!(
            cluster
                .geometry
                .indices
                .iter()
                .all(|index| (*index as usize) < cluster.geometry.vertices.len())
        );
        assert_eq!(cluster.geometry.contours.len(), 1);
    }
}

#[test]
fn multi_tag_note_centers_are_inside_every_associated_mesh() {
    let layout = clustered_layout(&index(vec![
        tagged_note("Alpha", &["alpha"]),
        tagged_note("Beta", &["beta"]),
        tagged_note("Bridge", &["alpha", "beta"]),
    ]));
    let bridge = layout.positions[&NoteId(PathBuf::from("Bridge.md"))];
    for name in ["alpha", "beta"] {
        let cluster = layout
            .clusters
            .iter()
            .find(|cluster| cluster.name == name)
            .expect("named cluster");
        assert!(mesh_contains(&cluster.geometry, bridge));
    }
}

fn mesh_contains(geometry: &super::metaballs::ClusterGeometry, point: Pos2) -> bool {
    geometry.indices.chunks_exact(3).any(|indices| {
        let [a, b, c] = [
            geometry.vertices[indices[0] as usize],
            geometry.vertices[indices[1] as usize],
            geometry.vertices[indices[2] as usize],
        ];
        let sign = |left: Pos2, right: Pos2| {
            (point.x - right.x) * (left.y - right.y) - (left.x - right.x) * (point.y - right.y)
        };
        let [ab, bc, ca] = [sign(a, b), sign(b, c), sign(c, a)];
        (ab >= 0.0 && bc >= 0.0 && ca >= 0.0) || (ab <= 0.0 && bc <= 0.0 && ca <= 0.0)
    })
}

#[test]
fn unrelated_notes_move_out_of_fallback_cluster_fields() {
    let vault = index(vec![
        tagged_note("Alpha 1", &["alpha"]),
        tagged_note("Alpha 2", &["alpha"]),
        tagged_note("Blocker", &["beta"]),
    ]);
    let cold = prepare_board_layout(&vault, None);
    let mut seed = seed_from(&cold, "/vault");
    seed.positions.insert(
        NoteId(PathBuf::from("Alpha 1.md")),
        Pos2::new(-GRID_SPACING.x, 0.0),
    );
    seed.positions.insert(
        NoteId(PathBuf::from("Alpha 2.md")),
        Pos2::new(GRID_SPACING.x, 0.0),
    );
    let blocker_id = NoteId(PathBuf::from("Blocker.md"));
    seed.positions.insert(blocker_id.clone(), Pos2::ZERO);

    let layout = prepare_board_layout(&vault, Some(&seed));
    let alpha = layout
        .clusters
        .iter()
        .find(|cluster| cluster.name == "alpha")
        .expect("alpha cluster");

    assert_ne!(layout.positions[&blocker_id], Pos2::ZERO);
    assert!(!mesh_contains(
        &alpha.geometry,
        layout.positions[&blocker_id]
    ));
    assert_no_card_overlaps(&layout);
}

#[test]
#[ignore = "release-mode preparation benchmark"]
fn benchmark_metaball_preparation() {
    for memberships in [1, 3] {
        for count in [100, 1_000, 5_000] {
            let notes: Vec<_> = (0..count)
                .map(|note_index| {
                    let mut note = note(&format!("Note {note_index:05}"));
                    note.tags = (0..memberships)
                        .map(|offset| format!("tag-{}", (note_index + offset) % 17))
                        .collect();
                    note
                })
                .collect();
            let vault = index(notes);
            let started = Instant::now();
            let cold = prepare_board_layout(&vault, None);
            let cold_duration = started.elapsed();
            let seed = seed_from(&cold, "/vault");
            let mut edited = vault.clone();
            edited.notes[0].tags.push("edited".to_owned());
            let started = Instant::now();
            let _warm = prepare_board_layout(&edited, Some(&seed));
            let warm_duration = started.elapsed();
            let positions: Vec<_> = cold.positions.values().copied().collect();
            let started = Instant::now();
            let cells = build_geometry(&positions, BASE_INFLUENCE_RADIUS, cold.stats.field_step)
                .sampled_cells;
            let field_duration = started.elapsed();
            eprintln!(
                "notes={count} memberships={memberships} cold_ms={:.1} warm_ms={:.1} field_ms={:.1} sampled_cells={} isolated_field_cells={cells} passes={} fallbacks={} max_radius={:.0} step={:.0}",
                cold_duration.as_secs_f64() * 1_000.0,
                warm_duration.as_secs_f64() * 1_000.0,
                field_duration.as_secs_f64() * 1_000.0,
                cold.stats.sampled_field_cells,
                cold.stats.connectivity_passes,
                cold.stats.fallback_cluster_count,
                cold.stats.maximum_influence_radius,
                cold.stats.field_step,
            );
            assert!(cold.stats.sampled_field_cells as f32 <= FIELD_CELL_BUDGET);
            if count == 1_000 {
                assert!(cold_duration.as_secs_f64() < 1.0);
                assert!(warm_duration.as_secs_f64() < 0.25);
            }
        }
    }
}

#[test]
fn independent_cluster_regions_stay_compact_and_non_overlapping() {
    let layout = clustered_layout(&index(vec![
        tagged_note("Alpha", &["alpha"]),
        tagged_note("Beta", &["beta"]),
    ]));
    let alpha = layout
        .clusters
        .iter()
        .find(|cluster| cluster.name == "alpha")
        .expect("alpha cluster");
    let beta = layout
        .clusters
        .iter()
        .find(|cluster| cluster.name == "beta")
        .expect("beta cluster");
    let alpha_position = layout.positions[&NoteId(PathBuf::from("Alpha.md"))];
    let beta_position = layout.positions[&NoteId(PathBuf::from("Beta.md"))];
    let separation = (alpha_position - beta_position).abs();

    assert!(separation.x <= GRID_SPACING.x * 4.0);
    assert!(separation.y <= GRID_SPACING.y * 6.0);
    assert!(
        !Rect::from_center_size(alpha_position, CARD_SIZE)
            .intersects(Rect::from_center_size(beta_position, CARD_SIZE))
    );
    assert!(alpha.bounds().contains(alpha_position));
    assert!(beta.bounds().contains(beta_position));
}

#[test]
fn same_tag_notes_fill_a_compact_grid_before_expanding() {
    let notes = (0..9)
        .map(|index| tagged_note(&format!("Note {index}"), &["cluster"]))
        .collect();
    let layout = clustered_layout(&index(notes));
    let slots: Vec<_> = layout
        .positions
        .values()
        .map(|position| {
            (
                (position.x / GRID_SPACING.x).round() as i32,
                (position.y / GRID_SPACING.y).round() as i32,
            )
        })
        .collect();
    let min_x = slots.iter().map(|slot| slot.0).min().expect("x slot");
    let max_x = slots.iter().map(|slot| slot.0).max().expect("x slot");
    let min_y = slots.iter().map(|slot| slot.1).min().expect("y slot");
    let max_y = slots.iter().map(|slot| slot.1).max().expect("y slot");

    assert_eq!(max_x - min_x, 2);
    assert_eq!(max_y - min_y, 2);
}

#[test]
fn growing_a_shared_tag_cluster_stays_as_compact_as_a_fresh_layout() {
    fn slot_span(layout: &super::BoardLayout) -> (i32, i32) {
        let slots: Vec<_> = layout
            .positions
            .values()
            .map(|position| {
                (
                    (position.x / GRID_SPACING.x).round() as i32,
                    (position.y / GRID_SPACING.y).round() as i32,
                )
            })
            .collect();
        let min_x = slots.iter().map(|slot| slot.0).min().expect("x slot");
        let max_x = slots.iter().map(|slot| slot.0).max().expect("x slot");
        let min_y = slots.iter().map(|slot| slot.1).min().expect("y slot");
        let max_y = slots.iter().map(|slot| slot.1).max().expect("y slot");
        (max_x - min_x, max_y - min_y)
    }

    let original = index(
        (0..5)
            .map(|index| tagged_note(&format!("Note {index}"), &["cluster"]))
            .collect(),
    );
    let cold = prepare_board_layout(&original, None);
    let seed = seed_from(&cold, "/vault");

    let mut grown = original.clone();
    grown
        .notes
        .extend((5..10).map(|index| tagged_note(&format!("Note {index}"), &["cluster"])));

    let warm = prepare_board_layout(&grown, Some(&seed));
    let fresh = prepare_board_layout(&grown, None);
    assert_eq!(slot_span(&warm), slot_span(&fresh));
    assert_no_card_overlaps(&warm);
}

#[test]
fn multi_tag_notes_contribute_to_each_connected_cluster() {
    let layout = clustered_layout(&index(vec![
        tagged_note("Alpha", &["alpha"]),
        tagged_note("Beta", &["beta"]),
        tagged_note("Bridge", &["alpha", "beta"]),
    ]));
    let alpha = layout
        .clusters
        .iter()
        .find(|cluster| cluster.name == "alpha")
        .expect("alpha cluster");
    let beta = layout
        .clusters
        .iter()
        .find(|cluster| cluster.name == "beta")
        .expect("beta cluster");
    let bridge = layout.positions[&NoteId(PathBuf::from("Bridge.md"))];
    assert!(alpha.bounds().contains(bridge));
    assert!(beta.bounds().contains(bridge));
    assert_eq!(alpha.note_count, 2);
    assert_eq!(beta.note_count, 2);
    assert_eq!(alpha.geometry.contours.len(), 1);
    assert_eq!(beta.geometry.contours.len(), 1);
}

#[test]
fn all_cluster_geometry_is_connected_with_intervening_tags() {
    let notes = vec![
        tagged_note("Alpha", &["alpha"]),
        tagged_note("Beta", &["beta"]),
        tagged_note("Gamma", &["gamma"]),
        tagged_note("Delta", &["delta"]),
        tagged_note("Epsilon", &["epsilon"]),
        tagged_note("Alpha Gamma Bridge", &["alpha", "gamma"]),
    ];
    let layout = clustered_layout(&index(notes));

    assert!(layout.stats.connectivity_passes <= 32);
    for cluster in &layout.clusters {
        assert_eq!(
            cluster.geometry.contours.len(),
            1,
            "cluster {} is disconnected: notes={} radius={} bounds={:?} vertices={} indices={} step={}",
            cluster.name,
            cluster.note_count,
            cluster.influence_radius,
            cluster.bounds(),
            cluster.geometry.vertices.len(),
            cluster.geometry.indices.len(),
            layout.stats.field_step,
        );
    }
}

#[test]
fn exact_normalized_and_nested_tags_are_distinct_memberships() {
    let layout = clustered_layout(&index(vec![tagged_note(
        "Tagged",
        &["#Methods", " methods ", "methods/stats"],
    )]));
    assert_eq!(layout.clusters.len(), 2);
    assert_eq!(
        layout
            .clusters
            .iter()
            .map(|cluster| cluster.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Methods", "methods/stats"]
    );
    let fingerprint = &layout.fingerprints[&NoteId(PathBuf::from("Tagged.md"))];
    assert_eq!(fingerprint.tags, vec!["methods", "methods/stats"]);
}

#[test]
fn visual_overlap_does_not_change_cluster_membership_counts() {
    let layout = clustered_layout(&index(vec![
        tagged_note("Alpha", &["alpha"]),
        tagged_note("Beta", &["beta"]),
        tagged_note("Gamma", &["gamma"]),
        tagged_note("Alpha Beta Bridge", &["alpha", "beta"]),
    ]));
    let cluster = |name| {
        layout
            .clusters
            .iter()
            .find(|cluster| cluster.name == name)
            .expect("named cluster")
    };

    assert_eq!(cluster("alpha").note_count, 2);
    assert_eq!(cluster("beta").note_count, 2);
    assert_eq!(cluster("gamma").note_count, 1);
    assert!(
        layout
            .clusters
            .iter()
            .all(|cluster| cluster.geometry.contours.len() == 1)
    );
}

#[test]
fn relationships_affect_static_cluster_order() {
    let left = tagged_note("Left", &["alpha"]);
    let middle = tagged_note("Middle", &["beta"]);
    let right = tagged_note("Right", &["gamma"]);
    let unrelated = clustered_layout(&index(vec![left.clone(), middle.clone(), right.clone()]));

    let mut linked_left = left;
    let mut linked_right = right;
    linked_left.references.push(linked_right.id.clone());
    linked_right.backlinks.push(linked_left.id.clone());
    let linked = clustered_layout(&index(vec![linked_left, middle, linked_right]));

    let center = |layout: &super::BoardLayout, name: &str| {
        layout
            .clusters
            .iter()
            .find(|cluster| cluster.name == name)
            .expect("named cluster")
            .bounds()
            .center()
    };
    let unrelated_distance = center(&unrelated, "alpha").distance(center(&unrelated, "gamma"));
    let linked_distance = center(&linked, "alpha").distance(center(&linked, "gamma"));

    assert!(linked_distance < unrelated_distance);
}

#[test]
fn clustered_notes_do_not_overlap_at_any_natural_zoom_level() {
    let notes = (0..18)
        .map(|index| {
            if index % 3 == 0 {
                tagged_note(&format!("Bridge {index}"), &["alpha", "beta"])
            } else if index % 2 == 0 {
                tagged_note(&format!("Alpha {index}"), &["alpha"])
            } else {
                tagged_note(&format!("Beta {index}"), &["beta"])
            }
        })
        .collect();
    let layout = clustered_layout(&index(notes));

    for scale in [0.10, 0.28, 0.50, 0.74, 1.0, 4.0] {
        let level = detail_level(scale);
        let size = match level {
            DetailLevel::Markers => Vec2::splat(14.0),
            DetailLevel::Titles => title_card_size(scale),
            DetailLevel::Content => super::content_card_size(scale),
        };
        let rects: Vec<_> = layout
            .positions
            .values()
            .map(|position| Rect::from_center_size(Pos2::ZERO + position.to_vec2() * scale, size))
            .collect();
        for (index, left) in rects.iter().enumerate() {
            for right in rects.iter().skip(index + 1) {
                assert!(!left.intersects(*right), "notes overlap at scale {scale}");
            }
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
    let index = index(vec![source, target]);
    let positions = clustered_layout(&index).positions;
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
    install_cold_layout(&mut board, &index);
    board.fitted = true;
    board.select_note(Some(&selected_id));
    board.camera.scale = 1_000.0 * SNAP_THRESHOLD / CARD_SIZE.x;
    let pre_snap_camera = board.camera;

    let context = eframe::egui::Context::default();
    let input = eframe::egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0))),
        ..eframe::egui::RawInput::default()
    };
    let _ = context.run_ui(input, |ui| {
        board.show(ui, &index, Some(&selected_id), true);
    });
    assert_eq!(board.snapped_note.as_ref(), Some(&selected_id));
    assert_eq!(board.camera.center, pre_snap_camera.center);
    assert_eq!(board.camera.scale, pre_snap_camera.scale);

    board.select_note(None);
    assert_eq!(board.snapped_note.as_ref(), Some(&selected_id));

    let _ = context.run_ui(eframe::egui::RawInput::default(), |ui| {
        board.show(ui, &index, Some(&selected_id), true);
    });

    let snapped_scale = board.camera.scale;
    let zoom_out = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0))),
        events: vec![Event::Zoom(0.8)],
        ..RawInput::default()
    };
    let _ = context.run_ui(zoom_out, |ui| {
        board.show(ui, &index, Some(&selected_id), true);
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
    install_cold_layout(&mut board, &index);
    board.fitted = true;

    let context = eframe::egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
    let pointer = screen.center();
    let mut selection = None;
    let _ = context.run_ui(pointer_input(screen, pointer, true), |ui| {
        board.show(ui, &index, None, true);
    });
    let _ = context.run_ui(pointer_input(screen, pointer, false), |ui| {
        selection = board.show(ui, &index, None, true);
    });

    assert!(matches!(selection, Some(SelectionRequest::Select(ref id)) if *id == selected_id));
    board.select_note(Some(&selected_id));
    assert!(board.relationship_panel_open);
}

#[test]
fn clicking_the_selected_note_requests_deselection() {
    let selected = note("Selected");
    let selected_id = selected.id.clone();

    assert!(matches!(
        SelectionRequest::toggled(Some(selected_id.clone()), Some(&selected_id)),
        SelectionRequest::Clear
    ));
    assert!(matches!(
        SelectionRequest::toggled(Some(selected_id.clone()), None),
        SelectionRequest::Select(id) if id == selected_id
    ));
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
    install_cold_layout(&mut board, &index);
    board.fitted = true;
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
        board.show(ui, &index, Some(&source_id), true);
    });
    let _ = context.run_ui(pointer_input(screen, pointer, false), |ui| {
        selection = board.show(ui, &index, Some(&source_id), true);
    });

    assert!(matches!(selection, Some(SelectionRequest::Select(id)) if id == target_id));
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
    install_cold_layout(&mut board, &index);
    board.fitted = true;
    board.select_note(Some(&selected_id));

    let context = eframe::egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 700.0));
    board.camera.scale = screen.width() * SNAP_THRESHOLD / CARD_SIZE.x;
    board.snapped_note = Some(selected_id.clone());
    board.relationship_panel_open = false;

    let note_body = screen.center();
    let mut body_selection = None;
    let _ = context.run_ui(pointer_input(screen, note_body, true), |ui| {
        board.show(ui, &index, None, true);
    });
    let _ = context.run_ui(pointer_input(screen, note_body, false), |ui| {
        body_selection = board.show(ui, &index, None, true);
    });
    assert!(body_selection.is_none());
    assert!(!board.relationship_panel_open);

    let relationship_counts = Pos2::new(900.0, 660.0);
    let _ = context.run_ui(pointer_input(screen, relationship_counts, true), |ui| {
        board.show(ui, &index, Some(&selected_id), true);
    });
    let _ = context.run_ui(pointer_input(screen, relationship_counts, false), |ui| {
        board.show(ui, &index, Some(&selected_id), true);
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
    install_cold_layout(&mut board, &index);
    board.fitted = true;

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
        board.show(ui, &index, None, true);
    });

    assert_eq!(board.snapped_note.as_ref(), Some(&selected_id));
    assert_eq!(board.camera.center, Pos2::ZERO);
    assert!((card_width_ratio(board.camera.scale, screen) - SNAP_THRESHOLD).abs() < 0.001);
}
