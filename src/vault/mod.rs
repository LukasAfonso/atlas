mod model;
mod parser;
mod resolve;
mod scan;

pub use model::{
    Diagnostic, DiagnosticSeverity, NoteId, NoteRecord, ParsedNote, ReferenceKind, ReferenceTarget,
    VaultIndex, VaultScanResult,
};
pub use parser::parse_note;
pub use resolve::resolve_graph;
pub use scan::scan_vault;
