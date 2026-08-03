use std::collections::HashMap;

use eframe::egui::{Pos2, Rect};

use super::{CARD_SIZE, GRID_SPACING};
use crate::vault::{NoteId, NoteRecord, VaultIndex};

pub(super) fn grid_layout(notes: &[NoteRecord]) -> HashMap<NoteId, Pos2> {
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

pub(super) fn layout_bounds(positions: &HashMap<NoteId, Pos2>) -> Option<Rect> {
    let mut positions = positions.values().copied();
    let first = positions.next()?;
    let mut bounds = Rect::from_center_size(first, CARD_SIZE);
    for position in positions {
        bounds = bounds.union(Rect::from_center_size(position, CARD_SIZE));
    }
    Some(bounds)
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
