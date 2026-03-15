use std::process::Command;

pub fn detect_default_branch() -> Result<String, String> {
    // Try symbolic-ref first
    let output = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if output.status.success() {
        let full_ref = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // refs/remotes/origin/main -> main
        if let Some(branch) = full_ref.strip_prefix("refs/remotes/origin/") {
            return Ok(branch.to_string());
        }
    }

    // Fallback: try common branch names
    for candidate in &["main", "master"] {
        let check = Command::new("git")
            .args(["rev-parse", "--verify", &format!("origin/{}", candidate)])
            .output()
            .map_err(|e| format!("Failed to run git: {}", e))?;

        if check.status.success() {
            return Ok(candidate.to_string());
        }
    }

    Err("Could not detect default branch. Ensure origin remote is configured.".to_string())
}

pub fn current_branch() -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if !output.status.success() {
        return Err("Failed to get current branch name.".to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn generate_diff(default_branch: &str) -> Result<String, String> {
    // Use origin/main (two-dot) against working tree so uncommitted changes
    // (e.g. from the bugfix agent) are visible to reviewers.
    let output = Command::new("git")
        .args(["diff", &format!("origin/{}", default_branch)])
        .output()
        .map_err(|e| format!("Failed to run git diff: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {}", stderr));
    }

    let diff = String::from_utf8_lossy(&output.stdout).to_string();
    if diff.trim().is_empty() {
        return Err("No diff found between current branch and default branch.".to_string());
    }

    Ok(diff)
}

pub fn repo_root() -> Result<std::path::PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if !output.status.success() {
        return Err("Not inside a git repository.".to_string());
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(std::path::PathBuf::from(root))
}
