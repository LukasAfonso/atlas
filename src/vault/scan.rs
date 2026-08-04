use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use ignore::WalkBuilder;

use super::{Diagnostic, VaultIndex, parse_note, resolve_graph};

pub fn scan_vault(root: PathBuf) -> VaultIndex {
    let started = Instant::now();

    if !root.is_dir() {
        return VaultIndex {
            diagnostics: vec![Diagnostic::error(
                None,
                format!("Vault is missing or is not a directory: {}", root.display()),
            )],
            root,
            scan_duration: started.elapsed(),
            ..VaultIndex::default()
        };
    }

    let (markdown_paths, mut diagnostics) = markdown_paths(&root);
    let mut parsed_notes = Vec::with_capacity(markdown_paths.len());
    for path in markdown_paths {
        match fs::read_to_string(&path) {
            Ok(source) => parsed_notes.push(parse_note(&root, &path, &source)),
            Err(error) => diagnostics.push(Diagnostic::error(
                Some(path.strip_prefix(&root).unwrap_or(&path).to_path_buf()),
                format!("Could not read Markdown file as UTF-8: {error}"),
            )),
        }
    }

    VaultIndex {
        root,
        diagnostics,
        scan_duration: started.elapsed(),
        ..resolve_graph(parsed_notes)
    }
}

fn markdown_paths(root: &Path) -> (Vec<PathBuf>, Vec<Diagnostic>) {
    let mut paths = Vec::new();
    let mut diagnostics = Vec::new();
    let walk = WalkBuilder::new(root)
        .hidden(true)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .build();

    for entry in walk {
        match entry {
            Ok(entry) if is_markdown_file(&entry) => paths.push(entry.into_path()),
            Ok(_) => {}
            Err(error) => diagnostics.push(Diagnostic::error(
                None,
                format!("Could not traverse vault entry: {error}"),
            )),
        }
    }

    paths.sort_by_key(|path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_lowercase()
    });
    (paths, diagnostics)
}

fn is_markdown_file(entry: &ignore::DirEntry) -> bool {
    entry
        .file_type()
        .is_some_and(|file_type| file_type.is_file())
        && entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::scan_vault;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn scans_case_insensitive_markdown_and_ignores_hidden_directories() {
        let directory = tempdir().unwrap();
        write(&directory.path().join("visible.MD"), "# Visible");
        write(&directory.path().join(".obsidian/hidden.md"), "# Hidden");
        write(&directory.path().join(".trash/deleted.md"), "# Deleted");
        write(&directory.path().join("attachment.txt"), "Not a note");

        let index = scan_vault(directory.path().to_path_buf());
        assert_eq!(index.notes.len(), 1);
        assert_eq!(index.notes[0].title, "visible");
    }

    #[test]
    fn scans_markdown_even_when_gitignore_excludes_it() {
        let directory = tempdir().unwrap();
        write(&directory.path().join(".gitignore"), "included.md\n");
        write(&directory.path().join("included.md"), "# Included");

        let index = scan_vault(directory.path().to_path_buf());
        assert_eq!(index.notes.len(), 1);
        assert_eq!(index.notes[0].title, "included");
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        write(&outside.path().join("outside.md"), "# Outside");
        symlink(outside.path(), directory.path().join("linked")).unwrap();

        let index = scan_vault(directory.path().to_path_buf());
        assert!(index.notes.is_empty());
    }

    #[test]
    fn invalid_utf8_is_isolated_as_a_global_diagnostic() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("broken.md"), [0xff, 0xfe]).unwrap();
        write(&directory.path().join("valid.md"), "# Valid");

        let index = scan_vault(directory.path().to_path_buf());
        assert_eq!(index.notes.len(), 1);
        assert_eq!(index.diagnostics.len(), 1);
        assert!(index.diagnostics[0].message.contains("UTF-8"));
    }

    #[test]
    fn empty_and_missing_vaults_return_results_without_panicking() {
        let directory = tempdir().unwrap();
        let empty = scan_vault(directory.path().to_path_buf());
        assert!(empty.notes.is_empty());
        assert!(empty.diagnostics.is_empty());

        let missing = scan_vault(directory.path().join("missing"));
        assert!(missing.notes.is_empty());
        assert_eq!(missing.diagnostics.len(), 1);
    }
}
