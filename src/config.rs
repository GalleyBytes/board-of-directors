use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub review: ReviewConfig,
    pub consolidate: ConsolidateConfig,
    pub bugfix: BugfixConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ReviewConfig {
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelEntry {
    pub codename: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ConsolidateConfig {
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BugfixConfig {
    pub model: String,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            models: vec![
                ModelEntry {
                    codename: "opus".to_string(),
                    model: "claude-opus-4.6".to_string(),
                },
                ModelEntry {
                    codename: "gemini".to_string(),
                    model: "gemini-3-pro-preview".to_string(),
                },
                ModelEntry {
                    codename: "codex".to_string(),
                    model: "gpt-5.3-codex".to_string(),
                },
            ],
        }
    }
}

impl Default for ConsolidateConfig {
    fn default() -> Self {
        Self {
            model: "claude-opus-4.6".to_string(),
        }
    }
}

impl Default for BugfixConfig {
    fn default() -> Self {
        Self {
            model: "gpt-5.3-codex".to_string(),
        }
    }
}

const LOCAL_CONFIG: &str = ".bodrc.toml";

pub fn global_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("board-of-directors")
        .join(".bodrc.toml")
}

/// Load config: repo-local .bodrc.toml > global ~/.config/board-of-directors/.bodrc.toml > defaults
pub fn load(repo_root: &Path) -> Config {
    // Try repo-local first
    let local_path = repo_root.join(LOCAL_CONFIG);
    if let Some(config) = try_load(&local_path, LOCAL_CONFIG) {
        return config;
    }

    // Try global
    let global_path = global_config_path();
    if let Some(config) = try_load(&global_path, &global_path.to_string_lossy()) {
        return config;
    }

    Config::default()
}

/// Load config from global path only (for use when not in a git repo).
pub fn load_global() -> Config {
    let global_path = global_config_path();
    if let Some(config) = try_load(&global_path, &global_path.to_string_lossy()) {
        return config;
    }
    Config::default()
}

fn try_load(path: &Path, label: &str) -> Option<Config> {
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(path) {
        Ok(content) => match toml::from_str::<Config>(&content) {
            Ok(config) => {
                println!("Loaded config from {}", label);
                Some(config)
            }
            Err(e) => {
                eprintln!("Warning: failed to parse {}: {}. Skipping.", label, e);
                None
            }
        },
        Err(e) => {
            eprintln!("Warning: failed to read {}: {}. Skipping.", label, e);
            None
        }
    }
}

pub fn write_global(config: &Config) -> Result<(), String> {
    let path = global_config_path();
    write_config(config, &path)
}

pub fn write_local(config: &Config, repo_root: &Path) -> Result<(), String> {
    let path = repo_root.join(LOCAL_CONFIG);
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

pub fn global_config_exists() -> bool {
    global_config_path().exists()
}

pub fn local_config_exists(repo_root: &Path) -> bool {
    repo_root.join(LOCAL_CONFIG).exists()
}

pub fn local_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(LOCAL_CONFIG)
}
