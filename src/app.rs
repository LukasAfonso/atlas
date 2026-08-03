use std::{
    fs,
    path::{Path, PathBuf},
};

use eframe::egui::{self, Color32, RichText, Stroke};
mod jobs;
mod registry;

use jobs::{FolderPickerJob, ScanJob};
use registry::PersistedState;

use crate::board::BoardState;
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
    Rescan(PathBuf),
    SetSelection(Option<NoteId>),
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
}

impl AtlasApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        let mut persisted: PersistedState = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, SETTINGS_KEY))
            .unwrap_or_default();
        persisted.deduplicate();

        theme::apply(&creation_context.egui_ctx);

        Self {
            persisted,
            screen: AppScreen::VaultList,
            scan_generation: 0,
            scan_job: None,
            folder_picker_job: None,
            board: BoardState::default(),
            selected_note: None,
            notice: None,
        }
    }

    fn apply_action(&mut self, action: UiAction, context: &egui::Context) {
        match action {
            UiAction::AddVault => self.start_folder_picker(context),
            UiAction::OpenVault(path) | UiAction::Rescan(path) => {
                self.notice = None;
                self.start_scan(path, context);
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
        }
    }

    fn render_vault_list(&self, ui: &mut egui::Ui) -> Vec<UiAction> {
        let mut actions = Vec::new();
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            render_brand(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.folder_picker_job.is_none(),
                        primary_button(if self.folder_picker_job.is_some() {
                            "Choosing…"
                        } else {
                            "+ Add vault"
                        }),
                    )
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
        } else {
            for path in &self.persisted.vaults {
                let accessible = fs::read_dir(path).is_ok();
                surface_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        egui::Frame::new()
                            .fill(if accessible {
                                theme::SAGE_SOFT
                            } else {
                                Color32::from_rgb(239, 224, 220)
                            })
                            .corner_radius(10)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                ui.label(RichText::new("V").size(16.0).strong().color(
                                    if accessible {
                                        theme::SAGE_DARK
                                    } else {
                                        theme::ERROR
                                    },
                                ));
                            });
                        ui.vertical(|ui| {
                            ui.label(RichText::new(vault_name(path)).size(15.0).strong());
                            ui.label(
                                RichText::new(path.display().to_string())
                                    .small()
                                    .color(theme::MUTED),
                            );
                            if accessible {
                                ui.label(RichText::new("●  Available").small().color(theme::SAGE));
                            } else {
                                ui.label(
                                    RichText::new("●  Unavailable").small().color(theme::ERROR),
                                );
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(quiet_button("Forget")).clicked() {
                                actions.push(UiAction::ForgetVault(path.clone()));
                            }
                            if ui
                                .add_enabled(accessible, egui::Button::new("Open").corner_radius(9))
                                .clicked()
                            {
                                actions.push(UiAction::OpenVault(path.clone()));
                            }
                        });
                    });
                });
                ui.add_space(9.0);
            }
        }
        actions
    }

    fn render_scanning(&self, ui: &mut egui::Ui, root: &Path, generation: u64) -> Vec<UiAction> {
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
    ) -> Vec<UiAction> {
        let mut actions = Vec::new();

        surface_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                egui::Frame::new()
                    .fill(theme::SAGE_DARK)
                    .corner_radius(9)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.label(RichText::new("A").strong().color(Color32::WHITE));
                    });
                ui.vertical(|ui| {
                    ui.label(RichText::new(vault_name(&index.root)).size(15.0).strong());
                    ui.label(
                        RichText::new(index.root.display().to_string())
                            .small()
                            .color(theme::MUTED),
                    );
                    let (warnings, errors) = index.diagnostic_counts();
                    ui.label(
                        RichText::new(format!(
                            "{} notes  ·  {} references  ·  {} backlinks  ·  {} citations  ·  {} issues  ·  {:.0} ms",
                            index.notes.len(),
                            index.reference_count(),
                            index.backlink_count(),
                            index.unique_citation_count(),
                            warnings + errors,
                            index.scan_duration.as_secs_f64() * 1_000.0,
                        ))
                        .size(10.0)
                        .color(theme::MUTED),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(quiet_button("← Vaults")).clicked() {
                        actions.push(UiAction::BackToVaults);
                    }
                    if ui.add(quiet_button("Rescan")).clicked() {
                        actions.push(UiAction::Rescan(index.root.clone()));
                    }
                    if ui.add(quiet_button("Fit board")).clicked() {
                        board.request_fit();
                    }
                });
            });
        });
        ui.add_space(8.0);

        if index.notes.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("This vault has no Markdown notes")
                            .size(20.0)
                            .strong(),
                    );
                    ui.label(
                        RichText::new("Add a .md file and rescan when you are ready.")
                            .color(theme::MUTED),
                    );
                });
            });
        } else {
            let board_output = board.show(ui, index, selected_note);
            if let Some(selection) = board_output.selection_request {
                actions.push(UiAction::SetSelection(selection));
            }
        }

        actions
    }
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

                match &self.screen {
                    AppScreen::VaultList => self.render_vault_list(ui),
                    AppScreen::Scanning { root, generation } => {
                        self.render_scanning(ui, root, *generation)
                    }
                    AppScreen::Vault { index } => {
                        Self::render_vault(ui, index, &mut self.board, self.selected_note.as_ref())
                    }
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

fn vault_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Vault")
        .to_owned()
}

fn render_brand(ui: &mut egui::Ui) {
    surface_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            egui::Frame::new()
                .fill(theme::SAGE_DARK)
                .corner_radius(10)
                .inner_margin(9)
                .show(ui, |ui| {
                    ui.label(RichText::new("A").size(18.0).strong().color(Color32::WHITE));
                });
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
