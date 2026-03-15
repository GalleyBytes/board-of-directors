use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub fn ensure_bod_dir(repo_root: &Path) -> Result<PathBuf, String> {
    let bod_dir = repo_root.join(".bod");
    if !bod_dir.exists() {
        fs::create_dir_all(&bod_dir)
            .map_err(|e| format!("Failed to create .bod directory: {}", e))?;
    }
    Ok(bod_dir)
}

const BUGFIX_LOG: &str = "bugfix.log.md";

/// Remove all .md files from the .bod directory, except the bugfix log.
pub fn clean_bod_dir(bod_dir: &Path) -> Result<u32, String> {
    let mut count = 0;
    if let Ok(entries) = fs::read_dir(bod_dir) {
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

pub fn bugfix_log_path(bod_dir: &Path) -> PathBuf {
    bod_dir.join(BUGFIX_LOG)
}

pub fn ensure_gitignore(repo_root: &Path) -> Result<(), String> {
    ensure_gitignore_entries(repo_root, &[".bod/", ".bodrc.toml"])
}

fn ensure_gitignore_entries(repo_root: &Path, entries: &[&str]) -> Result<(), String> {
    let gitignore_path = repo_root.join(".gitignore");

    let existing_lines: Vec<String> = if gitignore_path.exists() {
        let file = fs::File::open(&gitignore_path)
            .map_err(|e| format!("Failed to read .gitignore: {}", e))?;
        let reader = std::io::BufReader::new(file);
        reader
            .lines()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to read .gitignore: {}", e))?
    } else {
        Vec::new()
    };

    let mut to_add: Vec<&str> = Vec::new();
    for entry in entries {
        let base = entry.trim_matches('/');
        let already = existing_lines.iter().any(|line| {
            let t = line.trim();
            let t_base = t.trim_matches('/');
            t_base == base
        });
        if !already {
            to_add.push(entry);
        }
    }

    if to_add.is_empty() {
        return Ok(());
    }

    if gitignore_path.exists() {
        let content = fs::read_to_string(&gitignore_path)
            .map_err(|e| format!("Failed to read .gitignore: {}", e))?;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&gitignore_path)
            .map_err(|e| format!("Failed to open .gitignore for writing: {}", e))?;
        let prefix = if content.ends_with('\n') { "" } else { "\n" };
        for (i, entry) in to_add.iter().enumerate() {
            let p = if i == 0 { prefix } else { "" };
            writeln!(file, "{}{}", p, entry)
                .map_err(|e| format!("Failed to write to .gitignore: {}", e))?;
        }
    } else {
        let content: String = to_add.iter().map(|e| format!("{}\n", e)).collect();
        fs::write(&gitignore_path, content)
            .map_err(|e| format!("Failed to create .gitignore: {}", e))?;
    }

    Ok(())
}
