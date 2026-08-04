use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct PersistedState {
    pub(super) vaults: Vec<PathBuf>,
}

impl PersistedState {
    pub(super) fn register(&mut self, path: PathBuf) {
        if !self.vaults.contains(&path) {
            self.vaults.push(path);
            self.vaults.sort_by_key(|path| path_key(path));
        }
    }

    pub(super) fn forget(&mut self, path: &Path) {
        self.vaults.retain(|existing| existing != path);
    }

    pub(super) fn deduplicate(&mut self) {
        self.vaults.sort_by_key(|path| path_key(path));
        self.vaults.dedup();
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::PersistedState;

    #[test]
    fn vault_registry_deduplicates_paths() {
        let directory = tempdir().unwrap();
        let path = fs::canonicalize(directory.path()).unwrap();
        let mut state = PersistedState::default();
        state.register(path.clone());
        state.register(path);
        assert_eq!(state.vaults.len(), 1);
    }

    #[test]
    fn forgetting_a_vault_does_not_delete_it() {
        let directory = tempdir().unwrap();
        let path = fs::canonicalize(directory.path()).unwrap();
        let mut state = PersistedState::default();
        state.register(path.clone());
        state.forget(&path);
        assert!(path.exists());
        assert!(state.vaults.is_empty());
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
