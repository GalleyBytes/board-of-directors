use crate::config::Config;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_PERSONALITY: &str = "default";
const GLOBAL_PERSONALITY_DIR: &str = "personalities";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonalityConfig {
    pub name: String,
    pub instructions: Option<String>,
}

impl PersonalityConfig {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            instructions: None,
        }
    }

    pub fn inline(name: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            instructions: Some(instructions.into()),
        }
    }

    pub fn normalized_name(&self) -> &str {
        self.name.trim()
    }
}

impl Default for PersonalityConfig {
    fn default() -> Self {
        Self::named(DEFAULT_PERSONALITY)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPersonality {
    pub name: String,
    pub label: String,
    pub description: String,
    pub instructions: String,
    pub source: PersonalitySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonalitySource {
    Builtin,
    Global,
    Inline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalityChoice {
    pub name: String,
    pub label: String,
    pub description: String,
    pub source: PersonalitySource,
}

#[derive(Debug, Clone)]
pub struct BuiltinPersonality {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub instructions: &'static str,
}

pub fn builtin_personalities() -> &'static [BuiltinPersonality] {
    &[
        BuiltinPersonality {
            name: DEFAULT_PERSONALITY,
            label: "Default",
            description: "Use the standard behavior with no extra instructions.",
            instructions: "",
        },
        BuiltinPersonality {
            name: "architectural-sanity-check",
            label: "Architectural Sanity Check",
            description: "Question the core premise before trusting the local implementation.",
            instructions: "Question whether the change should exist in this form before focusing on line-level correctness. Look for signs that the codebase may already have a system, utility, boundary, or long-standing pattern that should own this responsibility, and surface possible duplication or architectural mismatch when you find it.",
        },
        BuiltinPersonality {
            name: "blast-radius-context",
            label: "Blast Radius & Context",
            description: "Prioritize missing system context, dependencies, and downstream impact.",
            instructions: "Map the external systems, services, data flows, and ownership boundaries implied by the change. Call out the broader system context a human needs before approving, especially where the diff could silently affect legacy behavior, adjacent services, observability, or operational workflows.",
        },
        BuiltinPersonality {
            name: "devils-advocate",
            label: "Devil's Advocate",
            description: "Take a skeptical posture and assume the problem may already be solved elsewhere.",
            instructions: "Actively look for reasons not to merge until the repository evidence supports the change. Assume the problem may already be solved elsewhere, look for reinvention, and point to concrete repo areas, search terms, or system boundaries that should be checked before the change is trusted.",
        },
        BuiltinPersonality {
            name: "systems-guru",
            label: "Systems Guru",
            description: "Connect the dots across subsystems, shared libraries, and historical behavior.",
            instructions: "Connect the local diff to neighboring services, shared libraries, background jobs, metrics, and historical system behavior that a surface-level review could miss. Use read-only exploration to explain how this change fits into the larger system and which upstream or downstream components are likely to matter.",
        },
        BuiltinPersonality {
            name: "legacy-archaeologist",
            label: "Legacy Archaeologist",
            description: "Hunt for historical reasons, compatibility paths, and older implementations.",
            instructions: "Assume the codebase has historical context worth excavating. Look for older flows, feature flags, migrations, fallback paths, and compatibility layers that might explain why the code is shaped the way it is, and flag cases where the change may bypass or duplicate that older behavior.",
        },
        BuiltinPersonality {
            name: "curious-junior",
            label: "Curious Junior",
            description: "Ask the basic questions that reveal hidden assumptions and missing explanations.",
            instructions: "Ask the simple and slightly uncomfortable questions that force the rationale to become explicit. Highlight areas where the change is hard to explain, where naming or control flow obscures intent, or where assumptions need to be justified before the change can be considered safe.",
        },
        BuiltinPersonality {
            name: "helpful-owner",
            label: "Helpful Owner",
            description: "Take ownership of the messy problem and propose concrete next steps.",
            instructions: "Be the reviewer who willingly tackles the hard or neglected problem. Chase difficult edge cases, dig through the messy areas other reviewers might skip, and offer concrete next steps that would make the change safer for the next engineer who inherits it.",
        },
    ]
}

pub fn builtin_choice(name: &str) -> Option<&'static BuiltinPersonality> {
    builtin_personalities().iter().find(|entry| entry.name == name)
}

pub fn resolve(selection: &PersonalityConfig) -> Result<ResolvedPersonality, String> {
    resolve_from_dir(&global_personalities_dir(), selection)
}

fn resolve_from_dir(dir: &Path, selection: &PersonalityConfig) -> Result<ResolvedPersonality, String> {
    let name = selection.normalized_name();
    validate_name(name)?;
    if let Some(instructions) = selection.instructions.as_ref() {
        let trimmed = instructions.trim();
        if trimmed.is_empty() {
            return Err(format!(
                "Personality '{}' has empty inline instructions.",
                name
            ));
        }
        return Ok(ResolvedPersonality {
            name: name.to_string(),
            label: title_case_slug(name),
            description: "Inline custom personality stored in config.".to_string(),
            instructions: trimmed.to_string(),
            source: PersonalitySource::Inline,
        });
    }

    if let Some(global) = load_global_personality_from_dir(dir, name)? {
        return Ok(global);
    }

    if let Some(entry) = builtin_choice(name) {
        return Ok(ResolvedPersonality {
            name: entry.name.to_string(),
            label: entry.label.to_string(),
            description: entry.description.to_string(),
            instructions: entry.instructions.to_string(),
            source: PersonalitySource::Builtin,
        });
    }

    Err(format!(
        "Unknown personality '{}'. Choose a built-in personality, save a global personality under {}, or add inline instructions in the config.",
        name,
        dir.display()
    ))
}

pub fn personality_prompt_block(
    role_label: &str,
    personality: &ResolvedPersonality,
) -> String {
    let instructions = personality.instructions.trim();
    if instructions.is_empty() {
        return String::new();
    }

    format!(
        "\n\nAdditional {role_label} personality guidance ({label} / {name}):\n{instructions}",
        label = personality.label,
        name = personality.name
    )
}

pub fn display_selection(selection: &PersonalityConfig) -> String {
    let name = selection.normalized_name();
    if selection.instructions.is_some() {
        format!("{} (inline)", name)
    } else if let Some(entry) = builtin_choice(name) {
        entry.label.to_string()
    } else {
        format!("{} (global)", name)
    }
}

pub fn validate_configured_personalities(config: &Config) -> Result<(), String> {
    for entry in &config.review.models {
        validate_selection(
            &entry.personality,
            &format!("reviewer '{}'", entry.codename),
        )?;
    }
    validate_selection(&config.consolidate.personality, "consolidator")?;
    Ok(())
}

pub fn validate_selection(selection: &PersonalityConfig, role_label: &str) -> Result<(), String> {
    validate_selection_from_dir(&global_personalities_dir(), selection, role_label)
}

fn validate_selection_from_dir(
    dir: &Path,
    selection: &PersonalityConfig,
    role_label: &str,
) -> Result<(), String> {
    let name = selection.normalized_name();
    validate_name(name)?;
    if let Some(instructions) = selection.instructions.as_ref() {
        if instructions.trim().is_empty() {
            return Err(format!(
                "{} personality '{}' has empty inline instructions.",
                role_label, name
            ));
        }
        return Ok(());
    }

    if load_global_personality_from_dir(dir, name)?.is_some() {
        return Ok(());
    }

    if builtin_choice(name).is_some() {
        return Ok(());
    }

    Err(format!(
        "{} personality '{}' was not found. Save it under {} or re-run 'bod init'.",
        role_label,
        name,
        dir.display()
    ))
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        || !name.chars().any(|c| c.is_ascii_alphanumeric())
    {
        return Err(format!(
            "Invalid personality name '{}'. Personality names must contain only [a-zA-Z0-9_-] with at least one alphanumeric character.",
            name
        ));
    }
    Ok(())
}

pub fn sanitize_name(raw: &str) -> Result<String, String> {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('-').to_string();
    validate_name(&sanitized)?;
    Ok(sanitized)
}

pub fn list_catalog() -> Result<Vec<PersonalityChoice>, String> {
    let mut choices = builtin_personalities()
        .iter()
        .map(|entry| PersonalityChoice {
            name: entry.name.to_string(),
            label: entry.label.to_string(),
            description: entry.description.to_string(),
            source: PersonalitySource::Builtin,
        })
        .collect::<Vec<_>>();
    choices.extend(list_global_personality_choices()?);
    Ok(choices)
}

pub fn save_global_personality(name: &str, instructions: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    if instructions.trim().is_empty() {
        return Err(format!(
            "Global personality '{}' cannot be empty.",
            name
        ));
    }
    write_global_personality_to_dir(
        &global_personalities_dir(),
        name,
        instructions.trim(),
    )
}

pub fn global_personalities_dir() -> PathBuf {
    paths::app_dir().join(GLOBAL_PERSONALITY_DIR)
}

fn load_global_personality_from_dir(
    dir: &Path,
    name: &str,
) -> Result<Option<ResolvedPersonality>, String> {
    let path = dir.join(format!("{}.md", name));
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read personality {}: {}", path.display(), e))?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "Global personality '{}' at {} is empty.",
            name,
            path.display()
        ));
    }
    Ok(Some(ResolvedPersonality {
        name: name.to_string(),
        label: title_case_slug(name),
        description: format!("Global custom personality from {}", path.display()),
        instructions: trimmed.to_string(),
        source: PersonalitySource::Global,
    }))
}

