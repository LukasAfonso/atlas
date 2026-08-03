use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::Duration,
};

use eframe::egui::{self, Color32, RichText};
use serde::{Deserialize, Serialize};

use crate::vault::{
    Diagnostic, DiagnosticSeverity, NoteId, NoteRecord, VaultIndex, VaultScanResult, scan_vault,
};

const SETTINGS_KEY: &str = "atlas.settings.v1";
const NOTE_ROW_HEIGHT: f32 = 70.0;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct PersistedState {
    vaults: Vec<PathBuf>,
}

impl PersistedState {
    fn register(&mut self, path: PathBuf) -> bool {
        if self.vaults.iter().any(|existing| existing == &path) {
            false
        } else {
            self.vaults.push(path);
            self.vaults.sort_by_key(|path| path_key(path));
            true
        }
    }

    fn forget(&mut self, path: &Path) -> bool {
        let original_len = self.vaults.len();
        self.vaults.retain(|existing| existing != path);
        self.vaults.len() != original_len
    }

    fn deduplicate(&mut self) {
        self.vaults.sort_by_key(|path| path_key(path));
        self.vaults.dedup();
    }
}

#[derive(Debug)]
enum AppScreen {
    VaultList,
    Scanning { root: PathBuf, generation: u64 },
    Vault { index: VaultIndex },
}

struct ScanEnvelope {
    generation: u64,
    result: VaultScanResult,
}

struct ScanJob {
    receiver: Receiver<ScanEnvelope>,
    cancelled: Arc<AtomicBool>,
}

struct FolderPickerJob {
    receiver: Receiver<Option<PathBuf>>,
}

#[derive(Debug)]
enum UiAction {
    AddVault,
    OpenVault(PathBuf),
    ForgetVault(PathBuf),
    CancelScan,
    BackToVaults,
    Rescan(PathBuf),
    SelectNote(NoteId),
}

pub struct AtlasApp {
    persisted: PersistedState,
    screen: AppScreen,
    scan_generation: u64,
    scan_job: Option<ScanJob>,
    folder_picker_job: Option<FolderPickerJob>,
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

        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = Color32::from_rgb(246, 246, 242);
        visuals.window_fill = Color32::from_rgb(250, 249, 245);
        visuals.selection.bg_fill = Color32::from_rgb(79, 116, 99);
        creation_context.egui_ctx.set_visuals(visuals);

