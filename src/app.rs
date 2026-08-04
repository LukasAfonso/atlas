use std::{
    fs,
    path::{Path, PathBuf},
};

use eframe::egui::{self, Color32, Pos2, RichText, Stroke};
use lucide_icons::Icon;
mod jobs;
mod registry;
mod search;

use jobs::{FolderPickerJob, ScanJob};
use registry::PersistedState;
use search::SearchState;

use crate::board::{BoardState, initialize_cluster_renderer};
use crate::theme;
use crate::vault::{DiagnosticSeverity, NoteId, VaultIndex};

const SETTINGS_KEY: &str = "atlas.settings.v1";

#[derive(Debug)]
enum AppScreen {
    VaultList,
    Scanning { root: PathBuf, generation: u64 },
    Vault { index: VaultIndex },
}

#[derive(Debug)]
enum UiAction {
    AddVault,
    OpenVault(PathBuf),
    ForgetVault(PathBuf),
    CancelScan,
    BackToVaults,
    SetSelection(Option<NoteId>),
    FocusNote(NoteId),
}

pub struct AtlasApp {
    persisted: PersistedState,
    screen: AppScreen,
    scan_generation: u64,
    scan_job: Option<ScanJob>,
    folder_picker_job: Option<FolderPickerJob>,
    board: BoardState,
    selected_note: Option<NoteId>,
    notice: Option<(DiagnosticSeverity, String)>,
    search: SearchState,
}

impl AtlasApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        let mut persisted: PersistedState = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, SETTINGS_KEY))
            .unwrap_or_default();
        persisted.deduplicate();

        theme::apply(&creation_context.egui_ctx);
        initialize_cluster_renderer(creation_context);

        Self {
            persisted,
            screen: AppScreen::VaultList,
            scan_generation: 0,
            scan_job: None,
            folder_picker_job: None,
            board: BoardState::default(),
            selected_note: None,
            notice: None,
            search: SearchState::default(),
        }
    }

    fn apply_action(&mut self, action: UiAction, context: &egui::Context) {
        match action {
            UiAction::AddVault => self.start_folder_picker(context),
            UiAction::OpenVault(path) => {
                self.notice = None;
                self.start_scan(path, context, false);
            }
            UiAction::ForgetVault(path) => {
                self.persisted.forget(&path);
                self.notice = Some((
                    DiagnosticSeverity::Warning,
                    format!("Forgot {}. No files were changed.", path.display()),
                ));
            }
            UiAction::CancelScan => self.cancel_scan(),
            UiAction::BackToVaults => {
                self.selected_note = None;
                self.screen = AppScreen::VaultList;
            }
            UiAction::SetSelection(note_id) => {
                self.board.select_note(note_id.as_ref());
                self.selected_note = note_id;
                context.request_repaint();
            }
            UiAction::FocusNote(note_id) => {
                self.board.focus_on_note(&note_id);
                self.selected_note = Some(note_id);
                context.request_repaint();
            }
        }
    }

    fn render_vault_list(&self, ui: &mut egui::Ui) -> Vec<UiAction> {
        let mut actions = Vec::new();
        let picking_folder = self.folder_picker_job.is_some();

        ui.add_space(18.0);
        ui.horizontal(|ui| {
            render_brand(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if picking_folder {
                    "Choosing…"
                } else {
                    "+ Add vault"
                };
                if ui
                    .add_enabled(!picking_folder, primary_button(label))
                    .clicked()
                {
                    actions.push(UiAction::AddVault);
                }
            });
        });
        ui.add_space(32.0);
        ui.label(RichText::new("Markdown vaults").size(20.0).strong());
        ui.label(
            RichText::new("Choose a research workspace to open on the Atlas board.")
                .color(theme::MUTED),
        );
        ui.add_space(16.0);

        if self.persisted.vaults.is_empty() {
            render_empty_vault_list(ui);
        }
        for path in &self.persisted.vaults {
            actions.extend(render_vault_row(ui, path));
            ui.add_space(9.0);
        }
        actions
    }
}

fn render_scanning(ui: &mut egui::Ui, root: &Path, generation: u64) -> Vec<UiAction> {
    let mut actions = Vec::new();
    ui.centered_and_justified(|ui| {
        surface_frame().show(ui, |ui| {
            ui.set_min_width(420.0);
            ui.vertical_centered(|ui| {
                ui.add_space(14.0);
                ui.spinner();
                ui.label(RichText::new("Scanning vault").size(20.0).strong());
                ui.label(RichText::new(root.display().to_string()).color(theme::MUTED));
                ui.label(
                    RichText::new(format!("INDEX PASS {generation}"))
                        .size(10.0)
                        .color(theme::MUTED),
                );
                ui.add_space(12.0);
                if ui.add(quiet_button("Cancel")).clicked() {
                    actions.push(UiAction::CancelScan);
                }
                ui.add_space(8.0);
            });
        });
    });
    actions
}

