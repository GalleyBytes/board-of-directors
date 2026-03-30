use crate::paths;
use crate::personalities::PersonalityConfig;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Copilot,
    #[serde(rename = "claude-code")]
    ClaudeCode,
    #[serde(rename = "gemini-cli")]
    GeminiCli,
}

impl Default for Backend {
    fn default() -> Self {
        Self::Copilot
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Copilot => write!(f, "copilot"),
            Self::ClaudeCode => write!(f, "claude-code"),
            Self::GeminiCli => write!(f, "gemini-cli"),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub review: ReviewConfig,
    pub consolidate: ConsolidateConfig,
    pub bugfix: BugfixConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub codename: String,
    pub backend: Backend,
    pub model: String,
    pub personality: PersonalityConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConsolidateConfig {
    pub backend: Backend,
    pub model: String,
    pub personality: PersonalityConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BugfixConfig {
    pub backend: Backend,
    pub model: String,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            models: vec![
                ModelEntry {
                    codename: "opus".to_string(),
                    backend: Backend::ClaudeCode,
                    model: "claude-opus-4-6".to_string(),
                    personality: PersonalityConfig::default(),
                },
                ModelEntry {
                    codename: "gemini".to_string(),
                    backend: Backend::GeminiCli,
                    model: "gemini-3-pro-preview".to_string(),
                    personality: PersonalityConfig::default(),
                },
                ModelEntry {
                    codename: "codex".to_string(),
                    backend: Backend::Copilot,
                    model: "gpt-4o".to_string(),
                    personality: PersonalityConfig::default(),
                },
            ],
        }
    }
}

impl Default for ConsolidateConfig {
    fn default() -> Self {
        Self {
            backend: Backend::ClaudeCode,
            model: "claude-sonnet-4-6".to_string(),
            personality: PersonalityConfig::default(),
        }
    }
}

impl Default for BugfixConfig {
    fn default() -> Self {
        Self {
            backend: Backend::Copilot,
            model: "gpt-4o".to_string(),
        }
    }
}

const GLOBAL_CONFIG: &str = ".bodrc.toml";

pub fn global_config_path() -> PathBuf {
    paths::app_dir().join(GLOBAL_CONFIG)
}

pub fn load(repo_root: &Path) -> Result<Config, String> {
    let local_path = local_config_path(repo_root);
    if let Some(config) = load_path(&local_path)? {
        return Ok(config);
    }

    let global_path = global_config_path();
    if let Some(config) = load_path(&global_path)? {
        return Ok(config);
    }

    Ok(Config::default())
}

pub fn load_path(path: &Path) -> Result<Option<Config>, String> {
    load_path_with_notice(path, true)
}

pub fn load_path_silent(path: &Path) -> Result<Option<Config>, String> {
    load_path_with_notice(path, false)
}

fn load_path_with_notice(path: &Path, announce: bool) -> Result<Option<Config>, String> {
    if !path.exists() {
        return Ok(None);
    }

    let label = path.display().to_string();
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", label, e))?;
    let config = parse_config_content(&content, &label)?;
    if announce {
        println!("Loaded config from {}", label);
    }
    Ok(Some(config))
}

fn parse_config_content(content: &str, label: &str) -> Result<Config, String> {
    toml::from_str::<Config>(content).map_err(|parse_error| {
        format!("Failed to parse {}: {}", label, parse_error)
    })
}

pub fn write_global(config: &Config) -> Result<(), String> {
    let path = global_config_path();
    write_config(config, &path)
}

pub fn write_local(config: &Config, repo_root: &Path) -> Result<(), String> {
    let path = local_config_path(repo_root);
    write_config(config, &path)
}

fn write_config(config: &Config, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }
    let content =
        toml::to_string_pretty(config).map_err(|e| format!("Failed to serialize config: {}", e))?;
    std::fs::write(path, content)
        .map_err(|e| format!("Failed to write config to {}: {}", path.display(), e))?;
    Ok(())
}

pub fn local_config_path(repo_root: &Path) -> PathBuf {
    paths::repo_config_path(repo_root)
}