fn list_global_personality_choices() -> Result<Vec<PersonalityChoice>, String> {
    list_global_personality_choices_from_dir(&global_personalities_dir())
}

fn list_global_personality_choices_from_dir(dir: &Path) -> Result<Vec<PersonalityChoice>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)
        .map_err(|e| format!("Failed to read {}: {}", dir.display(), e))?
    {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if validate_name(stem).is_err() {
            eprintln!("Warning: skipping non-conforming personality file: {}", path.display());
            continue;
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read personality {}: {}", path.display(), e))?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(format!(
                "Global personality '{}' at {} is empty.",
                stem,
                path.display()
            ));
        }
        let preview = trimmed
            .lines()
            .next()
            .unwrap_or("Custom personality")
            .trim();
        entries.push(PersonalityChoice {
            name: stem.to_string(),
            label: title_case_slug(stem),
            description: format!("Global custom: {}", preview),
            source: PersonalitySource::Global,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn write_global_personality_to_dir(
    dir: &Path,
    name: &str,
    instructions: &str,
) -> Result<PathBuf, String> {
    fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create personality directory {}: {}", dir.display(), e))?;
    let path = dir.join(format!("{}.md", name));
    fs::write(&path, instructions)
        .map_err(|e| format!("Failed to write personality {}: {}", path.display(), e))?;
    Ok(path)
}

fn title_case_slug(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = String::new();
                    out.push(first.to_ascii_uppercase());
                    out.push_str(chars.as_str());
                    out
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn default_personality_resolves_without_extra_instructions() {
        let resolved = resolve(&PersonalityConfig::default()).unwrap();
        assert_eq!(resolved.name, DEFAULT_PERSONALITY);
        assert!(resolved.instructions.is_empty());
    }

    #[test]
    fn inline_personality_wins_over_named_lookup() {
        let resolved =
            resolve(&PersonalityConfig::inline("custom-reviewer", "Find deeper system context."))
                .unwrap();
        assert_eq!(resolved.source, PersonalitySource::Inline);
        assert_eq!(resolved.instructions, "Find deeper system context.");
    }

    #[test]
    fn global_personalities_are_listed_from_md_files() {
        let dir = tempdir().unwrap();
        write_global_personality_to_dir(dir.path(), "my-reviewer", "Look for context.")
            .unwrap();

        let choices = list_global_personality_choices_from_dir(dir.path()).unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].name, "my-reviewer");
        assert_eq!(choices[0].source, PersonalitySource::Global);
    }

    #[test]
    fn load_global_personality_reads_template_body() {
        let dir = tempdir().unwrap();
        write_global_personality_to_dir(dir.path(), "systems-guy", "Trace adjacent systems.")
            .unwrap();

        let resolved = load_global_personality_from_dir(dir.path(), "systems-guy")
            .unwrap()
            .unwrap();
        assert_eq!(resolved.instructions, "Trace adjacent systems.");
        assert_eq!(resolved.source, PersonalitySource::Global);
    }

    #[test]
    fn global_personalities_override_builtin_names() {
        let dir = tempdir().unwrap();
        write_global_personality_to_dir(dir.path(), DEFAULT_PERSONALITY, "Use the custom file.")
            .unwrap();

        let resolved = resolve_from_dir(dir.path(), &PersonalityConfig::default()).unwrap();
        assert_eq!(resolved.source, PersonalitySource::Global);
        assert_eq!(resolved.instructions, "Use the custom file.");
    }

    #[test]
    fn validation_reports_invalid_global_personalities_even_for_builtin_names() {
        let dir = tempdir().unwrap();
        write_global_personality_to_dir(dir.path(), DEFAULT_PERSONALITY, "   ").unwrap();

        let error = validate_selection_from_dir(
            dir.path(),
            &PersonalityConfig::default(),
            "reviewer",
        )
        .unwrap_err();
        assert!(error.contains("empty"));
    }

    #[test]
    fn sanitize_name_rejects_empty_after_cleanup() {
        let error = sanitize_name("../../").unwrap_err();
        assert!(error.contains("Invalid personality name"));
    }

    #[test]
    fn non_conforming_md_files_are_skipped() {
        let dir = tempdir().unwrap();
        // Valid personality
        write_global_personality_to_dir(dir.path(), "good-name", "Valid personality.").unwrap();
        // Non-conforming file (contains a space)
        fs::write(dir.path().join("has space.md"), "Some content.").unwrap();

        let choices = list_global_personality_choices_from_dir(dir.path()).unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].name, "good-name");
    }
}
