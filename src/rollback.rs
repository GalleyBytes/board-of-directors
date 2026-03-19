use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IterationSnapshot {
    commit: Option<String>,
    untracked_before: BTreeSet<PathBuf>,
    /// Captured content of pre-existing untracked files so they can be
    /// restored if the fix agent modifies them.
    untracked_content: HashMap<PathBuf, Vec<u8>>,
}

pub fn capture(repo_root: &Path) -> Result<IterationSnapshot, String> {
    let output = run_git(repo_root, &["stash", "create", "bod-iteration-snapshot"])?;
    if !output.status.success() {
        return Err(format!(
            "git stash create failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let commit = if raw.is_empty() {
        None
    } else {
        // Validate the returned string is a real commit object.
        let verify = run_git(repo_root, &["rev-parse", "--verify", &raw])?;
        if !verify.status.success() {
            return Err(format!(
                "git stash create returned invalid ref '{}': {}",
                raw,
                String::from_utf8_lossy(&verify.stderr).trim()
            ));
        }
        Some(raw)
    };
    let untracked_before = list_untracked(repo_root)?;
    let mut untracked_content = HashMap::new();
    for path in &untracked_before {
        let full = repo_root.join(path);
        match std::fs::read(&full) {
            Ok(bytes) => {
                untracked_content.insert(path.clone(), bytes);
            }
            Err(e) => {
                eprintln!(
                    "Warning: could not snapshot untracked file {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }
    Ok(IterationSnapshot {
        commit,
        untracked_before,
        untracked_content,
    })
}

pub fn restore(repo_root: &Path, snapshot: &IterationSnapshot) -> Result<(), String> {
    let source = snapshot.commit.as_deref().unwrap_or("HEAD");

    // Restore the working tree from the stash commit tree (top-level snapshot).
    let wt_output = run_git(
        repo_root,
        &["restore", "--source", source, "--worktree", ":/"],
    )?;
    if !wt_output.status.success() {
        return Err(format!(
            "git restore --worktree failed: {}",
            String::from_utf8_lossy(&wt_output.stderr).trim()
        ));
    }

    // Restore the index from the stash's second parent (index snapshot).
    // `git stash create` stores the index state as <stash>^2.
    let index_source = format!("{}^2", source);
    let idx_output = run_git(
        repo_root,
        &["restore", "--source", &index_source, "--staged", ":/"],
    )?;
    if !idx_output.status.success() {
        // If the stash has no second parent (e.g. source is HEAD), fall back
        // to restoring the index from the same source as the worktree.
        let fallback = run_git(
            repo_root,
            &["restore", "--source", source, "--staged", ":/"],
        )?;
        if !fallback.status.success() {
            return Err(format!(
                "git restore --staged failed: {}",
                String::from_utf8_lossy(&fallback.stderr).trim()
            ));
        }
    }

    let current_untracked = list_untracked(repo_root)?;
    // Remove untracked files that were created after the snapshot.
    for path in current_untracked.difference(&snapshot.untracked_before) {
        let full_path = repo_join(repo_root, path)?;
        remove_path(&full_path)?;
        prune_empty_parents(full_path.parent(), repo_root);
    }

    // Restore pre-existing untracked files to their snapshot content.
    for (path, content) in &snapshot.untracked_content {
        let full_path = repo_join(repo_root, path)?;
        // Guard against symlink replacement: if the fix agent replaced a
        // regular file with a symlink, writing through it would land content
        // outside the repo.  Remove the symlink and recreate as a regular file.
        match std::fs::symlink_metadata(&full_path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if let Err(e) = std::fs::remove_file(&full_path) {
                    eprintln!(
                        "Warning: could not remove symlink before restore {}: {}",
                        path.display(),
                        e
                    );
                    continue;
                }
            }
            _ => {}
        }
        if let Err(e) = std::fs::write(&full_path, content) {
            eprintln!(
                "Warning: could not restore untracked file {}: {}",
                path.display(),
                e
            );
        }
    }

    Ok(())
}

fn list_untracked(repo_root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let output = run_git(
        repo_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut files = BTreeSet::new();
    for raw in output.stdout.split(|b| *b == 0) {
        if raw.is_empty() {
            continue;
        }
        let path = PathBuf::from(String::from_utf8_lossy(raw).to_string());
        if is_safe_relative_path(&path) {
            files.insert(path);
        }
    }
    Ok(files)
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run git {}: {}", args.join(" "), e))
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn repo_join(repo_root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if !is_safe_relative_path(relative) {
        return Err(format!(
            "Unsafe path returned by git for rollback: {}",
            relative.display()
        ));
    }
    Ok(repo_root.join(relative))
}

fn remove_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Failed to stat rollback path {}: {}", path.display(), e))?;
    if metadata.file_type().is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|e| format!("Failed to remove directory {}: {}", path.display(), e))?;
    } else {
        std::fs::remove_file(path)
            .map_err(|e| format!("Failed to remove file {}: {}", path.display(), e))?;
    }
    Ok(())
}

fn prune_empty_parents(mut current: Option<&Path>, repo_root: &Path) {
    while let Some(dir) = current {
        if dir == repo_root {
            break;
        }
        match std::fs::remove_dir(dir) {
            Ok(()) => current = dir.parent(),
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .current_dir(dir.path())
            .args(["config", "user.email", "test@example.com"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .current_dir(dir.path())
            .args(["config", "user.name", "Test User"])
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(dir.path().join("tracked.txt"), "base\n").unwrap();
        let status = Command::new("git")
            .current_dir(dir.path())
            .args(["add", "tracked.txt"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .current_dir(dir.path())
            .args(["commit", "-qm", "init"])
            .status()
            .unwrap();
        assert!(status.success());
        dir
    }

    #[test]
    fn restore_reverts_tracked_and_removes_new_untracked_files() {
        let dir = init_repo();
        fs::write(dir.path().join("tracked.txt"), "before snapshot\n").unwrap();

        let snapshot = capture(dir.path()).unwrap();

        fs::write(dir.path().join("tracked.txt"), "after snapshot\n").unwrap();
        fs::write(dir.path().join("new.txt"), "new file\n").unwrap();

        restore(dir.path(), &snapshot).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "before snapshot\n"
        );
        assert!(!dir.path().join("new.txt").exists());
    }

    #[test]
    fn restore_preserves_preexisting_untracked_files() {
        let dir = init_repo();
        fs::write(dir.path().join("keep.txt"), "keep me\n").unwrap();

        let snapshot = capture(dir.path()).unwrap();

        fs::write(dir.path().join("keep.txt"), "keep me\n").unwrap();
        fs::write(dir.path().join("remove.txt"), "remove me\n").unwrap();
        restore(dir.path(), &snapshot).unwrap();

        assert!(dir.path().join("keep.txt").exists());
        assert!(!dir.path().join("remove.txt").exists());
    }

    #[test]
    fn restore_reverts_modified_preexisting_untracked_files() {
        let dir = init_repo();
        fs::write(dir.path().join("config.txt"), "original content\n").unwrap();

        let snapshot = capture(dir.path()).unwrap();

        // Simulate the fix agent modifying the pre-existing untracked file.
        fs::write(dir.path().join("config.txt"), "modified by agent\n").unwrap();
        restore(dir.path(), &snapshot).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("config.txt")).unwrap(),
            "original content\n"
        );
    }

    #[test]
    fn restore_returns_repo_to_the_snapshot_even_if_that_snapshot_is_dirty() {
        let dir = init_repo();
        fs::write(dir.path().join("tracked.txt"), "snapshot state\n").unwrap();

        let snapshot = capture(dir.path()).unwrap();

        fs::write(dir.path().join("tracked.txt"), "after snapshot\n").unwrap();
        restore(dir.path(), &snapshot).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "snapshot state\n"
        );

        let status = Command::new("git")
            .current_dir(dir.path())
            .args(["status", "--short"])
            .output()
            .unwrap();
        assert!(status.status.success());
        assert_eq!(String::from_utf8_lossy(&status.stdout), " M tracked.txt\n");
    }

    #[test]
    fn safe_relative_path_rejects_parent_components() {
        assert!(!is_safe_relative_path(Path::new("../escape")));
        assert!(!is_safe_relative_path(Path::new("/abs")));
        assert!(is_safe_relative_path(Path::new("nested/file.txt")));
    }
}
