use crate::claude_cli;
use crate::config::{
    self, Backend, BugfixConfig, Config, ConsolidateConfig, ModelEntry, ReviewConfig,
};
use crate::gemini_cli;
use crate::personalities::{self, PersonalityConfig};
use regex::Regex;
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::Command;

/// Run `bod init`.
/// - `global`: write to ~/.config/board-of-directors/.bodrc.toml
/// - `reconfigure`: skip the "overwrite?" prompt
/// - `repo_root`: required when `global` is false (repo-scoped config, stored outside the repo)
pub fn run(global: bool, reconfigure: bool, repo_root: Option<&Path>) -> Result<(), String> {
    let config_path = if global {
        config::global_config_path()
    } else {
        let root =
            repo_root.ok_or("Not inside a git repository. Use --global for global config.")?;
        config::local_config_path(root)
    };
    let config_exists = config_path.exists();
    let existing_config_result = if config_exists {
        config::load_path_silent(&config_path)
    } else {
        Ok(None)
    };
    let existing_config = existing_config_result.as_ref().ok().and_then(|config| config.clone());

    if config_exists && !reconfigure {
        println!("A configuration already exists at:");
        println!("  {}\n", config_path.display());
        match &existing_config_result {
            Ok(Some(current)) => {
                print_config(&current);
                println!();
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("Warning: {}", e);
                eprintln!("The existing file must be replaced with a new per-role config.");
                println!();
            }
        }

        if !prompt_yes_no("Do you want to overwrite the current settings?")? {
            println!("Keeping existing configuration.");
            return Ok(());
        }
        println!();
    }

    let mut discovered_models = BTreeMap::new();
    let default_reviewer_count = existing_config
        .as_ref()
        .map(|config| config.review.models.len())
        .unwrap_or(3);

    println!("-- Reviewers --");
    let reviewer_count = prompt_reviewer_count(default_reviewer_count)?;
    println!(
        "Configure {} independent reviewer(s). Each one can use a different backend and personality.\n",
        reviewer_count
    );
    let mut review_models: Vec<ModelEntry> = Vec::new();
    let mut used_codenames: Vec<String> = Vec::new();
    for i in 1..=reviewer_count {
        println!("Reviewer #{}", i);
        let existing_reviewer = existing_config
            .as_ref()
            .and_then(|config| config.review.models.get(i - 1));
        let reviewer =
            prompt_review_model(i, existing_reviewer, &used_codenames, &mut discovered_models)?;
        used_codenames.push(reviewer.codename.clone());
        review_models.push(reviewer);
        println!();
    }

    println!("-- Consolidator --");
    let consolidate_defaults = existing_config.as_ref().map(|config| &config.consolidate);
    let (consolidate_backend, consolidate_model) =
        prompt_backend_and_model("consolidator", &mut discovered_models, consolidate_defaults.map(|entry| (entry.backend, entry.model.as_str())))?;
    let consolidate_personality = prompt_personality_selection(
        "consolidator",
        consolidate_defaults.map(|entry| &entry.personality),
    )?;
    println!();

    println!("-- Fixer --");
    let bugfix_defaults = existing_config.as_ref().map(|config| &config.bugfix);
    let (bugfix_backend, bugfix_model) = prompt_backend_and_model(
        "fixer",
        &mut discovered_models,
        bugfix_defaults.map(|entry| (entry.backend, entry.model.as_str())),
    )?;
    println!();

    let new_config = Config {
        review: ReviewConfig {
            models: review_models,
        },
        consolidate: ConsolidateConfig {
            backend: consolidate_backend,
            model: consolidate_model,
            personality: consolidate_personality,
        },
        bugfix: BugfixConfig {
            backend: bugfix_backend,
            model: bugfix_model,
        },
    };

    println!("Configuration:\n");
    print_config(&new_config);
    println!();

    if global {
        config::write_global(&new_config)?;
        println!("Saved to {}", config::global_config_path().display());
    } else {
        let root = repo_root.unwrap();
        config::write_local(&new_config, root)?;
        println!("Saved to {}", config::local_config_path(root).display());
    }

    Ok(())
}