impl Config {
    pub fn used_backends(&self) -> Vec<Backend> {
        let mut used = Vec::new();
        for entry in &self.review.models {
            push_unique_backend(&mut used, entry.backend);
        }
        push_unique_backend(&mut used, self.consolidate.backend);
        push_unique_backend(&mut used, self.bugfix.backend);
        used.sort();
        used
    }
}

fn push_unique_backend(backends: &mut Vec<Backend>, backend: Backend) {
    if !backends.contains(&backend) {
        backends.push(backend);
    }
}

pub fn codename_is_duplicate(codename: &str, existing: &[String]) -> bool {
    existing.iter().any(|existing_codename| existing_codename == codename)
}

pub fn validate_models_for_backend(config: &Config) -> Result<(), String> {
    let mut unsafe_codenames = Vec::new();
    for entry in &config.review.models {
        if entry.codename.is_empty()
            || !entry
                .codename
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || !entry.codename.chars().any(|c| c.is_ascii_alphanumeric())
        {
            unsafe_codenames.push(entry.codename.clone());
        }
    }
    if !unsafe_codenames.is_empty() {
        unsafe_codenames.sort();
        unsafe_codenames.dedup();
        return Err(format!(
            "Invalid codename(s): {}. Codenames must contain only [a-zA-Z0-9_-] with at least one alphanumeric character. Run 'bod init' to reconfigure.",
            unsafe_codenames.join(", ")
        ));
    }

    let mut reserved_codenames = Vec::new();
    for entry in &config.review.models {
        if entry.codename == "consolidated" || entry.codename.starts_with("consolidated-") {
            reserved_codenames.push(entry.codename.clone());
        }
    }
    if !reserved_codenames.is_empty() {
        reserved_codenames.sort();
        reserved_codenames.dedup();
        return Err(format!(
            "Reserved codename(s): {}. 'consolidated' conflicts with consolidated report filenames. Run 'bod init' to reconfigure.",
            reserved_codenames.join(", ")
        ));
    }

    let mut seen_codenames = Vec::new();
    let mut duplicate_codenames = Vec::new();
    for entry in &config.review.models {
        if codename_is_duplicate(&entry.codename, &seen_codenames) {
            duplicate_codenames.push(entry.codename.clone());
        } else {
            seen_codenames.push(entry.codename.clone());
        }
    }
    if !duplicate_codenames.is_empty() {
        duplicate_codenames.sort();
        duplicate_codenames.dedup();
        return Err(format!(
            "Duplicate codename(s): {}. Each reviewer must have a unique codename so dashboard files do not collide. Run 'bod init' to reconfigure.",
            duplicate_codenames.join(", ")
        ));
    }

    for entry in &config.review.models {
        validate_role_model(
            entry.backend,
            &entry.model,
            &format!("reviewer '{}'", entry.codename),
        )?;
    }
    validate_role_model(
        config.consolidate.backend,
        &config.consolidate.model,
        "consolidator",
    )?;
    validate_role_model(config.bugfix.backend, &config.bugfix.model, "fixer")?;
    Ok(())
}

fn validate_role_model(backend: Backend, model: &str, role: &str) -> Result<(), String> {
    match backend {
        Backend::Copilot => {
            let suspect = [
                "opus",
                "sonnet",
                "haiku",
                "auto",
                "pro",
                "flash",
                "flash-lite",
            ];
            if suspect.contains(&model) {
                eprintln!(
                    "Warning: {} model '{}' looks like a backend-specific shorthand and may be invalid for the Copilot backend. Run 'bod init' to reconfigure.",
                    role, model
                );
            }
            Ok(())
        }
        Backend::ClaudeCode => validate_claude_model(model, role),
        Backend::GeminiCli => validate_gemini_model(model, role),
    }
}

