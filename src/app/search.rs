use eframe::egui::{
    self, Color32, CursorIcon, Frame, Id, Key, Order, Pos2, Rect, RichText, Sense, Stroke,
    TextEdit, Vec2,
};

use crate::theme;
use crate::vault::{NoteId, VaultIndex};

const MAX_RESULTS: usize = 8;
const SNIPPET_CHARS: usize = 92;
const MODAL_WIDTH: f32 = 560.0;

#[derive(Default)]
pub(super) struct SearchState {
    open: bool,
    query: String,
    active: usize,
    focus_requested: bool,
}

impl SearchState {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn open(&mut self) {
        self.open = true;
        self.focus_requested = true;
    }

    fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.active = 0;
    }
}

struct Hit<'a> {
    id: &'a NoteId,
    title: &'a str,
    snippet: String,
    score: u8,
}

fn search<'a>(index: &'a VaultIndex, query: &str) -> Vec<Hit<'a>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<_> = index
        .notes
        .iter()
        .filter_map(|note| {
            let title_hit = note.title.to_lowercase().contains(&needle);
            let tag_hit = note
                .tags
                .iter()
                .any(|tag| tag.to_lowercase().contains(&needle));
            let body_hit = note.markdown_body.to_lowercase().contains(&needle);
            if !title_hit && !tag_hit && !body_hit {
                return None;
            }
            let score = u8::from(title_hit) * 4 + u8::from(tag_hit) * 2 + u8::from(body_hit);
            Some(Hit {
                id: &note.id,
                title: note.title.as_str(),
                snippet: snippet(&note.markdown_body, &needle),
                score,
            })
        })
        .collect();
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
    hits.truncate(MAX_RESULTS);
    hits
}

