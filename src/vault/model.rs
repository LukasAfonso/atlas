use std::{collections::BTreeSet, path::PathBuf, time::Duration};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NoteId(pub PathBuf);

impl NoteId {
    pub fn display(&self) -> String {
        self.0.to_string_lossy().replace('\\', "/")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub path: Option<PathBuf>,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

impl Diagnostic {
    pub fn warning(path: impl Into<Option<PathBuf>>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            severity: DiagnosticSeverity::Warning,
            message: message.into(),
        }
    }

    pub fn error(path: impl Into<Option<PathBuf>>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            severity: DiagnosticSeverity::Error,
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceKind {
    WikiLink,
    MarkdownLink,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceTarget {
    pub raw: String,
    pub kind: ReferenceKind,
}

#[derive(Clone, Debug)]
pub struct ParsedNote {
    pub relative_path: PathBuf,
    pub title: String,
    pub markdown_body: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub unresolved_references: Vec<ReferenceTarget>,
    pub citations: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug)]
pub struct NoteRecord {
    pub id: NoteId,
    pub relative_path: PathBuf,
    pub title: String,
    pub markdown_body: String,
    #[expect(dead_code, reason = "aliases are indexed now for future navigation UI")]
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub references: Vec<NoteId>,
    pub backlinks: Vec<NoteId>,
    pub citations: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Default)]
pub struct VaultIndex {
    pub root: PathBuf,
    pub notes: Vec<NoteRecord>,
    pub diagnostics: Vec<Diagnostic>,
    pub scan_duration: Duration,
}

impl VaultIndex {
    pub fn note(&self, note_id: &NoteId) -> Option<&NoteRecord> {
        self.notes.iter().find(|note| &note.id == note_id)
    }

    pub fn reference_count(&self) -> usize {
        self.notes.iter().map(|note| note.references.len()).sum()
    }

    pub fn backlink_count(&self) -> usize {
        self.notes.iter().map(|note| note.backlinks.len()).sum()
    }

    pub fn unique_citation_count(&self) -> usize {
        self.notes
            .iter()
            .flat_map(|note| note.citations.iter())
            .collect::<BTreeSet<_>>()
            .len()
    }

    pub fn diagnostic_counts(&self) -> (usize, usize) {
        self.diagnostics
            .iter()
            .chain(self.notes.iter().flat_map(|note| note.diagnostics.iter()))
            .fold((0, 0), |(warnings, errors), diagnostic| {
                match diagnostic.severity {
                    DiagnosticSeverity::Warning => (warnings + 1, errors),
                    DiagnosticSeverity::Error => (warnings, errors + 1),
                }
            })
    }
}