pub fn canonicalize_model_choice(
    backend: Backend,
    model: &str,
    role: &str,
) -> Result<String, String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err(format!("Invalid model for {}: model cannot be empty.", role));
    }

    match backend {
        Backend::Copilot => Ok(trimmed.to_string()),
        Backend::ClaudeCode => {
            let normalized = trimmed.replace('.', "-");
            validate_claude_model(&normalized, role)?;
            Ok(normalized)
        }
        Backend::GeminiCli => {
            validate_gemini_model(trimmed, role)?;
            Ok(trimmed.to_string())
        }
    }
}

fn validate_claude_model(model: &str, role: &str) -> Result<(), String> {
    let known = claude_code_model_ids();
    if known.contains(&model) {
        return Ok(());
    }
    if model.starts_with("claude-") && !model.contains('.') {
        eprintln!(
            "Warning: {} uses unrecognized Claude model '{}'. It may work with a newer Claude CLI, but it is not in the known list.",
            role, model
        );
        return Ok(());
    }
    Err(format!(
        "Invalid model '{}' for {} on the Claude Code backend. Claude Code only supports Claude models (for example: opus, sonnet, claude-sonnet-4-6). Run 'bod init' to reconfigure.",
        model, role
    ))
}

fn validate_gemini_model(model: &str, role: &str) -> Result<(), String> {
    let known = gemini_cli_model_ids();
    if known.contains(&model) {
        return Ok(());
    }
    if model.starts_with("gemini-") {
        eprintln!(
            "Warning: {} uses unrecognized Gemini model '{}'. It may work with a newer Gemini CLI, but it is not in the known list.",
            role, model
        );
        return Ok(());
    }
    Err(format!(
        "Invalid model '{}' for {} on the Gemini CLI backend. Gemini CLI models should be one of the known aliases (auto, pro, flash, flash-lite) or start with 'gemini-'. Run 'bod init' to reconfigure.",
        model, role
    ))
}

pub fn claude_code_model_ids() -> &'static [&'static str] {
    &[
        "opus",
        "sonnet",
        "haiku",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-sonnet-4-5",
        "claude-haiku-4-5",
    ]
}