fn print_config(config: &Config) {
    println!("  Reviewers:");
    for m in &config.review.models {
        println!(
            "    {} -> {} / {} / personality: {}",
            m.codename,
            m.backend,
            m.model,
            personalities::display_selection(&m.personality)
        );
    }
    println!(
        "  Consolidation: {} / {} / personality: {}",
        config.consolidate.backend,
        config.consolidate.model,
        personalities::display_selection(&config.consolidate.personality)
    );
    println!(
        "  Bugfix:        {} / {}",
        config.bugfix.backend, config.bugfix.model
    );
}

fn prompt_review_model(
    index: usize,
    defaults: Option<&ModelEntry>,
    used_codenames: &[String],
    discovered_models: &mut BTreeMap<Backend, Vec<String>>,
) -> Result<ModelEntry, String> {
    let (backend, model) =
        prompt_backend_and_model(
            &format!("reviewer #{}", index),
            discovered_models,
            defaults.map(|entry| (entry.backend, entry.model.as_str())),
        )?;
    let default_cn = defaults
        .map(|entry| entry.codename.clone())
        .unwrap_or_else(|| derive_codename(&model, used_codenames));
    loop {
        let codename = prompt_string_with_default(&format!("Codename for '{}'", model), &default_cn)?;
        let codename = sanitize_codename(&codename)?;
        if codename == "consolidated" || codename.starts_with("consolidated-") {
            eprintln!(
                "  Codename '{}' is reserved (conflicts with consolidated report filenames). Choose a different codename.",
                codename
            );
            continue;
        }
        if config::codename_is_duplicate(&codename, used_codenames) {
            eprintln!(
                "  Codename '{}' is already in use by another reviewer. Choose a different codename.",
                codename
            );
            continue;
        }
        let personality = prompt_personality_selection(
            &format!("reviewer #{}", index),
            defaults.map(|entry| &entry.personality),
        )?;
        return Ok(ModelEntry {
            codename,
            backend,
            model,
            personality,
        });
    }
}

fn prompt_backend_and_model(
    role_label: &str,
    discovered_models: &mut BTreeMap<Backend, Vec<String>>,
    defaults: Option<(Backend, &str)>,
) -> Result<(Backend, String), String> {
    let default_backend = defaults.map(|(backend, _)| backend);
    let backend = prompt_backend(role_label, default_backend)?;
    print_backend_warning(backend);
    let models = discover_models_for_backend_cached(backend, discovered_models)?;
    print_available_models(&models);
    let default_model = defaults.and_then(|(default_backend, model)| {
        if default_backend == backend {
            Some(model)
        } else {
            None
        }
    });
    let model = prompt_model_choice(
        backend,
        &format!("Model for {}", role_label),
        role_label,
        &models,
        default_model,
    )?;
    Ok((backend, model))
}

fn prompt_backend(role_label: &str, default: Option<Backend>) -> Result<Backend, String> {
    println!("Which CLI backend should {} use?\n", role_label);
    println!("  [1] Copilot CLI  (copilot)");
    println!("  [2] Claude Code  (claude)");
    println!("  [3] Gemini CLI   (gemini)\n");

    loop {
        match default {
            Some(default_backend) => print!(
                "Backend [{}] (1-3): ",
                backend_choice_number(default_backend)
            ),
            None => print!("Backend (1-3): "),
        }
        io::stdout()
            .flush()
            .map_err(|e| format!("IO error: {}", e))?;

        let mut input = String::new();
        let bytes = io::stdin()
            .lock()
            .read_line(&mut input)
            .map_err(|e| format!("Failed to read input: {}", e))?;
        if bytes == 0 {
            return Err("Unexpected end of input".to_string());
        }

        let trimmed = input.trim();
        if trimmed.is_empty() {
            if let Some(default_backend) = default {
                println!("  -> {}", backend_choice_label(default_backend));
                return Ok(default_backend);
            }
        }

        match trimmed {
            "1" => {
                println!("  -> Copilot CLI");
                return Ok(Backend::Copilot);
            }
            "2" => {
                println!("  -> Claude Code");
                return Ok(Backend::ClaudeCode);
            }
            "3" => {
                println!("  -> Gemini CLI");
                return Ok(Backend::GeminiCli);
            }
            other => {
                eprintln!("  Invalid choice '{}'. Enter 1, 2, or 3.", other);
            }
        }
    }
}

fn print_backend_warning(backend: Backend) {
    let _ = backend;
}