fn render_vault(
    ui: &mut egui::Ui,
    index: &VaultIndex,
    board: &mut BoardState,
    selected_note: Option<&NoteId>,
    search: &mut SearchState,
) -> Vec<UiAction> {
    let mut actions = Vec::new();

    ui.horizontal(|ui| {
        identity_pill(ui, index);
        ui.add_space(8.0);
        if nav_group(ui, &[(Icon::ArrowLeft, "")]) == Some(0) {
            actions.push(UiAction::BackToVaults);
        }
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| match nav_group(ui, &[(Icon::Minus, ""), (Icon::Home, ""), (Icon::Plus, "")]) {
                Some(0) => board.zoom_by(1.0 / 1.2),
                Some(1) => board.request_fit(),
                Some(2) => board.zoom_by(1.2),
                _ => {}
            },
        );
    });
    ui.add_space(8.0);

    if index.notes.is_empty() {
        render_empty_vault(ui);
    } else if let Some(request) = board.show(ui, index, selected_note, !search.is_open()) {
        actions.push(UiAction::SetSelection(request.into_selection()));
    }

    render_stats_overlay(ui.ctx(), index);
    render_search_button(ui.ctx(), search);

    if let Some(note_id) = search::render(ui.ctx(), search, index) {
        actions.push(UiAction::FocusNote(note_id));
    }

    actions
}

fn identity_pill(ui: &mut egui::Ui, index: &VaultIndex) {
    egui::Frame::new()
        .fill(theme::PAPER)
        .stroke(Stroke::new(1.0, theme::LINE))
        .corner_radius(14)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                badge(
                    ui,
                    theme::SAGE_DARK,
                    RichText::new("A").strong().color(Color32::WHITE),
                );
                ui.vertical(|ui| {
                    ui.label(RichText::new(vault_name(&index.root)).size(15.0).strong());
                    ui.label(
                        RichText::new(index.root.display().to_string())
                            .small()
                            .color(theme::MUTED),
                    );
                });
            });
        });
}

fn render_search_button(ctx: &egui::Context, search: &mut SearchState) {
    let screen = ctx.content_rect();
    let position = Pos2::new(screen.center().x, screen.top() + 18.0);
    egui::Area::new(egui::Id::new("atlas.search-button"))
        .order(egui::Order::Foreground)
        .fixed_pos(position)
        .pivot(egui::Align2::CENTER_TOP)
        .show(ctx, |ui| {
            let response = egui::Frame::new()
                .fill(theme::PAPER)
                .stroke(Stroke::new(1.0, theme::LINE))
                .corner_radius(14)
                .inner_margin(egui::vec2(14.0, 9.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::icon(Icon::Search, 15.0, theme::MUTED));
                        ui.label(RichText::new("Search notes…").color(theme::MUTED));
                    });
                })
                .response;
            let click = ui.interact(
                response.rect,
                ui.id().with("search-button-click"),
                egui::Sense::click(),
            );
            if click.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if click.clicked() {
                search.open();
            }
        });
}

fn render_stats_overlay(ctx: &egui::Context, index: &VaultIndex) {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("atlas.stats"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.left_bottom() + egui::vec2(16.0, -16.0))
        .pivot(egui::Align2::LEFT_BOTTOM)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::PAPER)
                .stroke(Stroke::new(1.0, theme::LINE))
                .corner_radius(10)
                .inner_margin(egui::vec2(10.0, 7.0))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(vault_summary(index))
                            .size(10.0)
                            .color(theme::MUTED),
                    );
                });
        });
}

fn nav_group(ui: &mut egui::Ui, buttons: &[(Icon, &str)]) -> Option<usize> {
    let mut clicked = None;
    egui::Frame::new()
        .fill(theme::PAPER)
        .stroke(Stroke::new(1.0, theme::LINE))
        .corner_radius(11)
        .inner_margin(2)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (index, (icon, label)) in buttons.iter().enumerate() {
                    if index > 0 {
                        ui.add_space(2.0);
                        ui.separator();
                        ui.add_space(2.0);
                    }
                    let button =
                        egui::Button::new(theme::icon_and_label(*icon, label, 15.0, theme::INK))
                            .frame(false)
                            .corner_radius(9)
                            .min_size(egui::vec2(38.0, 38.0));
                    if ui.add(button).clicked() {
                        clicked = Some(index);
                    }
                }
            });
        });
    clicked
}

fn render_empty_vault(ui: &mut egui::Ui) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("This vault has no Markdown notes")
                    .size(20.0)
                    .strong(),
            );
            ui.label(
                RichText::new("Add a .md file and rescan when you are ready.").color(theme::MUTED),
            );
        });
    });
}