        Self {
            persisted,
            screen: AppScreen::VaultList,
            scan_generation: 0,
            scan_job: None,
            folder_picker_job: None,
            selected_note: None,
            notice: None,
        }
    }

    fn start_scan(&mut self, root: PathBuf, context: &egui::Context) {
        self.cancel_running_scan();
        self.scan_generation = self.scan_generation.wrapping_add(1);
        let generation = self.scan_generation;
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, receiver) = mpsc::channel();
        let worker_context = context.clone();
        let worker_root = root.clone();

        thread::spawn(move || {
            let result = scan_vault(worker_root);
            if !worker_cancelled.load(Ordering::Acquire) {
                let _ = sender.send(ScanEnvelope { generation, result });
                worker_context.request_repaint();
            }
        });

        self.selected_note = None;
        self.scan_job = Some(ScanJob {
            receiver,
            cancelled,
        });
        self.screen = AppScreen::Scanning { root, generation };
    }

    fn cancel_running_scan(&mut self) {
        if let Some(job) = self.scan_job.take() {
            job.cancelled.store(true, Ordering::Release);
        }
    }

    fn cancel_scan(&mut self) {
        self.cancel_running_scan();
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.screen = AppScreen::VaultList;
    }

    fn poll_scan(&mut self, context: &egui::Context) {
        let message = self.scan_job.as_ref().map(|job| job.receiver.try_recv());

        match message {
            Some(Ok(envelope)) => {
                self.scan_job = None;
                if is_current_generation(self.scan_generation, envelope.generation) {
                    self.screen = AppScreen::Vault {
                        index: envelope.result.index,
                    };
                    self.selected_note = None;
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.scan_job = None;
                self.notice = Some((
                    DiagnosticSeverity::Error,
                    "The vault scan stopped unexpectedly".to_owned(),
                ));
                self.screen = AppScreen::VaultList;
            }
            Some(Err(TryRecvError::Empty)) => {
                context.request_repaint_after(Duration::from_millis(50));
            }
            None => {}
        }
    }

    fn start_folder_picker(&mut self, context: &egui::Context) {
        if self.folder_picker_job.is_some() {
            return;
        }

        let (sender, receiver) = mpsc::channel();
        let worker_context = context.clone();
        thread::spawn(move || {
            let selected = pollster::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title("Add Markdown vault")
                    .pick_folder(),
            )
            .map(|handle| handle.path().to_path_buf());
            let _ = sender.send(selected);
            worker_context.request_repaint();
        });
        self.folder_picker_job = Some(FolderPickerJob { receiver });
        self.notice = Some((
            DiagnosticSeverity::Warning,
            "Waiting for folder selection…".to_owned(),
        ));
        context.request_repaint_after(Duration::from_millis(50));
    }

    fn poll_folder_picker(&mut self, context: &egui::Context) {
        let message = self
            .folder_picker_job
            .as_ref()
            .map(|job| job.receiver.try_recv());

        match message {
            Some(Ok(selected)) => {
                self.folder_picker_job = None;
                self.accept_selected_vault(selected, context);
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.folder_picker_job = None;
                self.notice = Some((
                    DiagnosticSeverity::Error,
                    "The folder picker stopped unexpectedly".to_owned(),
                ));
            }
            Some(Err(TryRecvError::Empty)) => {
                context.request_repaint_after(Duration::from_millis(50));
            }
            None => {}
        }
    }

    fn accept_selected_vault(&mut self, selected: Option<PathBuf>, context: &egui::Context) {
        let Some(selected) = selected else {
            self.notice = None;
            return;
        };

        match fs::canonicalize(&selected) {
            Ok(path) => {
                self.persisted.register(path.clone());
                self.notice = None;
                self.start_scan(path, context);
            }
            Err(error) => {
                self.notice = Some((
                    DiagnosticSeverity::Error,
                    format!("Could not add {}: {error}", selected.display()),
                ));
            }
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
            UiAction::SelectNote(note_id) => {
                self.selected_note = if self.selected_note.as_ref() == Some(&note_id) {
                    None
                } else {
                    Some(note_id)
                };
            }
        }
    }

    fn render_vault_list(&self, ui: &mut egui::Ui) -> Vec<UiAction> {
        let mut actions = Vec::new();
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.heading("Atlas");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.folder_picker_job.is_none(),
                        egui::Button::new(if self.folder_picker_job.is_some() {
                            "Choosing…"
                        } else {
                            "Add vault"
                        }),
                    )
                    .clicked()
                {
                    actions.push(UiAction::AddVault);
                }
            });
        });
        ui.label("Your Markdown vaults");
        ui.add_space(12.0);

        if self.persisted.vaults.is_empty() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_height(120.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(25.0);
                    ui.strong("No vaults yet");
                    ui.label("Add a folder containing Markdown notes to begin.");
                });
            });
        } else {
            for path in &self.persisted.vaults {
                let accessible = fs::read_dir(path).is_ok();
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.strong(vault_name(path));
                            ui.label(RichText::new(path.display().to_string()).small().weak());
                            if accessible {
                                ui.label(
                                    RichText::new("Available")
                                        .small()
                                        .color(Color32::DARK_GREEN),
                                );
                            } else {
                                ui.label(
                                    RichText::new("Unavailable")
                                        .small()
                                        .color(Color32::DARK_RED),
                                );
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Forget").clicked() {
                                actions.push(UiAction::ForgetVault(path.clone()));
                            }
                            if ui
                                .add_enabled(accessible, egui::Button::new("Open"))
                                .clicked()
                            {
                                actions.push(UiAction::OpenVault(path.clone()));
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        }
        actions
    }

    fn render_scanning(&self, ui: &mut egui::Ui, root: &Path, generation: u64) -> Vec<UiAction> {
        let mut actions = Vec::new();
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.heading("Scanning vault");
                ui.label(root.display().to_string());
                ui.label(RichText::new(format!("Scan {generation}")).small().weak());
                ui.add_space(12.0);
                if ui.button("Cancel").clicked() {
                    actions.push(UiAction::CancelScan);
                }
            });
        });
        actions
    }

    fn render_vault(&self, ui: &mut egui::Ui, index: &VaultIndex) -> Vec<UiAction> {
        let mut actions = Vec::new();
        let (warnings, errors) = index.diagnostic_counts();

        ui.horizontal(|ui| {
            if ui.button("← Vaults").clicked() {
                actions.push(UiAction::BackToVaults);
            }
            if ui.button("Rescan").clicked() {
                actions.push(UiAction::Rescan(index.root.clone()));
            }
            ui.separator();
            ui.heading(vault_name(&index.root));
        });
        ui.label(
            RichText::new(index.root.display().to_string())
                .small()
                .weak(),
        );
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            metric(ui, "Notes", index.notes.len());
            metric(ui, "References", index.reference_count());
            metric(ui, "Backlinks", index.backlink_count());
            metric(ui, "Citations", index.unique_citation_count());
            metric(ui, "Warnings", warnings);
            metric(ui, "Errors", errors);
            ui.label(
                RichText::new(format!(
                    "{:.1} ms",
                    index.scan_duration.as_secs_f64() * 1_000.0
                ))
                .small()
                .weak(),
            );
        });
        ui.separator();

        if !index.diagnostics.is_empty() {
            egui::CollapsingHeader::new(format!("Scan diagnostics ({})", index.diagnostics.len()))
                .default_open(true)
                .show(ui, |ui| {
                    for diagnostic in &index.diagnostics {
                        diagnostic_label(ui, diagnostic);
                    }
                });
            ui.separator();
        }

        if let Some(selected) = self
            .selected_note
            .as_ref()
            .and_then(|selected| index.notes.iter().find(|note| &note.id == selected))
        {
            render_note_details(ui, selected, index);
            ui.separator();
        }

        if index.notes.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("This vault has no Markdown notes");
                    ui.label("Add a .md file and rescan when you are ready.");
                });
            });
            return actions;
        }

        let mut selected = None;
        egui::ScrollArea::vertical()
            .id_salt("vault-note-list")
            .auto_shrink([false, false])
            .show_rows(ui, NOTE_ROW_HEIGHT, index.notes.len(), |ui, row_range| {
                for row in row_range {
                    let note = &index.notes[row];
                    let is_selected = self.selected_note.as_ref() == Some(&note.id);
                    let response = ui.selectable_label(
                        is_selected,
                        RichText::new(&note.title).strong().size(15.0),
                    );
                    if response.clicked() {
                        selected = Some(note.id.clone());
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(note.relative_path.display().to_string())
                                .small()
                                .weak(),
                        );
                        for tag in &note.tags {
                            ui.label(
                                RichText::new(format!("#{tag}"))
                                    .small()
                                    .color(Color32::from_rgb(78, 111, 96)),
                            );
                        }
                    });
                    ui.label(
                        RichText::new(format!(
                            "{} references · {} backlinks · {} citations · {} diagnostics",
                            note.references.len(),
                            note.backlinks.len(),
                            note.citations.len(),
                            note.diagnostics.len()
                        ))
                        .small(),
                    );
                    ui.separator();
                }
            });

        if let Some(note_id) = selected {
            actions.push(UiAction::SelectNote(note_id));
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
                        DiagnosticSeverity::Warning => Color32::from_rgb(145, 99, 38),
                        DiagnosticSeverity::Error => Color32::DARK_RED,
                    };
                    ui.label(RichText::new(message).color(color));
                    ui.separator();
                }

                match &self.screen {
                    AppScreen::VaultList => self.render_vault_list(ui),
                    AppScreen::Scanning { root, generation } => {
                        self.render_scanning(ui, root, *generation)
                    }
                    AppScreen::Vault { index } => self.render_vault(ui, index),
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

fn render_note_details(ui: &mut egui::Ui, note: &NoteRecord, index: &VaultIndex) {
    let titles = index.note_titles();
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.strong(&note.title);
            ui.label(RichText::new(note.id.display()).small().weak());
        });
        if !note.aliases.is_empty() {
            ui.label(format!("Aliases: {}", note.aliases.join(", ")));
        }
        relationship_label(ui, "References", &note.references, &titles);
        relationship_label(ui, "Backlinks", &note.backlinks, &titles);
        if !note.citations.is_empty() {
            ui.label(format!("Citations: @{}", note.citations.join(", @")));
        }
        for diagnostic in &note.diagnostics {
            diagnostic_label(ui, diagnostic);
        }
    });
}

