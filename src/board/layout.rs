use std::{collections::HashMap, path::PathBuf};

use eframe::egui::{Pos2, Rect};

use super::{CARD_SIZE, metaballs::ClusterGeometry};
use crate::vault::{NoteId, VaultIndex};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BoardLayout {
    pub(super) positions: HashMap<NoteId, Pos2>,
    pub(super) clusters: Vec<ClusterRegion>,
    pub(super) edges: Vec<BoardEdge>,
    pub(super) stats: LayoutStats,
    pub(super) fingerprints: HashMap<NoteId, PlacementFingerprint>,
    pub(super) content_bounds: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClusterRegion {
    pub(super) key: String,
    pub(super) name: String,
    pub(super) bounds: Rect,
    pub(super) label_anchor: Pos2,
    pub(super) note_count: usize,
    pub(super) geometry: ClusterGeometry,
    pub(super) influence_radius: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct LayoutStats {
    pub(super) connectivity_passes: usize,
    pub(super) fallback_cluster_count: usize,
    pub(super) maximum_influence_radius: f32,
    pub(super) sampled_field_cells: usize,
    pub(super) field_step: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PlacementFingerprint {
    pub(super) tags: Vec<String>,
    pub(super) related: Vec<NoteId>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LayoutSeed {
    pub(super) root: PathBuf,
    pub(super) positions: HashMap<NoteId, Pos2>,
    pub(super) fingerprints: HashMap<NoteId, PlacementFingerprint>,
}

pub(super) fn layout_bounds(
    positions: &HashMap<NoteId, Pos2>,
    clusters: &[ClusterRegion],
) -> Option<Rect> {
    let note_bounds = positions
        .values()
        .copied()
        .map(|position| Rect::from_center_size(position, CARD_SIZE))
        .reduce(Rect::union);
    clusters
        .iter()
        .map(|cluster| cluster.bounds)
        .chain(note_bounds)
        .reduce(Rect::union)
}

pub(super) fn resolved_edges(
    index: &VaultIndex,
    positions: &HashMap<NoteId, Pos2>,
) -> Vec<BoardEdge> {
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
pub(super) struct BoardEdge {
    pub(super) source: NoteId,
    pub(super) target: NoteId,
    pub(super) source_position: Pos2,
    pub(super) target_position: Pos2,
}
