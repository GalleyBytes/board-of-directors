use crate::agents;
use crate::config::Config;
use crate::files;
use crate::git;
use std::path::PathBuf;
use tokio::process::Command;

pub async fn run(config: &Config) -> Result<(), String> {
    let repo_root = git::repo_root()?;
    let bod_dir = files::ensure_bod_dir(&repo_root)?;
    files::ensure_gitignore(&repo_root)?;

    let default_branch = git::detect_default_branch()?;
    let branch = git::current_branch()?;
    let sanitized = agents::sanitize_branch_name(&branch);

    println!(
        "Reviewing branch '{}' against '{}'...",
        branch, default_branch
    );

    let diff = git::generate_diff(&default_branch)?;

    // Truncate diff if extremely large to avoid overwhelming agents
    let max_diff_len = 100_000;
    let diff_for_prompt = if diff.len() > max_diff_len {
        println!(
            "Warning: diff is large ({} bytes), truncating to {} bytes for review.",
            diff.len(),
            max_diff_len
        );
        &diff[..diff.floor_char_boundary(max_diff_len)]
    } else {
        &diff
    };

    let mut handles = Vec::new();

    for entry in &config.review.models {
        let review_num = agents::next_review_number(&bod_dir, &entry.codename, &sanitized);
        let filename = agents::review_filename(&entry.codename, &sanitized, review_num);
        let output_path = bod_dir.join(&filename);
        let model_id = entry.model.clone();
        let codename = entry.codename.clone();
        let diff_text = diff_for_prompt.to_string();
        let out_path = output_path.clone();

        let handle = tokio::spawn(async move {
            run_agent_review(&codename, &model_id, &diff_text, &out_path).await
        });

        handles.push((filename, handle));
    }

    let mut success_count = 0;
    let mut fail_count = 0;

    for (filename, handle) in handles {
        match handle.await {
            Ok(Ok(())) => {
                println!("  [ok] {}", filename);
                success_count += 1;
            }
            Ok(Err(e)) => {
                eprintln!("  [FAIL] {}: {}", filename, e);
                fail_count += 1;
            }
            Err(e) => {
                eprintln!("  [FAIL] {}: task panicked: {}", filename, e);
                fail_count += 1;
            }
        }
    }

    println!(
        "\nReview complete: {} succeeded, {} failed.",
        success_count, fail_count
    );

    if fail_count > 0 {
        Err(format!("{} agent(s) failed.", fail_count))
    } else {
        Ok(())
    }
}

async fn run_agent_review(
    codename: &str,
    model_id: &str,
    diff: &str,
    output_path: &PathBuf,
) -> Result<(), String> {
    let output_path_str = output_path.to_string_lossy().to_string();

    let prompt = format!(
        r#"You are a senior code reviewer. Review the following git diff for a pull request.

Your task:
- Identify critical bugs, logic errors, security vulnerabilities, and correctness issues.
- Be very critical but constructive -- provide actionable feedback.
- Prioritize correctness over complexity.
- Keep your review concise enough for a human to read quickly. Do not be overly verbose.
- Format your review as markdown.
- Do NOT look at or reference any files in the .bod directory.
- Do NOT reference other reviewers or reviews.

Write your complete review to the file: {output_path_str}

Here is the diff to review:

```diff
{diff}
```"#
    );

    let output = Command::new("copilot")
        .args([
            "-p",
            &prompt,
            "--model",
            model_id,
            "--allow-all",
            "--autopilot",
        ])
        .output()
        .await
        .map_err(|e| format!("{} failed to start copilot: {}", codename, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{} copilot exited with error: {}",
            codename, stderr
        ));
    }

    // If copilot didn't write the file via tool use, write stdout as fallback
    if !output_path.exists() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Err(format!(
                "{} produced no output and did not create file",
                codename
            ));
        }
        tokio::fs::write(output_path, stdout.as_bytes())
            .await
            .map_err(|e| format!("{} failed to write review file: {}", codename, e))?;
    }

    Ok(())
}
