use crate::paths;
use std::fs;
use std::path::{Path, PathBuf};

const BUGFIX_LOG: &str = "bugfix.log.md";

pub fn ensure_state_dir(repo_root: &Path) -> Result<PathBuf, String> {
    paths::ensure_repo_state_dir(repo_root)
}

/// Remove all generated review .md files from the state directory, except the bugfix log.
pub fn clean_state_dir(state_dir: &Path) -> Result<u32, String> {
    let mut count = 0;
    if let Ok(entries) = fs::read_dir(state_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".md") && *name_str != *BUGFIX_LOG {
                fs::remove_file(entry.path())
                    .map_err(|e| format!("Failed to remove {}: {}", name_str, e))?;
                count += 1;
            }
        }
    }
    Ok(count)
}

pub fn bugfix_log_path(state_dir: &Path) -> PathBuf {
    state_dir.join(BUGFIX_LOG)
}