fn discover_models_for_backend_cached(
    backend: Backend,
    discovered_models: &mut BTreeMap<Backend, Vec<String>>,
) -> Result<Vec<String>, String> {
    if let Some(models) = discovered_models.get(&backend) {
        return Ok(models.clone());
    }

    let models = discover_models_for_backend(&backend)?;
    discovered_models.insert(backend, models.clone());
    Ok(models)
}

/// Discover models appropriate for the selected backend.
fn discover_models_for_backend(backend: &Backend) -> Result<Vec<String>, String> {
    match backend {
        Backend::Copilot => discover_copilot_models(),
        Backend::ClaudeCode => {
            verify_cli_version("claude", "Claude Code CLI")?;
            let help_output = Command::new("claude")
                .arg("--help")
                .output()
                .map_err(|e| format!("Failed to run 'claude --help': {}", e))?;
            let help_stdout = String::from_utf8_lossy(&help_output.stdout);
            let help_stderr = String::from_utf8_lossy(&help_output.stderr);
            claude_cli::check_required_flags(&help_stdout, &help_stderr)?;
            Ok(config::claude_code_model_ids()
                .iter()
                .map(|s| s.to_string())
                .collect())
        }
        Backend::GeminiCli => {
            verify_cli_version("gemini", "Gemini CLI")?;
            let help_output = Command::new("gemini")
                .arg("--help")
                .output()
                .map_err(|e| format!("Failed to run 'gemini --help': {}", e))?;
            let help_stdout = String::from_utf8_lossy(&help_output.stdout);
            let help_stderr = String::from_utf8_lossy(&help_output.stderr);
            gemini_cli::check_required_flags(&help_stdout, &help_stderr)?;
            Ok(config::gemini_cli_model_ids()
                .iter()
                .map(|s| s.to_string())
                .collect())
        }
    }
}

fn verify_cli_version(binary: &str, label: &str) -> Result<(), String> {
    let version_check = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| {
            format!(
                "Failed to run '{} --version': {}. Is {} installed?",
                binary, e, label
            )
        })?;
    if !version_check.status.success() {
        return Err(format!(
            "The '{}' CLI is installed but '{} --version' failed. Please verify your {} installation.",
            binary, binary, label
        ));
    }
    Ok(())
}

fn print_available_models(models: &[String]) {
    println!("Available models:\n");
    for (i, model) in models.iter().enumerate() {
        println!("  [{}] {}", i + 1, model);
    }
    println!("\nYou can also paste a custom model ID instead of choosing a number.\n");
}

/// Discover models by parsing `copilot --help`.
fn discover_copilot_models() -> Result<Vec<String>, String> {
    println!("Discovering available models from copilot...\n");

    let config_help = Command::new("copilot")
        .args(["help", "config"])
        .output()
        .map_err(|e| format!("Failed to run 'copilot help config': {}", e))?;
    let config_help_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&config_help.stdout),
        String::from_utf8_lossy(&config_help.stderr)
    );
    let models = parse_copilot_models_from_config_help(&config_help_text);
    if !models.is_empty() {
        return Ok(models);
    }

    let help_output = Command::new("copilot")
        .arg("--help")
        .output()
        .map_err(|e| format!("Failed to run 'copilot --help': {}", e))?;
    let help_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&help_output.stdout),
        String::from_utf8_lossy(&help_output.stderr)
    );
    let models = parse_copilot_models_from_flag_help(&help_text);

    if models.is_empty() {
        eprintln!(
            "Warning: could not parse models from Copilot CLI help output. Using fallback list."
        );
        return Ok(fallback_copilot_models());
    }

    Ok(models)
}

