use regex::Regex;
use std::collections::HashMap;

pub fn sanitize_branch_name(branch: &str) -> String {
    let re = Regex::new(r"[^a-zA-Z0-9_\-]").unwrap();
    let sanitized = re.replace_all(branch, "-").to_string();
    let re_multi = Regex::new(r"-{2,}").unwrap();
    let result = re_multi.replace_all(&sanitized, "-").to_string();
    result.trim_matches('-').to_string()
}

pub fn next_review_number(
    bod_dir: &std::path::Path,
    codename: &str,
    sanitized_branch: &str,
) -> u32 {
    let prefix = format!("{}-{}-", codename, sanitized_branch);
    let mut max_num: u32 = 0;

    if let Ok(entries) = std::fs::read_dir(bod_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(rest) = name.strip_prefix(&prefix)
                && let Some(num_str) = rest.strip_suffix(".md")
                && let Ok(n) = num_str.parse::<u32>()
            {
                max_num = max_num.max(n);
            }
        }
    }

    max_num + 1
}

pub fn review_filename(codename: &str, sanitized_branch: &str, number: u32) -> String {
    format!("{}-{}-{}.md", codename, sanitized_branch, number)
}

pub fn consolidated_filename(sanitized_branch: &str, number: u32) -> String {
    format!("consolidated-{}-{}.md", sanitized_branch, number)
}

pub fn next_consolidated_number(bod_dir: &std::path::Path, sanitized_branch: &str) -> u32 {
    let prefix = format!("consolidated-{}-", sanitized_branch);
    let mut max_num: u32 = 0;

    if let Ok(entries) = std::fs::read_dir(bod_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(rest) = name.strip_prefix(&prefix)
                && let Some(num_str) = rest.strip_suffix(".md")
                && let Ok(n) = num_str.parse::<u32>()
            {
                max_num = max_num.max(n);
            }
        }
    }

    max_num + 1
}

/// List all review .md files in the state directory, excluding consolidated reports and bugfix log.
pub fn list_review_files(bod_dir: &std::path::Path) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(bod_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if name.ends_with(".md")
                && !name.starts_with("consolidated-")
                && name != "bugfix.log.md"
            {
                files.push(name);
            }
        }
    }

    files.sort();
    files
}

/// Group review files by round key, using the provided codenames to parse correctly.
pub fn group_reviews_by_round(
    files: &[String],
    codenames: &[String],
) -> HashMap<String, Vec<String>> {
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();

    for file in files {
        if let Some(stem) = file.strip_suffix(".md") {
            let round_key = codenames
                .iter()
                .find_map(|cn| {
                    stem.strip_prefix(cn.as_str())
                        .and_then(|rest| rest.strip_prefix('-'))
                })
                .unwrap_or(stem);
            groups
                .entry(round_key.to_string())
                .or_default()
                .push(file.clone());
        }
    }

    groups
}

pub fn latest_review_files(
    files: &[String],
    codenames: &[String],
    sanitized_branch: &str,
) -> Option<Vec<String>> {
    let prefix = format!("{}-", sanitized_branch);

    group_reviews_by_round(files, codenames)
        .into_iter()
        .filter_map(|(round_key, mut round_files)| {
            let number = round_key.strip_prefix(&prefix)?.parse::<u32>().ok()?;
            round_files.sort();
            Some((number, round_files))
        })
        .max_by_key(|(number, _)| *number)
        .map(|(_, round_files)| round_files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_review_files_picks_highest_round_for_branch() {
        let files = vec![
            "codex-feature-1.md".to_string(),
            "gemini-feature-1.md".to_string(),
            "opus-feature-1.md".to_string(),
            "codex-feature-2.md".to_string(),
            "gemini-feature-2.md".to_string(),
            "opus-feature-2.md".to_string(),
            "codex-other-3.md".to_string(),
        ];
        let codenames = vec![
            "codex".to_string(),
            "gemini".to_string(),
            "opus".to_string(),
        ];

        let latest = latest_review_files(&files, &codenames, "feature").unwrap();

        assert_eq!(
            latest,
            vec![
                "codex-feature-2.md".to_string(),
                "gemini-feature-2.md".to_string(),
                "opus-feature-2.md".to_string(),
            ]
        );
    }

    #[test]
    fn latest_review_files_returns_none_when_branch_has_no_reviews() {
        let files = vec!["codex-other-1.md".to_string()];
        let codenames = vec!["codex".to_string()];

        assert!(latest_review_files(&files, &codenames, "feature").is_none());
    }
}
