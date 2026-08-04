use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

use eframe::egui::Pos2;

use super::layout::{
    BoardLayout, LayoutSeed, LayoutStats, PlacementFingerprint, layout_bounds, resolved_edges,
};
use crate::vault::{NoteId, NoteRecord, VaultIndex};

mod connectivity;
mod fields;
mod grid;
mod slots;
mod tags;

const UNTAGGED_KEY: &str = "\0atlas-untagged";
const PRESERVED_WEIGHT: f64 = 32.0;
const NEW_WEIGHT: f64 = 1.0;

type NoteTags = HashMap<NoteId, Vec<(String, String)>>;
type ClusterMembers = BTreeMap<String, Vec<NoteId>>;
type TagWeights = BTreeMap<(String, String), u32>;

pub(crate) fn prepare_board_layout(index: &VaultIndex, seed: Option<&LayoutSeed>) -> BoardLayout {
    if index.notes.is_empty() {
        return BoardLayout::default();
    }

    let mut notes: Vec<_> = index.notes.iter().collect();
    notes.sort_by_key(|note| note_sort_key(note));
    let note_tags: NoteTags = notes
        .iter()
        .map(|note| (note.id.clone(), normalized_tags(note)))
        .collect();
    let fingerprints: HashMap<_, _> = notes
        .iter()
        .map(|note| {
            (
                note.id.clone(),
                placement_fingerprint(note, &note_tags[&note.id]),
            )
        })
        .collect();
    let seeding = Seeding::new(seed, &index.root, &fingerprints, notes.len());

    let members = tags::cluster_members(&notes, &note_tags);
    let (mut positions, connectivity_passes) =
        solve_positions(&notes, &note_tags, &members, &seeding);
    let fields = fields::resolve_cluster_fields(&notes, &note_tags, &members, &mut positions);
    let (clusters, sampled_field_cells) =
        fields::build_cluster_regions(&notes, &note_tags, &members, &positions, &fields);

    BoardLayout {
        edges: resolved_edges(index, &positions),
        content_bounds: layout_bounds(&positions, &clusters),
        stats: LayoutStats {
            connectivity_passes,
            fallback_cluster_count: fields.fallback_tags.len(),
            maximum_influence_radius: fields.radii.values().copied().fold(0.0, f32::max),
            sampled_field_cells,
            field_step: fields.step,
        },
        positions,
        clusters,
        fingerprints,
    }
}

fn solve_positions(
    notes: &[&NoteRecord],
    note_tags: &NoteTags,
    members: &ClusterMembers,
    seeding: &Seeding<'_>,
) -> (HashMap<NoteId, Pos2>, usize) {
    if let Some(seed) = seeding.exact_rescan() {
        return (seed.positions.clone(), 0);
    }

    let tag_weights = tags::tag_relationship_weights(notes, note_tags);
    let tag_centers = tags::tag_centers(members, &tag_weights, seeding);
    let desired = slots::desired_note_slots(notes, note_tags, &tag_centers, seeding);
    let spread = slots::spread_shared_anchors(notes, note_tags, desired.clone(), seeding);
    let initial = grid::slots_to_positions(slots::pack_rows(notes, &spread, seeding));

    connectivity::refine_connectivity(
        notes,
        members,
        initial,
        &grid::slots_to_positions(desired),
        seeding,
    )
}

struct Seeding<'a> {
    seed: Option<&'a LayoutSeed>,
    unchanged: HashSet<NoteId>,
    is_exact_rescan: bool,
}

impl<'a> Seeding<'a> {
    fn new(
        seed: Option<&'a LayoutSeed>,
        root: &Path,
        fingerprints: &HashMap<NoteId, PlacementFingerprint>,
        note_count: usize,
    ) -> Self {
        let seed = seed.filter(|seed| seed.root == root);
        let unchanged: HashSet<_> = seed.map_or_else(HashSet::new, |seed| {
            seed.fingerprints
                .iter()
                .filter(|(note_id, fingerprint)| {
                    fingerprints.get(*note_id) == Some(*fingerprint)
                        && seed.positions.contains_key(*note_id)
                })
                .map(|(note_id, _)| note_id.clone())
                .collect()
        });
        let is_exact_rescan = seed.is_some_and(|seed| {
            seed.positions.len() == note_count && unchanged.len() == note_count
        });
        Self {
            seed,
            unchanged,
            is_exact_rescan,
        }
    }

    fn exact_rescan(&self) -> Option<&LayoutSeed> {
        self.seed.filter(|_| self.is_exact_rescan)
    }

    fn is_unchanged(&self, note_id: &NoteId) -> bool {
        self.unchanged.contains(note_id)
    }

    fn preserved_position(&self, note_id: &NoteId) -> Option<Pos2> {
        self.is_unchanged(note_id)
            .then(|| self.seed?.positions.get(note_id).copied())
            .flatten()
    }

    fn weight(&self, note_id: &NoteId) -> f64 {
        if self.is_unchanged(note_id) {
            PRESERVED_WEIGHT
        } else {
            NEW_WEIGHT
        }
    }
}

fn normalized_tags(note: &NoteRecord) -> Vec<(String, String)> {
    let mut tags = BTreeMap::new();
    for tag in &note.tags {
        let display = tag.trim().trim_start_matches('#');
        if !display.is_empty() {
            tags.entry(display.to_lowercase())
                .or_insert_with(|| display.to_owned());
        }
    }
    if tags.is_empty() {
        vec![(UNTAGGED_KEY.to_owned(), "Untagged".to_owned())]
    } else {
        tags.into_iter().collect()
    }
}

fn placement_fingerprint(note: &NoteRecord, tags: &[(String, String)]) -> PlacementFingerprint {
    let mut related: Vec<_> = note
        .references
        .iter()
        .chain(note.backlinks.iter())
        .cloned()
        .collect();
    related.sort();
    related.dedup();
    PlacementFingerprint {
        tags: tags.iter().map(|(key, _)| key.clone()).collect(),
        related,
    }
}

fn note_sort_key(note: &NoteRecord) -> (String, String) {
    let id = note.id.display();
    (id.to_lowercase(), id)
}
