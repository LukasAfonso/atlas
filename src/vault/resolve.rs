use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

use super::{Diagnostic, NoteId, NoteRecord, ParsedNote, ReferenceKind, VaultIndex};

pub fn resolve_graph(notes: Vec<ParsedNote>) -> VaultIndex {
    let index = NoteLookups::new(&notes);
    let mut records: Vec<_> = notes
        .into_iter()
        .map(|note| resolve_note(note, &index))
        .collect();

    attach_backlinks(&mut records);
    records.sort_by(|left, right| {
        left.title
            .to_lowercase()
            .cmp(&right.title.to_lowercase())
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });

    VaultIndex {
        notes: records,
        ..VaultIndex::default()
    }
}

struct NoteLookups {
    ids: HashSet<NoteId>,
    stems: HashMap<String, Vec<NoteId>>,
    aliases: HashMap<String, Vec<NoteId>>,
}

impl NoteLookups {
    fn new(notes: &[ParsedNote]) -> Self {
        let mut lookups = Self {
            ids: HashSet::with_capacity(notes.len()),
            stems: HashMap::new(),
            aliases: HashMap::new(),
        };
        for note in notes {
            let id = NoteId(normalize_note_path(&note.relative_path));
            if let Some(stem) = note
                .relative_path
                .file_stem()
                .and_then(|stem| stem.to_str())
            {
                lookups
                    .stems
                    .entry(stem.to_lowercase())
                    .or_default()
                    .push(id.clone());
            }
            for alias in &note.aliases {
                lookups
                    .aliases
                    .entry(alias.to_lowercase())
                    .or_default()
                    .push(id.clone());
            }
            lookups.ids.insert(id);
        }
        lookups
    }
}

