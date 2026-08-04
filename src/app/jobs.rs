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

const POLL_INTERVAL: Duration = Duration::from_millis(50);

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
            let index = scan_vault(worker_root);
            if worker_cancelled.load(Ordering::Acquire) {
                return;
            }
            let layout_started = Instant::now();
            let layout = prepare_board_layout(&index, seed.as_ref());
            let layout_duration = layout_started.elapsed();
            if !worker_cancelled.load(Ordering::Acquire) {
                let _ = sender.send(ScanEnvelope {
                    generation,
                    index,
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
        let Some(update) = poll(self.scan_job.as_ref().map(|job| &job.receiver), context) else {
            return;
        };
        self.scan_job = None;

        match update {
            JobUpdate::Finished(envelope) if envelope.generation == self.scan_generation => {
                self.install_scan(envelope);
            }
            JobUpdate::Finished(_) => {}
            JobUpdate::Lost => {
                self.report_error("The vault scan stopped unexpectedly");
                self.screen = AppScreen::VaultList;
            }
        }
    }

    fn install_scan(&mut self, envelope: ScanEnvelope) {
        self.selected_note = envelope
            .preserve_view
            .then(|| self.selected_note.take())
            .flatten()
            .filter(|selected| envelope.index.note(selected).is_some());
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
        context.request_repaint_after(POLL_INTERVAL);
    }

    pub(super) fn poll_folder_picker(&mut self, context: &egui::Context) {
        let Some(update) = poll(
            self.folder_picker_job.as_ref().map(|job| &job.receiver),
            context,
        ) else {
            return;
        };
        self.folder_picker_job = None;

        match update {
            JobUpdate::Finished(selected) => self.accept_selected_vault(selected, context),
            JobUpdate::Lost => self.report_error("The folder picker stopped unexpectedly"),
        }
    }

    fn report_error(&mut self, message: impl Into<String>) {
        self.notice = Some((DiagnosticSeverity::Error, message.into()));
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
                self.report_error(format!("Could not add {}: {error}", selected.display()));
            }
        }
    }
}

enum JobUpdate<T> {
    Finished(T),
    Lost,
}

fn poll<T>(receiver: Option<&Receiver<T>>, context: &egui::Context) -> Option<JobUpdate<T>> {
    match receiver?.try_recv() {
        Ok(value) => Some(JobUpdate::Finished(value)),
        Err(TryRecvError::Disconnected) => Some(JobUpdate::Lost),
        Err(TryRecvError::Empty) => {
            context.request_repaint_after(POLL_INTERVAL);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::vault::{NoteId, NoteRecord, VaultIndex};

    #[test]
    fn notes_are_looked_up_by_identifier() {
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
        assert!(index.note(&note_id).is_some());
        assert!(index.note(&NoteId(PathBuf::from("deleted.md"))).is_none());
    }
}