pub fn gemini_cli_model_ids() -> &'static [&'static str] {
    &[
        "auto",
        "pro",
        "flash",
        "flash-lite",
        "gemini-2.5-pro",
        "gemini-2.5-flash",
        "gemini-2.5-flash-lite",
        "gemini-3-pro-preview",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_backends_are_unique_and_sorted() {
        let config = Config {
            review: ReviewConfig {
                models: vec![
                    ModelEntry {
                        codename: "a".to_string(),
                        backend: Backend::GeminiCli,
                        model: "flash".to_string(),
                        personality: PersonalityConfig::default(),
                    },
                    ModelEntry {
                        codename: "b".to_string(),
                        backend: Backend::Copilot,
                        model: "gpt-5.3-codex".to_string(),
                        personality: PersonalityConfig::default(),
                    },
                ],
            },
            consolidate: ConsolidateConfig {
                backend: Backend::ClaudeCode,
                model: "sonnet".to_string(),
                personality: PersonalityConfig::default(),
            },
            bugfix: BugfixConfig {
                backend: Backend::Copilot,
                model: "gpt-5.2".to_string(),
            },
        };

        assert_eq!(
            config.used_backends(),
            vec![Backend::Copilot, Backend::ClaudeCode, Backend::GeminiCli]
        );
    }

    #[test]
    fn rejects_non_claude_model_for_claude_backend() {
        let config = Config {
            review: ReviewConfig::default(),
            consolidate: ConsolidateConfig::default(),
            bugfix: BugfixConfig {
                backend: Backend::ClaudeCode,
                model: "gpt-5.3-codex".to_string(),
            },
        };

        let error = validate_models_for_backend(&config).unwrap_err();
        assert!(error.contains("Claude Code backend"));
    }

    #[test]
    fn rejects_non_gemini_model_for_gemini_backend() {
        let config = Config {
            review: ReviewConfig {
                models: vec![ModelEntry {
                    codename: "gem".to_string(),
                    backend: Backend::GeminiCli,
                    model: "claude-sonnet-4-6".to_string(),
                    personality: PersonalityConfig::default(),
                }],
            },
            consolidate: ConsolidateConfig::default(),
            bugfix: BugfixConfig::default(),
        };

        let error = validate_models_for_backend(&config).unwrap_err();
        assert!(error.contains("Gemini CLI backend"));
    }

    #[test]
    fn rejects_duplicate_review_codenames() {
        let config = Config {
            review: ReviewConfig {
                models: vec![
                    ModelEntry {
                        codename: "alpha".to_string(),
                        backend: Backend::Copilot,
                        model: "gpt-5.3-codex".to_string(),
                        personality: PersonalityConfig::default(),
                    },
                    ModelEntry {
                        codename: "alpha".to_string(),
                        backend: Backend::GeminiCli,
                        model: "flash".to_string(),
                        personality: PersonalityConfig::default(),
                    },
                ],
            },
            consolidate: ConsolidateConfig::default(),
            bugfix: BugfixConfig::default(),
        };

        let error = validate_models_for_backend(&config).unwrap_err();
        assert!(error.contains("Duplicate codename(s)"));
    }

    #[test]
    fn missing_personality_fields_fail_to_parse() {
        let content = r#"
[review]
models = [
  { codename = "opus", backend = "claude-code", model = "claude-opus-4-6" },
]

[consolidate]
backend = "claude-code"
model = "claude-sonnet-4-6"

[bugfix]
backend = "copilot"
model = "gpt-4o"
"#;

        let error = parse_config_content(content, "test.toml").unwrap_err();
        assert!(error.contains("personality"));
    }

    #[test]
    fn underscore_backend_names_fail_to_parse() {
        let content = r#"
[review]
models = [
  { codename = "opus", backend = "claude_code", model = "claude-opus-4-6", personality = { name = "default" } },
]

[consolidate]
backend = "claude_code"
model = "claude-sonnet-4-6"
personality = { name = "default" }

[bugfix]
backend = "copilot"
model = "gpt-4o"
"#;

        let error = parse_config_content(content, "test.toml").unwrap_err();
        assert!(error.contains("claude_code"));
    }

    #[test]
    fn dotted_claude_models_are_rejected_for_claude_backend() {
        let config = Config {
            review: ReviewConfig {
                models: vec![ModelEntry {
                    codename: "claude".to_string(),
                    backend: Backend::ClaudeCode,
                    model: "claude-opus-4.6".to_string(),
                    personality: PersonalityConfig::default(),
                }],
            },
            consolidate: ConsolidateConfig::default(),
            bugfix: BugfixConfig::default(),
        };

        let error = validate_models_for_backend(&config).unwrap_err();
        assert!(error.contains("Claude Code backend"));
    }

    #[test]
    fn canonicalize_model_choice_normalizes_dotted_claude_ids() {
        let model = canonicalize_model_choice(
            Backend::ClaudeCode,
            "claude-opus-4.6",
            "reviewer 'claude'",
        )
        .unwrap();

        assert_eq!(model, "claude-opus-4-6");
    }

    #[test]
    fn parses_nested_personality_tables_for_reviewers_and_consolidator() {
        let content = r#"
[[review.models]]
codename = "mini"
backend = "copilot"
model = "gpt-5.4-mini"

[review.models.personality]
name = "default"

[[review.models]]
codename = "sonnet"
backend = "copilot"
model = "claude-sonnet-4.6"

[review.models.personality]
name = "architectural-sanity-check"

[[review.models]]
codename = "gemini"
backend = "gemini-cli"
model = "auto"

[review.models.personality]
name = "devils-advocate"

[consolidate]
backend = "copilot"
model = "gpt-5.4-mini"

[consolidate.personality]
name = "systems-guru"

[bugfix]
backend = "copilot"
model = "gpt-5.4"
"#;

        let parsed = parse_config_content(content, "test.toml").unwrap();
        assert_eq!(parsed.review.models.len(), 3);
        assert_eq!(parsed.review.models[1].personality.name, "architectural-sanity-check");
        assert_eq!(parsed.consolidate.personality.name, "systems-guru");
    }
}