fn resolve_note(note: ParsedNote, lookups: &NoteLookups) -> NoteRecord {
    let id = NoteId(normalize_note_path(&note.relative_path));
    let source_directory = id.0.parent().unwrap_or_else(|| Path::new(""));
    let mut diagnostics = note.diagnostics;
    let mut references = Vec::new();

    for reference in note.unresolved_references {
        match resolve_reference(source_directory, &reference.raw, reference.kind, lookups) {
            Resolution::Resolved(target) => {
                if !references.contains(&target) {
                    references.push(target);
                }
            }
            Resolution::Unresolved => diagnostics.push(Diagnostic::warning(
                Some(note.relative_path.clone()),
                format!("Unresolved note reference: {}", reference.raw),
            )),
            Resolution::Ambiguous(matches) => diagnostics.push(Diagnostic::warning(
                Some(note.relative_path.clone()),
                format!(
                    "Ambiguous note reference `{}` matches: {}",
                    reference.raw,
                    matches
                        .iter()
                        .map(NoteId::display)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }

    NoteRecord {
        id,
        relative_path: note.relative_path,
        title: note.title,
        markdown_body: note.markdown_body,
        aliases: note.aliases,
        tags: note.tags,
        references,
        backlinks: Vec::new(),
        citations: note.citations,
        diagnostics,
    }
}

fn attach_backlinks(records: &mut [NoteRecord]) {
    let mut backlinks: HashMap<NoteId, Vec<NoteId>> = HashMap::new();
    for record in &*records {
        for target in &record.references {
            backlinks
                .entry(target.clone())
                .or_default()
                .push(record.id.clone());
        }
    }
    for record in records {
        if let Some(sources) = backlinks.remove(&record.id) {
            record.backlinks = sources;
        }
    }
}

enum Resolution {
    Resolved(NoteId),
    Unresolved,
    Ambiguous(Vec<NoteId>),
}

fn resolve_reference(
    source_directory: &Path,
    raw: &str,
    kind: ReferenceKind,
    lookups: &NoteLookups,
) -> Resolution {
    let raw_path = PathBuf::from(raw.replace('\\', "/"));
    let path_with_extension = with_markdown_extension(raw_path, kind);

    let paths = [
        source_directory.join(&path_with_extension),
        path_with_extension.clone(),
    ];
    for path in &paths {
        if let Some(candidate) = normalize_relative(path).map(NoteId)
            && lookups.ids.contains(&candidate)
        {
            return Resolution::Resolved(candidate);
        }
    }

    let stem = path_with_extension
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(raw)
        .to_lowercase();
    if let Some(matches) = lookups.stems.get(&stem) {
        return unique_or_ambiguous(matches);
    }
    if let Some(matches) = lookups.aliases.get(&raw.to_lowercase()) {
        return unique_or_ambiguous(matches);
    }
    Resolution::Unresolved
}

fn unique_or_ambiguous(matches: &[NoteId]) -> Resolution {
    if matches.len() == 1 {
        Resolution::Resolved(matches[0].clone())
    } else {
        Resolution::Ambiguous(matches.to_vec())
    }
}

fn with_markdown_extension(mut path: PathBuf, kind: ReferenceKind) -> PathBuf {
    if kind == ReferenceKind::WikiLink && path.extension().is_none() {
        path.set_extension("md");
    }
    path
}

fn normalize_note_path(path: &Path) -> PathBuf {
    normalize_relative(path).unwrap_or_else(|| path.to_path_buf())
}

fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::resolve_graph;
    use crate::vault::{ParsedNote, ReferenceKind, ReferenceTarget};

    fn note(path: &str, title: &str, aliases: &[&str], references: &[&str]) -> ParsedNote {
        ParsedNote {
            relative_path: PathBuf::from(path),
            title: title.to_owned(),
            markdown_body: format!("# {title}"),
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
            tags: Vec::new(),
            unresolved_references: references
                .iter()
                .map(|reference| ReferenceTarget {
                    raw: (*reference).to_owned(),
                    kind: ReferenceKind::WikiLink,
                })
                .collect(),
            citations: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn resolves_relative_vault_stem_and_alias_then_builds_backlinks() {
        let index = resolve_graph(vec![
            note(
                "folder/source.md",
                "Source",
                &[],
                &["local", "root", "Unique", "Alias"],
            ),
            note("folder/local.md", "Local", &[], &[]),
            note("root.md", "Root", &[], &[]),
            note("elsewhere/Unique.md", "Unique", &[], &[]),
            note("aliased.md", "Aliased", &["Alias"], &[]),
        ]);

        let source = index
            .notes
            .iter()
            .find(|note| note.title == "Source")
            .unwrap();
        assert_eq!(source.references.len(), 4);
        for title in ["Local", "Root", "Unique", "Aliased"] {
            let target = index.notes.iter().find(|note| note.title == title).unwrap();
            assert_eq!(
                target.backlinks.as_slice(),
                std::slice::from_ref(&source.id)
            );
        }
    }

    #[test]
    fn unresolved_and_ambiguous_references_are_diagnostics() {
        let index = resolve_graph(vec![
            note("source.md", "Source", &[], &["Missing", "Duplicate"]),
            note("a/Duplicate.md", "First", &[], &[]),
            note("b/Duplicate.md", "Second", &[], &[]),
        ]);
        let source = index
            .notes
            .iter()
            .find(|note| note.title == "Source")
            .unwrap();
        assert_eq!(source.references.len(), 0);
        assert_eq!(source.diagnostics.len(), 2);
        assert!(source.diagnostics[0].message.contains("Unresolved"));
        assert!(source.diagnostics[1].message.contains("Ambiguous"));
    }

    #[test]
    fn duplicate_references_create_one_edge() {
        let index = resolve_graph(vec![
            note("source.md", "Source", &[], &["target", "target"]),
            note("target.md", "Target", &[], &[]),
        ]);
        let source = index
            .notes
            .iter()
            .find(|note| note.title == "Source")
            .unwrap();
        let target = index
            .notes
            .iter()
            .find(|note| note.title == "Target")
            .unwrap();
        assert_eq!(source.references.len(), 1);
        assert_eq!(target.backlinks.len(), 1);
    }

    #[test]
    fn resolves_relative_and_vault_relative_markdown_links() {
        let source = ParsedNote {
            relative_path: PathBuf::from("folder/source.md"),
            title: "Source".to_owned(),
            markdown_body: "# Source".to_owned(),
            aliases: Vec::new(),
            tags: Vec::new(),
            unresolved_references: ["../target.md", "root.md"]
                .into_iter()
                .map(|raw| ReferenceTarget {
                    raw: raw.to_owned(),
                    kind: ReferenceKind::MarkdownLink,
                })
                .collect(),
            citations: Vec::new(),
            diagnostics: Vec::new(),
        };
        let index = resolve_graph(vec![
            source,
            note("target.md", "Target", &[], &[]),
            note("root.md", "Root", &[], &[]),
        ]);
        let source = index
            .notes
            .iter()
            .find(|note| note.title == "Source")
            .unwrap();
        assert_eq!(source.references.len(), 2);
    }
}