impl eframe::App for AtlasApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_folder_picker(context);
        self.poll_scan(context);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let actions = egui::CentralPanel::default()
            .show(ui, |ui| {
                if let Some((severity, message)) = &self.notice {
                    render_notice(ui, *severity, message);
                }

                match &self.screen {
                    AppScreen::VaultList => self.render_vault_list(ui),
                    AppScreen::Scanning { root, generation } => {
                        render_scanning(ui, root, *generation)
                    }
                    AppScreen::Vault { index } => render_vault(
                        ui,
                        index,
                        &mut self.board,
                        self.selected_note.as_ref(),
                        &mut self.search,
                    ),
                }
            })
            .inner;

        for action in actions {
            self.apply_action(action, ui.ctx());
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.persisted);
    }
}

impl Drop for AtlasApp {
    fn drop(&mut self) {
        self.cancel_running_scan();
    }
}

fn vault_summary(index: &VaultIndex) -> String {
    let (warnings, errors) = index.diagnostic_counts();
    format!(
        "{} notes  ·  {} references  ·  {} backlinks  ·  {} citations  ·  {} issues",
        index.notes.len(),
        index.reference_count(),
        index.backlink_count(),
        index.unique_citation_count(),
        warnings + errors,
    )
}

fn vault_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Vault")
        .to_owned()
}

fn render_notice(ui: &mut egui::Ui, severity: DiagnosticSeverity, message: &str) {
    let color = match severity {
        DiagnosticSeverity::Warning => theme::AMBER,
        DiagnosticSeverity::Error => theme::ERROR,
    };
    egui::Frame::new()
        .fill(theme::PAPER)
        .stroke(Stroke::new(1.0, color))
        .corner_radius(8)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.label(RichText::new(message).color(color));
        });
    ui.add_space(8.0);
}

fn render_empty_vault_list(ui: &mut egui::Ui) {
    surface_frame().show(ui, |ui| {
        ui.set_min_height(150.0);
        ui.vertical_centered(|ui| {
            ui.add_space(32.0);
            ui.label(RichText::new("No vaults yet").size(17.0).strong());
            ui.label(
                RichText::new("Add a folder containing Markdown notes to begin.")
                    .color(theme::MUTED),
            );
        });
    });
}

fn render_vault_row(ui: &mut egui::Ui, path: &Path) -> Vec<UiAction> {
    let accessible = fs::read_dir(path).is_ok();
    let (badge_fill, accent, status) = if accessible {
        (theme::SAGE_SOFT, theme::SAGE_DARK, "●  Available")
    } else {
        (
            Color32::from_rgb(239, 224, 220),
            theme::ERROR,
            "●  Unavailable",
        )
    };

    let mut actions = Vec::new();
    surface_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            badge(
                ui,
                badge_fill,
                RichText::new("V").size(16.0).strong().color(accent),
            );
            ui.vertical(|ui| {
                ui.label(RichText::new(vault_name(path)).size(15.0).strong());
                ui.label(
                    RichText::new(path.display().to_string())
                        .small()
                        .color(theme::MUTED),
                );
                ui.label(RichText::new(status).small().color(if accessible {
                    theme::SAGE
                } else {
                    theme::ERROR
                }));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(quiet_button("Forget")).clicked() {
                    actions.push(UiAction::ForgetVault(path.to_path_buf()));
                }
                if ui
                    .add_enabled(accessible, egui::Button::new("Open").corner_radius(9))
                    .clicked()
                {
                    actions.push(UiAction::OpenVault(path.to_path_buf()));
                }
            });
        });
    });
    actions
}

fn badge(ui: &mut egui::Ui, fill: Color32, text: RichText) {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(10)
        .inner_margin(9)
        .show(ui, |ui| {
            ui.label(text);
        });
}

fn render_brand(ui: &mut egui::Ui) {
    surface_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            badge(
                ui,
                theme::SAGE_DARK,
                RichText::new("A").size(18.0).strong().color(Color32::WHITE),
            );
            ui.vertical(|ui| {
                ui.label(RichText::new("Atlas").size(16.0).strong());
                ui.label(
                    RichText::new("RESEARCH WORKSPACE")
                        .size(9.0)
                        .color(theme::MUTED),
                );
            });
        });
    });
}

fn surface_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::PAPER)
        .stroke(Stroke::new(1.0, theme::LINE))
        .corner_radius(14)
        .inner_margin(12)
}

fn primary_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).strong().color(Color32::WHITE))
        .fill(theme::SAGE_DARK)
        .stroke(Stroke::new(1.0, theme::SAGE_DARK))
        .corner_radius(10)
}

fn quiet_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(label)
        .fill(theme::PAPER)
        .stroke(Stroke::new(1.0, theme::LINE))
        .corner_radius(9)
}