fn snippet(body: &str, needle: &str) -> String {
    let start = body.to_lowercase().find(needle).unwrap_or(0);
    let clipped = body.get(start..).unwrap_or(body);
    clipped
        .chars()
        .take(SNIPPET_CHARS)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn render(
    ctx: &egui::Context,
    state: &mut SearchState,
    index: &VaultIndex,
) -> Option<NoteId> {
    if !state.open {
        let shortcut = !ctx.egui_wants_keyboard_input()
            && ctx.input(|input| {
                (input.modifiers.command && input.key_pressed(Key::K))
                    || input.key_pressed(Key::Slash)
            });
        if shortcut {
            state.open();
        }
    }
    if !state.open {
        return None;
    }

    let screen = ctx.content_rect();
    let hits = search(index, &state.query);
    if !hits.is_empty() {
        state.active = state.active.min(hits.len() - 1);
    }

    let mut chosen = None;
    ctx.input(|input| {
        if !hits.is_empty() {
            if input.key_pressed(Key::ArrowDown) {
                state.active = (state.active + 1) % hits.len();
            }
            if input.key_pressed(Key::ArrowUp) {
                state.active = (state.active + hits.len() - 1) % hits.len();
            }
        }
        if input.key_pressed(Key::Enter) && !hits.is_empty() {
            chosen = Some(hits[state.active].id.clone());
        }
    });
    let escape_pressed = ctx.input(|input| input.key_pressed(Key::Escape));

    let panel_width = MODAL_WIDTH.min(screen.width() - 96.0);
    let panel_pos = Pos2::new(
        screen.center().x - panel_width / 2.0,
        screen.top() + screen.height() * 0.14,
    );
    let panel_max_rect = Rect::from_min_size(
        panel_pos,
        Vec2::new(panel_width, screen.bottom() - panel_pos.y - 40.0),
    );
    let results_height = (panel_max_rect.height() - 90.0).max(200.0);

    let mut panel_rect = panel_max_rect;
    egui::Area::new(Id::new("atlas.search.modal"))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(90));
            let panel = ui.scope_builder(egui::UiBuilder::new().max_rect(panel_max_rect), |ui| {
                Frame::new()
                    .fill(theme::PAPER_STRONG)
                    .stroke(Stroke::new(1.0, theme::LINE))
                    .corner_radius(16)
                    .inner_margin(14)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("⌕").color(theme::MUTED));
                            let edit = ui.add(
                                TextEdit::singleline(&mut state.query)
                                    .hint_text("Find a note, tag, or phrase…")
                                    .desired_width(ui.available_width())
                                    .frame(Frame::NONE),
                            );
                            if state.focus_requested {
                                edit.request_focus();
                                state.focus_requested = false;
                            }
                        });
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);

                        egui::ScrollArea::vertical()
                            .max_height(results_height)
                            .show(ui, |ui| {
                                if state.query.is_empty() {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new("Start typing to search this vault")
                                            .color(theme::MUTED),
                                    );
                                    ui.add_space(4.0);
                                    return;
                                }
                                if hits.is_empty() {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new("No matching notes").color(theme::MUTED),
                                    );
                                    ui.add_space(4.0);
                                    return;
                                }
                                for (row, hit) in hits.iter().enumerate() {
                                    let active = row == state.active;
                                    let response = Frame::new()
                                        .corner_radius(8)
                                        .inner_margin(Vec2::new(9.0, 7.0))
                                        .fill(if active {
                                            theme::SAGE_SOFT
                                        } else {
                                            Color32::TRANSPARENT
                                        })
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    RichText::new(hit.title).strong().size(13.0),
                                                );
                                                if !hit.snippet.is_empty() {
                                                    ui.label(
                                                        RichText::new(format!("{}…", hit.snippet))
                                                            .small()
                                                            .color(theme::MUTED),
                                                    );
                                                }
                                            });
                                        })
                                        .response;
                                    let row_response = ui.interact(
                                        response.rect,
                                        ui.id().with(("search-result", row)),
                                        Sense::click(),
                                    );
                                    if row_response.hovered() {
                                        state.active = row;
                                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    }
                                    if row_response.clicked() {
                                        chosen = Some(hit.id.clone());
                                    }
                                }
                            });
                    });
            });
            panel_rect = panel.response.rect;
        });

    let click_outside = ctx.input(|input| {
        input.pointer.primary_clicked()
            && input
                .pointer
                .interact_pos()
                .is_some_and(|position| !panel_rect.contains(position))
    });

    if chosen.is_some() || click_outside || escape_pressed {
        state.close();
    }
    chosen
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{MAX_RESULTS, search, snippet};
    use crate::vault::{NoteId, NoteRecord, VaultIndex};

    fn note(name: &str, body: &str, tags: &[&str]) -> NoteRecord {
        let path = PathBuf::from(format!("{name}.md"));
        NoteRecord {
            id: NoteId(path.clone()),
            relative_path: path,
            title: name.to_owned(),
            markdown_body: body.to_owned(),
            aliases: Vec::new(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            references: Vec::new(),
            backlinks: Vec::new(),
            citations: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn index(notes: Vec<NoteRecord>) -> VaultIndex {
        VaultIndex {
            root: PathBuf::from("/vault"),
            notes,
            diagnostics: Vec::new(),
            scan_duration: std::time::Duration::ZERO,
        }
    }

    #[test]
    fn title_matches_rank_above_body_only_matches() {
        let vault = index(vec![
            note("Body note", "the target-trial protocol appears here", &[]),
            note("Target trial emulation", "unrelated content", &[]),
        ]);
        let hits = search(&vault, "target-trial");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Body note");
    }

    #[test]
    fn title_matches_outrank_body_matches_when_both_present() {
        let vault = index(vec![
            note("Unrelated", "mentions causal in passing", &[]),
            note("Causal representation", "distinct body content", &[]),
        ]);
        let hits = search(&vault, "causal");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Causal representation");
    }

    #[test]
    fn tag_matches_are_found() {
        let vault = index(vec![note(
            "Note",
            "nothing relevant",
            &["causal/g-methods"],
        )]);
        let hits = search(&vault, "g-methods");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn empty_query_returns_no_hits() {
        let vault = index(vec![note("Note", "body", &[])]);
        assert!(search(&vault, "").is_empty());
        assert!(search(&vault, "   ").is_empty());
    }

    #[test]
    fn results_are_capped() {
        let notes = (0..MAX_RESULTS + 5)
            .map(|index| note(&format!("Match {index}"), "shared", &[]))
            .collect();
        let vault = index(notes);
        let hits = search(&vault, "shared");
        assert_eq!(hits.len(), MAX_RESULTS);
    }

    #[test]
    fn snippet_starts_near_the_match() {
        let body =
            "a very long preamble that goes on for a while before the actual keyword shows up";
        let result = snippet(body, "keyword");
        assert!(result.starts_with("keyword"));
    }
}