fn parse_copilot_models_from_config_help(help_text: &str) -> Vec<String> {
    let mut models = Vec::new();
    let mut in_model_section = false;
    let quoted_model = Regex::new(r#""([^"]+)""#).unwrap();

    for line in help_text.lines() {
        let trimmed = line.trim_start();
        if !in_model_section {
            if trimmed.starts_with("`model`:") {
                in_model_section = true;
            }
            continue;
        }

        if trimmed.starts_with('`') && trimmed.contains("`:") {
            break;
        }

        if let Some(caps) = quoted_model.captures(trimmed) {
            models.push(caps[1].to_string());
        }
    }

    models
}

fn parse_copilot_models_from_flag_help(help_text: &str) -> Vec<String> {
    for line in help_text.lines() {
        if !line.contains("--model") || !line.contains("choices:") {
            continue;
        }

        let quoted_model = Regex::new(r#""([^"]+)""#).unwrap();
        let models: Vec<String> = quoted_model
            .captures_iter(line)
            .map(|caps| caps[1].to_string())
            .collect();
        if !models.is_empty() {
            return models;
        }
    }

    Vec::new()
}

fn fallback_copilot_models() -> Vec<String> {
    vec![
        "claude-opus-4.6".to_string(),
        "claude-sonnet-4.6".to_string(),
        "claude-sonnet-4.5".to_string(),
        "claude-haiku-4.5".to_string(),
        "gemini-3-pro-preview".to_string(),
        "gpt-5.3-codex".to_string(),
        "gpt-5.2".to_string(),
        "gpt-4.1".to_string(),
    ]
}

fn prompt_yes_no(question: &str) -> Result<bool, String> {
    print!("{} (y/n): ", question);
    io::stdout()
        .flush()
        .map_err(|e| format!("IO error: {}", e))?;

    let mut input = String::new();
    let bytes = io::stdin()
        .lock()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {}", e))?;
    if bytes == 0 {
        return Err("Unexpected end of input".to_string());
    }

    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

fn prompt_reviewer_count(default: usize) -> Result<usize, String> {
    let default_str = default.to_string();
    loop {
        let input = prompt_string_with_default("How many reviewers should bod use", &default_str)?;
        match parse_reviewer_count_input(&input) {
            Ok(count) => return Ok(count),
            Err(e) => eprintln!("  {}", e),
        }
    }
}

fn parse_reviewer_count_input(input: &str) -> Result<usize, String> {
    let trimmed = input.trim();
    let count = trimmed
        .parse::<usize>()
        .map_err(|_| format!("'{}' is not a valid positive number of reviewers.", trimmed))?;
    if count == 0 {
        return Err("Reviewer count must be at least 1.".to_string());
    }
    Ok(count)
}

fn prompt_personality_selection(
    role_label: &str,
    default: Option<&PersonalityConfig>,
) -> Result<PersonalityConfig, String> {
    let choices = personalities::list_catalog()?;
    let default_selection = default
        .cloned()
        .unwrap_or_else(|| PersonalityConfig::named(personalities::DEFAULT_PERSONALITY));
    println!("Which personality should {} use?\n", role_label);
    for (i, choice) in choices.iter().enumerate() {
        println!(
            "  [{}] {} ({})",
            i + 1,
            choice.label,
            choice.name
        );
        println!("      {}", choice.description);
    }
    println!("  [c] Custom personality");
    println!("      Paste your own instructions and optionally save them for reuse.\n");

    loop {
        print!(
            "Personality [{}] (1-{} or c): ",
            personalities::display_selection(&default_selection),
            choices.len(),
        );
        io::stdout()
            .flush()
            .map_err(|e| format!("IO error: {}", e))?;

        let mut input = String::new();
        let bytes = io::stdin()
            .lock()
            .read_line(&mut input)
            .map_err(|e| format!("Failed to read input: {}", e))?;
        if bytes == 0 {
            return Err("Unexpected end of input".to_string());
        }

        let input = input.trim();
        if input.is_empty() {
            println!("  -> {}", personalities::display_selection(&default_selection));
            return Ok(default_selection.clone());
        }
        if input.eq_ignore_ascii_case("c") {
            return prompt_custom_personality(role_label, Some(&default_selection));
        }
        if let Ok(n) = input.parse::<usize>() {
            if n >= 1 && n <= choices.len() {
                let choice = &choices[n - 1];
                println!("  -> {} ({})", choice.label, choice.name);
                return Ok(PersonalityConfig::named(choice.name.clone()));
            }
        }

        eprintln!(
            "  Enter a number from 1 to {} or 'c' for a custom personality.",
            choices.len()
        );
    }
}

fn prompt_custom_personality(
    role_label: &str,
    default: Option<&PersonalityConfig>,
) -> Result<PersonalityConfig, String> {
    let default_name = default
        .map(|selection| selection.name.clone())
        .unwrap_or_else(|| default_custom_personality_name(role_label));
    let raw_name = prompt_string_with_default("Custom personality name", &default_name)?;
    let name = personalities::sanitize_name(&raw_name)?;
    let instructions = prompt_multiline_text(
        "Paste custom personality instructions. Finish with a line containing only END.",
    )?;

    if prompt_yes_no("Save this personality for reuse in the global personality library?")? {
        let path = personalities::global_personalities_dir().join(format!("{}.md", name));
        if path.exists()
            && !prompt_yes_no(&format!(
                "A global personality already exists at {}. Overwrite it?",
                path.display()
            ))?
        {
            println!("  Keeping existing global personality. Using inline instructions for this role.");
            return Ok(PersonalityConfig::inline(name, instructions));
        }

        let saved_path = personalities::save_global_personality(&name, &instructions)?;
        println!("  Saved global personality to {}", saved_path.display());
        return Ok(PersonalityConfig::named(name));
    }

    Ok(PersonalityConfig::inline(name, instructions))
}

fn default_custom_personality_name(role_label: &str) -> String {
    personalities::sanitize_name(&format!("{}-custom", role_label.replace('#', "")))
        .unwrap_or_else(|_| "custom-reviewer".to_string())
}

fn prompt_multiline_text(label: &str) -> Result<String, String> {
    println!("{}\n", label);
    let mut lines = Vec::new();
    loop {
        let mut input = String::new();
        let bytes = io::stdin()
            .lock()
            .read_line(&mut input)
            .map_err(|e| format!("Failed to read input: {}", e))?;
        if bytes == 0 {
            if lines.is_empty() {
                return Err("Unexpected end of input".to_string());
            }
            break;
        }

        let line = input.trim_end_matches(['\r', '\n']);
        if line == "END" {
            break;
        }
        lines.push(line.to_string());
    }

    let value = lines.join("\n").trim().to_string();
    if value.is_empty() {
        return Err("Custom personality instructions cannot be empty.".to_string());
    }
    Ok(value)
}

fn prompt_model_choice(
    backend: Backend,
    label: &str,
    role_label: &str,
    models: &[String],
    default: Option<&str>,
) -> Result<String, String> {
    loop {
        match default {
            Some(default_model) => {
                print!(
                    "{} [{}] (1-{} or custom model ID): ",
                    label,
                    default_model,
                    models.len()
                )
            }
            None => print!("{} (1-{} or custom model ID): ", label, models.len()),
        }
        io::stdout()
            .flush()
            .map_err(|e| format!("IO error: {}", e))?;

        let mut input = String::new();
        let bytes = io::stdin()
            .lock()
            .read_line(&mut input)
            .map_err(|e| format!("Failed to read input: {}", e))?;
        if bytes == 0 {
            return Err("Unexpected end of input".to_string());
        }

        let input = input.trim();
        if input.is_empty() {
            if let Some(default_model) = default {
                match config::canonicalize_model_choice(backend, default_model, role_label) {
                    Ok(model) => {
                        println!("  -> {}", model);
                        return Ok(model);
                    }
                    Err(e) => {
                        eprintln!("  {}", e);
                        continue;
                    }
                }
            }
        }
        if let Ok(n) = input.parse::<usize>() {
            if n >= 1 && n <= models.len() {
                match config::canonicalize_model_choice(backend, &models[n - 1], role_label) {
                    Ok(model) => {
                        println!("  -> {}", model);
                        return Ok(model);
                    }
                    Err(e) => {
                        eprintln!("  {}", e);
                        continue;
                    }
                }
            } else {
                eprintln!(
                    "  Invalid selection: {} is out of bounds (1-{}).",
                    n,
                    models.len()
                );
                continue;
            }
        }
        if !input.is_empty() {
            match config::canonicalize_model_choice(backend, input, role_label) {
                Ok(model) => {
                    println!("  -> {}", model);
                    return Ok(model);
                }
                Err(e) => {
                    eprintln!("  {}", e);
                    continue;
                }
            }
        }

        eprintln!(
            "  Enter a number from 1 to {} or a custom model ID.",
            models.len()
        );
    }
}

fn backend_choice_number(backend: Backend) -> &'static str {
    match backend {
        Backend::Copilot => "1",
        Backend::ClaudeCode => "2",
        Backend::GeminiCli => "3",
    }
}

fn backend_choice_label(backend: Backend) -> &'static str {
    match backend {
        Backend::Copilot => "Copilot CLI",
        Backend::ClaudeCode => "Claude Code",
        Backend::GeminiCli => "Gemini CLI",
    }
}

fn prompt_string_with_default(label: &str, default: &str) -> Result<String, String> {
    print!("{} [{}]: ", label, default);
    io::stdout()
        .flush()
        .map_err(|e| format!("IO error: {}", e))?;

    let mut input = String::new();
    let bytes = io::stdin()
        .lock()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {}", e))?;
    if bytes == 0 {
        return Err("Unexpected end of input".to_string());
    }

    let val = input.trim();
    if val.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(val.to_string())
    }
}