fn relationship_label(
    ui: &mut egui::Ui,
    label: &str,
    ids: &[NoteId],
    titles: &std::collections::HashMap<NoteId, &str>,
) {
    if ids.is_empty() {
        return;
    }
    let values = ids
        .iter()
        .map(|id| {
            titles
                .get(id)
                .map(|title| (*title).to_owned())
                .unwrap_or_else(|| id.display())
        })
        .collect::<Vec<_>>()
        .join(", ");
    ui.label(format!("{label}: {values}"));
}

fn diagnostic_label(ui: &mut egui::Ui, diagnostic: &Diagnostic) {
    let color = match diagnostic.severity {
        DiagnosticSeverity::Warning => Color32::from_rgb(145, 99, 38),
        DiagnosticSeverity::Error => Color32::DARK_RED,
    };
    let path = diagnostic
        .path
        .as_ref()
        .map(|path| format!("{}: ", path.display()))
        .unwrap_or_default();
    ui.label(
        RichText::new(format!("{path}{}", diagnostic.message))
            .small()
            .color(color),
    );
}

fn metric(ui: &mut egui::Ui, label: &str, value: usize) {
    ui.label(RichText::new(format!("{value} {label}")).small());
    ui.separator();
}

fn vault_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Vault")
        .to_owned()
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn is_current_generation(active: u64, incoming: u64) -> bool {
    active == incoming
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{PersistedState, is_current_generation};

    #[test]
    fn vault_registry_deduplicates_paths() {
        let directory = tempdir().unwrap();
        let path = fs::canonicalize(directory.path()).unwrap();
        let mut state = PersistedState::default();
        assert!(state.register(path.clone()));
        assert!(!state.register(path));
        assert_eq!(state.vaults.len(), 1);
    }

    #[test]
    fn forgetting_a_vault_does_not_delete_it() {
        let directory = tempdir().unwrap();
        let path = fs::canonicalize(directory.path()).unwrap();
        let mut state = PersistedState::default();
        state.register(path.clone());
        assert!(state.forget(&path));
        assert!(path.exists());
        assert!(state.vaults.is_empty());
    }

    #[test]
    fn superseded_scan_generations_are_rejected() {
        assert!(is_current_generation(4, 4));
        assert!(!is_current_generation(5, 4));
    }

    #[test]
    fn persisted_registry_round_trips() {
        let state = PersistedState {
            vaults: vec!["/vault/one".into(), "/vault/two".into()],
        };
        let yaml = serde_yaml_ng::to_string(&state).unwrap();
        let restored: PersistedState = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(restored.vaults, state.vaults);
    }
}
