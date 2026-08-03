use std::{fs, path::PathBuf, time::Instant};

use ignore::WalkBuilder;

use super::{Diagnostic, VaultIndex, VaultScanResult, parse_note, resolve_graph};

pub fn scan_vault(root: PathBuf) -> VaultScanResult {
    let started = Instant::now();
    let mut diagnostics = Vec::new();

    if !root.is_dir() {
        return VaultScanResult {
            index: VaultIndex {
                root: root.clone(),
                diagnostics: vec![Diagnostic::error(
                    None,
                    format!("Vault is missing or is not a directory: {}", root.display()),
                )],
                scan_duration: started.elapsed(),
                ..VaultIndex::default()
            },
        };
    }

    let mut builder = WalkBuilder::new(&root);
    builder
        .hidden(true)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false);

    let mut markdown_paths = Vec::new();
    for entry in builder.build() {
        match entry {
            Ok(entry) => {
                let is_file = entry
                    .file_type()
                    .is_some_and(|file_type| file_type.is_file());
                let is_markdown = entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
                if is_file && is_markdown {
                    markdown_paths.push(entry.into_path());
                }
            }
            Err(error) => diagnostics.push(Diagnostic::error(
                None,
                format!("Could not traverse vault entry: {error}"),
            )),
        }
    }

    markdown_paths.sort_by_key(|path| {
        path.strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_lowercase()
    });

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

    let mut index = resolve_graph(parsed_notes);
    index.root = root;
    index.diagnostics = diagnostics;
    index.scan_duration = started.elapsed();
    VaultScanResult { index }
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

        let result = scan_vault(directory.path().to_path_buf());
        assert_eq!(result.index.notes.len(), 1);
        assert_eq!(result.index.notes[0].title, "Visible");
    }

    #[test]
    fn scans_markdown_even_when_gitignore_excludes_it() {
        let directory = tempdir().unwrap();
        write(&directory.path().join(".gitignore"), "included.md\n");
        write(&directory.path().join("included.md"), "# Included");

        let result = scan_vault(directory.path().to_path_buf());
        assert_eq!(result.index.notes.len(), 1);
        assert_eq!(result.index.notes[0].title, "Included");
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        write(&outside.path().join("outside.md"), "# Outside");
        symlink(outside.path(), directory.path().join("linked")).unwrap();

        let result = scan_vault(directory.path().to_path_buf());
        assert!(result.index.notes.is_empty());
    }

    #[test]
    fn invalid_utf8_is_isolated_as_a_global_diagnostic() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("broken.md"), [0xff, 0xfe]).unwrap();
        write(&directory.path().join("valid.md"), "# Valid");

        let result = scan_vault(directory.path().to_path_buf());
        assert_eq!(result.index.notes.len(), 1);
        assert_eq!(result.index.diagnostics.len(), 1);
        assert!(result.index.diagnostics[0].message.contains("UTF-8"));
    }

    #[test]
    fn empty_and_missing_vaults_return_results_without_panicking() {
        let directory = tempdir().unwrap();
        let empty = scan_vault(directory.path().to_path_buf());
        assert!(empty.index.notes.is_empty());
        assert!(empty.index.diagnostics.is_empty());

        let missing = scan_vault(directory.path().join("missing"));
        assert!(missing.index.notes.is_empty());
        assert_eq!(missing.index.diagnostics.len(), 1);
    }
}
