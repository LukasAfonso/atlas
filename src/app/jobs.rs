use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use eframe::egui;

use super::{AppScreen, AtlasApp};
use crate::{
    board::{BoardLayout, prepare_board_layout},
    vault::{DiagnosticSeverity, VaultIndex, scan_vault},
};

struct ScanEnvelope {
    generation: u64,
    index: VaultIndex,
    layout: BoardLayout,
    layout_duration: Duration,
    preserve_view: bool,
}

pub(super) struct ScanJob {
    receiver: Receiver<ScanEnvelope>,
    cancelled: Arc<AtomicBool>,
}

pub(super) struct FolderPickerJob {
    receiver: Receiver<Option<PathBuf>>,
}

impl AtlasApp {
    pub(super) fn start_scan(
        &mut self,
        root: PathBuf,
        context: &egui::Context,
        preserve_current: bool,
    ) {
        self.cancel_running_scan();
        self.scan_generation = self.scan_generation.wrapping_add(1);
        let generation = self.scan_generation;
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, receiver) = mpsc::channel();
        let worker_context = context.clone();
        let worker_root = root.clone();
        let seed = preserve_current
            .then(|| self.board.layout_seed(&root))
            .flatten();
        let preserve_view = seed.is_some();

        thread::spawn(move || {
            let result = scan_vault(worker_root);
            if worker_cancelled.load(Ordering::Acquire) {
                return;
            }
            let layout_started = Instant::now();
            let layout = prepare_board_layout(&result.index, seed.as_ref());
            let layout_duration = layout_started.elapsed();
            if !worker_cancelled.load(Ordering::Acquire) {
                let _ = sender.send(ScanEnvelope {
                    generation,
                    index: result.index,
                    layout,
                    layout_duration,
                    preserve_view,
                });
                worker_context.request_repaint();
            }
        });

        if !preserve_view {
            self.selected_note = None;
        }
        self.scan_job = Some(ScanJob {
            receiver,
            cancelled,
        });
        self.screen = AppScreen::Scanning { root, generation };
    }

    pub(super) fn cancel_running_scan(&mut self) {
        if let Some(job) = self.scan_job.take() {
            job.cancelled.store(true, Ordering::Release);
        }
    }

    pub(super) fn cancel_scan(&mut self) {
        self.cancel_running_scan();
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.screen = AppScreen::VaultList;
    }

    pub(super) fn poll_scan(&mut self, context: &egui::Context) {
        let message = self.scan_job.as_ref().map(|job| job.receiver.try_recv());

        match message {
            Some(Ok(envelope)) => {
                self.scan_job = None;
                if is_current_generation(self.scan_generation, envelope.generation) {
                    self.selected_note = retained_selection(
                        self.selected_note.take(),
                        &envelope.index,
                        envelope.preserve_view,
                    );
                    self.board.install_layout(
                        envelope.index.root.clone(),
                        envelope.layout,
                        envelope.preserve_view,
                        envelope.layout_duration,
                    );
                    self.screen = AppScreen::Vault {
                        index: envelope.index,
                    };
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

    pub(super) fn start_folder_picker(&mut self, context: &egui::Context) {
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

    pub(super) fn poll_folder_picker(&mut self, context: &egui::Context) {
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
                self.start_scan(path, context, false);
            }
            Err(error) => {
                self.notice = Some((
                    DiagnosticSeverity::Error,
                    format!("Could not add {}: {error}", selected.display()),
                ));
            }
        }
    }
}

fn is_current_generation(active: u64, incoming: u64) -> bool {
    active == incoming
}

fn retained_selection(
    selected: Option<crate::vault::NoteId>,
    index: &VaultIndex,
    preserve_view: bool,
) -> Option<crate::vault::NoteId> {
    preserve_view
        .then_some(selected)
        .flatten()
        .filter(|selected| index.notes.iter().any(|note| note.id == *selected))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{is_current_generation, retained_selection};
    use crate::vault::{NoteId, NoteRecord, VaultIndex};

    #[test]
    fn superseded_scan_generations_are_rejected() {
        assert!(is_current_generation(4, 4));
        assert!(!is_current_generation(5, 4));
    }

    #[test]
    fn same_vault_selection_is_kept_only_while_the_note_exists() {
        let note_id = NoteId(PathBuf::from("kept.md"));
        let index = VaultIndex {
            notes: vec![NoteRecord {
                id: note_id.clone(),
                relative_path: note_id.0.clone(),
                title: "kept".to_owned(),
                markdown_body: String::new(),
                aliases: Vec::new(),
                tags: Vec::new(),
                references: Vec::new(),
                backlinks: Vec::new(),
                citations: Vec::new(),
                diagnostics: Vec::new(),
            }],
            ..VaultIndex::default()
        };
        assert_eq!(
            retained_selection(Some(note_id.clone()), &index, true),
            Some(note_id.clone())
        );
        assert_eq!(
            retained_selection(Some(NoteId(PathBuf::from("deleted.md"))), &index, true),
            None
        );
        assert_eq!(retained_selection(Some(note_id), &index, false), None);
    }
}