/// Sanitize a codename to contain only filesystem-safe characters `[a-zA-Z0-9_-]`.
/// Rejects codenames that produce an empty string after sanitization (e.g. `../../tmp`).
fn sanitize_codename(raw: &str) -> Result<String, String> {
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
    if sanitized.is_empty() || !sanitized.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err(format!(
            "Codename '{}' contains no valid characters. Codenames must have at least one alphanumeric character.",
            raw
        ));
    }
    Ok(sanitized)
}

/// Derive a short codename from a model ID, avoiding collisions with already-used names.
fn derive_codename(model: &str, used: &[String]) -> String {
    let base = if model.contains("opus") {
        if model.contains("fast") {
            "opus-fast"
        } else {
            "opus"
        }
    } else if model.contains("sonnet") {
        "sonnet"
    } else if model.contains("haiku") {
        "haiku"
    } else if model.starts_with("gemini")
        || model == "flash"
        || model == "flash-lite"
        || model == "pro"
        || model == "auto"
    {
        "gemini"
    } else if model.contains("codex-max") {
        "codex-max"
    } else if model.contains("codex-mini") {
        "codex-mini"
    } else if model.contains("codex") {
        "codex"
    } else if model.contains("mini") {
        "mini"
    } else {
        model
    };

    let candidate = base.to_string();
    if !used.contains(&candidate) {
        return candidate;
    }

    for i in 2..=9 {
        let suffixed = format!("{}{}", base, i);
        if !used.contains(&suffixed) {
            return suffixed;
        }
    }
    model.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_copilot_models_from_config_help_extracts_model_list() {
        let help = r#"  `model`: AI model to use for Copilot CLI; can be changed with /model command or --model flag option.
    - "claude-sonnet-4.6"
    - "gemini-3-pro-preview"

  `mouse`: whether to enable mouse support in alt screen mode; defaults to `true`.
"#;
        assert_eq!(
            parse_copilot_models_from_config_help(help),
            vec![
                "claude-sonnet-4.6".to_string(),
                "gemini-3-pro-preview".to_string()
            ]
        );
    }

    #[test]
    fn parse_copilot_models_from_flag_help_ignores_other_choice_blocks() {
        let help = r#"  --model <model>                     Set the AI model to use
  --output-format <format>            Output format: 'text' (default) or 'json'
                                      (choices: "text", "json")
"#;
        assert!(parse_copilot_models_from_flag_help(help).is_empty());
    }

    #[test]
    fn derive_codename_maps_gemini_aliases() {
        assert_eq!(derive_codename("flash", &[]), "gemini");
        assert_eq!(derive_codename("pro", &["gemini".to_string()]), "gemini2");
    }

    #[test]
    fn parse_reviewer_count_accepts_positive_numbers() {
        assert_eq!(parse_reviewer_count_input("3").unwrap(), 3);
        assert_eq!(parse_reviewer_count_input("12").unwrap(), 12);
    }

    #[test]
    fn parse_reviewer_count_rejects_zero_and_non_numbers() {
        let zero_error = parse_reviewer_count_input("0").unwrap_err();
        assert!(zero_error.contains("at least 1"));

        let invalid_error = parse_reviewer_count_input("abc").unwrap_err();
        assert!(invalid_error.contains("not a valid positive number"));
    }

    #[test]
    fn default_personality_is_first_catalog_entry() {
        let choices = personalities::list_catalog().unwrap();
        assert_eq!(choices[0].name, personalities::DEFAULT_PERSONALITY);
    }
}
