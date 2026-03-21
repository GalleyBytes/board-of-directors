use crate::claude_cli;
use crate::config::Backend;
use crate::copilot_cli;
use crate::gemini_cli;
use fs2::FileExt;
use regex::Regex;
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const NODE_HEAP_LIMIT_MB: &str = "8192";

/// Default agent timeout: 30 minutes.
///
/// Review, consolidation, and fix runs can spend several minutes in cold-start
/// work such as provider-side queueing or Gemini sandbox image pulls. A longer
/// timeout avoids failing healthy-but-slow agents during `bugfix`.
const AGENT_TIMEOUT_SECS: u64 = 1800;
const RATE_LIMIT_MAX_RETRIES: usize = 3;
const RATE_LIMIT_FALLBACK_DELAYS_SECS: [u64; RATE_LIMIT_MAX_RETRIES] = [60, 120, 180];

pub enum AgentRunResult {
    Completed(std::process::Output),
    Cancelled,
}

struct RunningAgent {
    child: tokio::process::Child,
    stdout_handle: JoinHandle<std::io::Result<Vec<u8>>>,
    stderr_handle: JoinHandle<std::io::Result<Vec<u8>>>,
    stdin_handle: Option<JoinHandle<std::io::Result<()>>>,
}

pub async fn run_agent(
    backend: &Backend,
    prompt: &str,
    model: &str,
    working_dir: &Path,
    allow_repo_access: bool,
    use_sandbox: bool,
    repo_root: &Path,
    state_dir: &Path,
) -> std::io::Result<std::process::Output> {
    match run_agent_inner_with_cancel(
        backend,
        prompt,
        model,
        working_dir,
        allow_repo_access,
        use_sandbox,
        repo_root,
        state_dir,
        None,
    )
    .await?
    {
        AgentRunResult::Completed(output) => Ok(output),
        AgentRunResult::Cancelled => Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "Agent was cancelled.",
        )),
    }
}

pub async fn run_agent_cancellable(
    backend: &Backend,
    prompt: &str,
    model: &str,
    working_dir: &Path,
    allow_repo_access: bool,
    use_sandbox: bool,
    repo_root: &Path,
    state_dir: &Path,
    cancel_rx: &mut watch::Receiver<bool>,
) -> std::io::Result<AgentRunResult> {
    run_agent_inner_with_cancel(
        backend,
        prompt,
        model,
        working_dir,
        allow_repo_access,
        use_sandbox,
        repo_root,
        state_dir,
        Some(cancel_rx),
    )
    .await
}

async fn run_agent_inner_with_cancel(
    backend: &Backend,
    prompt: &str,
    model: &str,
    working_dir: &Path,
    allow_repo_access: bool,
    use_sandbox: bool,
    repo_root: &Path,
    state_dir: &Path,
    mut cancel_rx: Option<&mut watch::Receiver<bool>>,
) -> std::io::Result<AgentRunResult> {
    let mut retry_count = 0usize;
    loop {
        let output = match run_agent_once_with_cancel(
            backend,
            prompt,
            model,
            working_dir,
            allow_repo_access,
            use_sandbox,
            repo_root,
            state_dir,
            cancel_rx.as_deref_mut(),
        )
        .await?
        {
            AgentRunResult::Completed(output) => output,
            AgentRunResult::Cancelled => return Ok(AgentRunResult::Cancelled),
        };

        if !should_retry_rate_limit(&output) {
            return Ok(AgentRunResult::Completed(output));
        }

        if retry_count >= RATE_LIMIT_MAX_RETRIES {
            let msg = format!(
                "Rate limit persisted after {} retries for backend '{}' and model '{}'. Giving up.",
                RATE_LIMIT_MAX_RETRIES, backend, model
            );
            eprintln!("{}", msg);
            // Intentional immediate error return: propagating rate-limit exhaustion
            // as an Err aborts the current run rather than returning a 'Completed'
            // state with partial/failed agent output. This prevents cascading failures
            // where subsequent steps try to parse rate-limit HTML/JSON as code or reviews.
            return Err(std::io::Error::new(std::io::ErrorKind::Other, msg));
        }

        let wait = if let Some(dur) = retry_delay_from_output(&output) {
            dur
        } else {
            // Bounds-safe fallback indexing.
            let fallback_secs = RATE_LIMIT_FALLBACK_DELAYS_SECS
                .get(retry_count)
                .copied()
                .unwrap_or_else(|| *RATE_LIMIT_FALLBACK_DELAYS_SECS.last().unwrap());
            eprintln!(
                "Failed to parse retry delay from agent output; using fallback {}s. Raw output:\nSTDOUT:\n{}\nSTDERR:\n{}",
                fallback_secs,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            Duration::from_secs(fallback_secs)
        };
        retry_count += 1;
        eprintln!(
            "Rate limit detected from backend '{}' with model '{}'. Waiting {}s before retry {}/{}.",
            backend,
            model,
            wait.as_secs(),
            retry_count,
            RATE_LIMIT_MAX_RETRIES
        );

        if !sleep_with_cancel(wait, cancel_rx.as_deref_mut()).await? {
            return Ok(AgentRunResult::Cancelled);
        }
    }
}

fn apply_git_wrapper(
    command: &mut tokio::process::Command,
    state_dir: &Path,
) -> std::io::Result<()> {
    let bin_dir = state_dir.join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    let git_wrapper_path = bin_dir.join(git_wrapper_filename());
    let real_git = resolve_git_executable(Some(&bin_dir))?;
    apply_git_wrapper_with_executable(command, &bin_dir, &git_wrapper_path, &real_git)?;

    Ok(())
}

fn apply_git_wrapper_with_executable(
    command: &mut tokio::process::Command,
    bin_dir: &Path,
    git_wrapper_path: &Path,
    real_git: &Path,
) -> std::io::Result<()> {
    let script = build_git_wrapper_script(real_git);
    write_wrapper_script(git_wrapper_path, &script)?;

    let mut paths = vec![bin_dir.to_path_buf()];
    if let Some(existing_path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing_path));
    }
    let new_path = std::env::join_paths(paths).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, e)
    })?;
    command.env("PATH", new_path);
    Ok(())
}

fn resolve_git_executable(exclude_dir: Option<&Path>) -> std::io::Result<PathBuf> {
    let path = std::env::var_os("PATH").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "PATH is not set; unable to locate git.",
        )
    })?;

    for dir in std::env::split_paths(&path) {
        for candidate in git_candidates(&dir) {
            if let Some(excluded) = exclude_dir {
                if candidate.parent() == Some(excluded) {
                    continue;
                }
            }
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Unable to locate the git executable in PATH.",
    ))
}

#[cfg(windows)]
fn git_candidates(dir: &Path) -> Vec<PathBuf> {
    vec![
        dir.join("git.exe"),
        dir.join("git.cmd"),
        dir.join("git.bat"),
        dir.join("git"),
    ]
}

#[cfg(not(windows))]
fn git_candidates(dir: &Path) -> Vec<PathBuf> {
    vec![dir.join("git")]
}

fn build_git_wrapper_script(real_git: &Path) -> String {
    #[cfg(windows)]
    {
        build_windows_git_wrapper_script(real_git)
    }
    #[cfg(not(windows))]
    {
        build_unix_git_wrapper_script(real_git)
    }
}

#[cfg(not(windows))]
fn build_unix_git_wrapper_script(real_git: &Path) -> String {
    let real_git = shell_single_quote(real_git);
    format!(
        r#"#!/bin/sh
git_subcommand=""
find_git_subcommand() {{
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --)
                shift
                break
                ;;
            -C|-c|--git-dir|--work-tree|--namespace|--separate-git-dir|--super-prefix|--exec-path)
                shift
                if [ "$#" -gt 0 ]; then
                    shift
                fi
                ;;
            -C*|-c*|--git-dir=*|--work-tree=*|--namespace=*|--separate-git-dir=*|--super-prefix=*|--exec-path=*)
                shift
                ;;
            -*)
                shift
                ;;
            *)
                git_subcommand=$1
                break
                ;;
        esac
    done
}}
find_git_subcommand "$@"
if [ "$git_subcommand" = "commit" ] || [ "$git_subcommand" = "push" ]; then
    printf '%s\n' "Git commit/push blocked by Board of Directors." >&2
    exit 1
fi
exec {real_git} "$@"
"#
    )
}

#[cfg(windows)]
fn build_windows_git_wrapper_script(real_git: &Path) -> String {
    let real_git = format!("\"{}\"", real_git.to_string_lossy());
    format!(
        r#"@echo off
setlocal EnableExtensions
set "git_subcommand="
:scan
if "%~1"=="" goto check
set "arg=%~1"
if /I "%arg%"=="--" goto after_double_dash
if /I "%arg%"=="-C" goto consume_next
if /I "%arg%"=="-c" goto consume_next
if /I "%arg%"=="--git-dir" goto consume_next
if /I "%arg%"=="--work-tree" goto consume_next
if /I "%arg%"=="--namespace" goto consume_next
if /I "%arg%"=="--separate-git-dir" goto consume_next
if /I "%arg%"=="--super-prefix" goto consume_next
if /I "%arg%"=="--exec-path" goto consume_next
if /I "%arg:~0,2%"=="-C" goto consume_one
if /I "%arg:~0,2%"=="-c" goto consume_one
if /I "%arg:~0,10%"=="--git-dir=" goto consume_one
if /I "%arg:~0,12%"=="--work-tree=" goto consume_one
if /I "%arg:~0,12%"=="--namespace=" goto consume_one
if /I "%arg:~0,19%"=="--separate-git-dir=" goto consume_one
if /I "%arg:~0,15%"=="--super-prefix=" goto consume_one
if /I "%arg:~0,12%"=="--exec-path=" goto consume_one
if "%arg:~0,1%"=="-" goto consume_one
set "git_subcommand=%arg%"
goto check
:consume_next
shift
if "%~1"=="" goto check
shift
goto scan
:consume_one
shift
goto scan
:after_double_dash
shift
if "%~1"=="" goto check
set "git_subcommand=%~1"
goto check
:check
if /I "%git_subcommand%"=="commit" goto blocked
if /I "%git_subcommand%"=="push" goto blocked
call {real_git} %*
exit /b %ERRORLEVEL%
:blocked
>&2 echo Git commit/push blocked by Board of Directors.
exit /b 1
"#
    )
}

fn shell_single_quote(value: &Path) -> String {
    let escaped = value.to_string_lossy().replace('\'', "'\"'\"'");
    format!("'{}'", escaped)
}

fn write_wrapper_script(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;
    let result = write_wrapper_script_locked(path, contents);
    let _ = lock_file.unlock();
    result
}

#[cfg(not(windows))]
fn write_wrapper_script_locked(path: &Path, contents: &str) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "git wrapper path has no parent directory",
        )
    })?;
    let stem = path.file_name().and_then(|name| name.to_str()).unwrap_or("git");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..16u32 {
        let temp_path = parent.join(format!(".{}.{}.{}.tmp", stem, nanos, attempt));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o755);
        }

        match options.open(&temp_path) {
            Ok(mut file) => {
                let write_result = file
                    .write_all(contents.as_bytes())
                    .and_then(|_| file.sync_all());
                drop(file);
                if let Err(err) = write_result {
                    let _ = std::fs::remove_file(&temp_path);
                    return Err(err);
                }
                if let Err(err) = std::fs::rename(&temp_path, path) {
                    let _ = std::fs::remove_file(&temp_path);
                    return Err(err);
                }
                return Ok(());
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Failed to create a unique temporary file for the git wrapper.",
    ))
}

#[cfg(windows)]
fn write_wrapper_script_locked(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn git_wrapper_filename() -> &'static str {
    "git.cmd"
}

#[cfg(not(windows))]
fn git_wrapper_filename() -> &'static str {
    "git"
}

async fn run_agent_once_with_cancel(
    backend: &Backend,
    prompt: &str,
    model: &str,
    working_dir: &Path,
    allow_repo_access: bool,
    use_sandbox: bool,
    repo_root: &Path,
    state_dir: &Path,
    mut cancel_rx: Option<&mut watch::Receiver<bool>>,
) -> std::io::Result<AgentRunResult> {
    let running = match backend {
        Backend::Copilot => {
            let warn_threshold: usize = if cfg!(target_os = "macos") {
                250_000
            } else {
                900_000
            };
            if prompt.len() > warn_threshold {
                eprintln!(
                    "Warning: prompt is very large ({} bytes). This may exceed OS argument-size limits and cause the agent to fail to start.",
                    prompt.len()
                );
            }
            let cmd = copilot_cli::command(
                prompt,
                model,
                working_dir,
                allow_repo_access,
                repo_root,
                state_dir,
            )
            .await?;
            let mut cmd = cmd;
            apply_git_wrapper(&mut cmd, state_dir)?;
            spawn_command(cmd, None)?
        }
        Backend::ClaudeCode => {
            let mut cmd =
                claude_cli::command(model, working_dir, allow_repo_access, repo_root, state_dir)
                    .await?;
            apply_git_wrapper(&mut cmd, state_dir)?;
            spawn_command(cmd, Some(prompt.as_bytes().to_vec()))?
        }
        Backend::GeminiCli => {
            let mut cmd = gemini_cli::command(
                model,
                working_dir,
                allow_repo_access,
                use_sandbox,
                repo_root,
                state_dir,
            )
            .await?;
            apply_git_wrapper(&mut cmd, state_dir)?;
            spawn_command(cmd, Some(prompt.as_bytes().to_vec()))?
        }
    };

    run_running_agent(running, cancel_rx.as_deref_mut()).await
}

async fn sleep_with_cancel(
    duration: Duration,
    mut cancel_rx: Option<&mut watch::Receiver<bool>>,
) -> std::io::Result<bool> {
    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);

    if let Some(cancel_rx_ref) = cancel_rx.as_deref_mut() {
        loop {
            tokio::select! {
                _ = &mut sleep => return Ok(true),
                changed = cancel_rx_ref.changed() => {
                    match changed {
                        Ok(()) if *cancel_rx_ref.borrow_and_update() => return Ok(false),
                        Err(_) => return Ok(true),
                        _ => {}
                    }
                }
            }
        }
    }

    sleep.await;
    Ok(true)
}

fn should_retry_rate_limit(output: &std::process::Output) -> bool {
    if output.status.success() {
        return false;
    }
    let combined = rate_limit_text(output);
    is_rate_limited_text(&combined)
}

fn rate_limit_text(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{}\n{}", stdout, stderr)
}

fn is_rate_limited_text(text: &str) -> bool {
    // Use regex-driven detection with word boundaries and common JSON/header forms
    static RATE_RE: OnceLock<Regex> = OnceLock::new();
    let re = RATE_RE.get_or_init(|| {
        Regex::new(r##"(?i)(?:(?:^|[^0-9.])429(?:$|[^0-9.])|http\s*/?\s*429|status\s*:\s*429|error\s*:\s*"?429(?:$|[^0-9.])|retry-after\s*:|\bretry_after\b|\btoo many requests\b|\brate limit\b|\brate-limit\b|\brate_limited\b|\bresource exhausted\b|\bresource_exhausted\b|\bquota exceeded\b)"##).unwrap()
    });
    re.is_match(text)
}

fn retry_delay_from_output(output: &std::process::Output) -> Option<Duration> {
    extract_retry_delay(&rate_limit_text(output))
}

fn extract_retry_delay(text: &str) -> Option<Duration> {
    // Prefer structured headers like "Retry-After: <seconds>"
    static RETRY_AFTER_RE: OnceLock<Regex> = OnceLock::new();
    let retry_after_re =
        RETRY_AFTER_RE.get_or_init(|| Regex::new(r"(?i)retry-after\s*:\s*(\d+)").unwrap());
    if let Some(caps) = retry_after_re.captures(text) {
        if let Ok(secs) = caps[1].parse::<u64>() {
            return Some(Duration::from_secs(secs.max(1)));
        }
    }

    // JSON-style fields: retry_after, retryAfter, retryDelay (e.g., "42s" or numeric)
    // Use a two-step, forgiving parse to avoid brittle single-regex failures.
    let key_re = Regex::new(r"(?i)(?:retry_after|retryafter|retrydelay|retry-after)").unwrap();
    if let Some(m) = key_re.find(text) {
        let suffix = &text[m.end()..];
        let val_re = Regex::new(
            r##"(?i)[:=]\s*\"?(\d+(?:\.\d+)?)(s|sec|secs|seconds|m|min|mins|minutes)?\"?"##,
        )
        .unwrap();
        if let Some(caps) = val_re.captures(suffix) {
            let val = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let unit = caps.get(2).map(|m| m.as_str()).unwrap_or("s");
            return duration_from_capture(val, unit);
        }
    }
    // Phrase-based detection with stricter boundaries.
    static PHRASE_RE: OnceLock<Regex> = OnceLock::new();
    let phrase_re = PHRASE_RE.get_or_init(|| Regex::new(r##"(?i)\b(?:retry(?: after| in)?|try again in|wait(?: for)?|available in|reset in)\b[^0-9]{0,20}\b(\d+(?:\.\d+)?)\b\s*(seconds?|secs?|s|minutes?|mins?|m)\b"##).unwrap());
    if let Some(caps) = phrase_re.captures(text) {
        return duration_from_capture(
            caps.get(1).map(|m| m.as_str())?,
            caps.get(2).map(|m| m.as_str()).unwrap_or("s"),
        );
    }

    None
}

fn duration_from_capture(value: &str, unit: &str) -> Option<Duration> {
    let value = value.parse::<f64>().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let seconds = match unit.to_ascii_lowercase().as_str() {
        "minute" | "minutes" | "min" | "mins" | "m" => value * 60.0,
        _ => value,
    };
    let seconds = seconds.ceil().max(1.0) as u64;
    Some(Duration::from_secs(seconds))
}

fn spawn_command(
    mut cmd: tokio::process::Command,
    stdin_payload: Option<Vec<u8>>,
) -> std::io::Result<RunningAgent> {
    use std::process::Stdio;
    cmd.stdin(if stdin_payload.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "Agent child stdout was not piped as expected.",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "Agent child stderr was not piped as expected.",
        )
    })?;
    let stdin_handle = stdin_payload.map(|payload| {
        let stdin = child.stdin.take();
        tokio::spawn(async move {
            if let Some(mut stdin) = stdin {
                let res = stdin.write_all(&payload).await;
                drop(stdin);
                res
            } else {
                Ok(())
            }
        })
    });

    Ok(RunningAgent {
        child,
        stdout_handle: spawn_reader_task(stdout),
        stderr_handle: spawn_reader_task(stderr),
        stdin_handle,
    })
}

async fn run_running_agent(
    mut running: RunningAgent,
    mut cancel_rx: Option<&mut watch::Receiver<bool>>,
) -> std::io::Result<AgentRunResult> {
    let timeout = tokio::time::sleep(Duration::from_secs(AGENT_TIMEOUT_SECS));
    tokio::pin!(timeout);

    loop {
        if let Some(cancel_rx_ref) = cancel_rx.as_deref_mut() {
            tokio::select! {
                status = running.child.wait() => {
                    let output = finish_agent_output(status?, running).await?;
                    return Ok(AgentRunResult::Completed(output));
                }
                _ = &mut timeout => {
                    if let Some(status) = running.child.try_wait()? {
                        let output = finish_agent_output(status, running).await?;
                        return Ok(AgentRunResult::Completed(output));
                    }
                    let status = kill_and_wait(&mut running.child).await?;
                    finish_agent_output(status, running).await?;
                    return Err(timed_out_error());
                }
                changed = cancel_rx_ref.changed() => {
                    match changed {
                        Ok(()) if *cancel_rx_ref.borrow_and_update() => {
                            if let Some(status) = running.child.try_wait()? {
                                let output = finish_agent_output(status, running).await?;
                                return Ok(AgentRunResult::Completed(output));
                            }
                            let status = kill_and_wait(&mut running.child).await?;
                            finish_agent_output(status, running).await?;
                            return Ok(AgentRunResult::Cancelled);
                        }
                        Err(_) => {
                            cancel_rx = None;
                        }
                        _ => {}
                    }
                }
            }
        } else {
            tokio::select! {
                status = running.child.wait() => {
                    let output = finish_agent_output(status?, running).await?;
                    return Ok(AgentRunResult::Completed(output));
                }
                _ = &mut timeout => {
                    if let Some(status) = running.child.try_wait()? {
                        let output = finish_agent_output(status, running).await?;
                        return Ok(AgentRunResult::Completed(output));
                    }
                    let status = kill_and_wait(&mut running.child).await?;
                    finish_agent_output(status, running).await?;
                    return Err(timed_out_error());
                }
            }
        }
    }
}

fn spawn_reader_task<R>(mut reader: R) -> JoinHandle<std::io::Result<Vec<u8>>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer).await?;
        Ok(buffer)
    })
}

async fn kill_and_wait(
    child: &mut tokio::process::Child,
) -> std::io::Result<std::process::ExitStatus> {
    match child.start_kill() {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {}
        Err(e) => return Err(e),
    }
    child.wait().await
}

async fn finish_agent_output(
    status: std::process::ExitStatus,
    running: RunningAgent,
) -> std::io::Result<std::process::Output> {
    let stdout = join_reader_task(running.stdout_handle, "stdout").await?;
    let stderr = join_reader_task(running.stderr_handle, "stderr").await?;

    if let Some(stdin_handle) = running.stdin_handle {
        match stdin_handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Ok(Err(e)) => return Err(e),
            Err(join_err) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("stdin writer task panicked: {}", join_err),
                ));
            }
        }
    }

    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

async fn join_reader_task(
    handle: JoinHandle<std::io::Result<Vec<u8>>>,
    stream_name: &str,
) -> std::io::Result<Vec<u8>> {
    match handle.await {
        Ok(result) => result,
        Err(join_err) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("agent {} reader task panicked: {}", stream_name, join_err),
        )),
    }
}

fn timed_out_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!(
            "Agent timed out after {} seconds. The child process may be hung (network stall, API outage, or infinite tool-use loop).",
            AGENT_TIMEOUT_SECS
        ),
    )
}

pub fn apply_node_heap_limit(command: &mut tokio::process::Command) {
    let existing = env::var("NODE_OPTIONS").ok();
    let combined = merge_node_options(existing.as_deref(), NODE_HEAP_LIMIT_MB);
    command.env("NODE_OPTIONS", combined);
}

fn merge_node_options(existing: Option<&str>, heap_limit_mb: &str) -> String {
    let heap_flag = format!("--max-old-space-size={heap_limit_mb}");
    match existing {
        Some(existing) if existing.contains("--max-old-space-size=") => existing.to_string(),
        Some(existing) if existing.trim().is_empty() => heap_flag,
        Some(existing) => format!("{existing} {heap_flag}"),
        None => heap_flag,
    }
}

/// Strip ANSI escape sequences and control characters from a string.
///
/// Pass 1: strip ANSI escape sequences using the `strip-ansi-escapes` crate,
/// which uses a proper VT100 state-machine parser (via `vte`). This handles
/// CSI, OSC (both BEL and ST terminated), character-set selection, keypad
/// modes, and all other standard escape sequences.
///
/// Pass 2: strip any remaining control characters (bare BEL, NUL, etc.) that
/// are not part of escape sequences but have no role in markdown content.
/// Preserves newline (0x0A), tab (0x09), and carriage return (0x0D).
pub fn strip_ansi_codes(s: &str) -> String {
    let stripped = strip_ansi_escapes::strip(s);
    let stripped = String::from_utf8_lossy(&stripped);

    static CTRL: OnceLock<Regex> = OnceLock::new();
    let ctrl = CTRL.get_or_init(|| Regex::new(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]").unwrap());
    ctrl.replace_all(&stripped, "").to_string()
}

/// Check if an I/O error is E2BIG (argument list too long).
pub fn is_arg_too_long(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::ArgumentListTooLong
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[tokio::test]
    async fn cancelling_running_agent_prevents_late_write() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("late-write.txt");

        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 2; printf late > \"$OUT_PATH\"")
            .env("OUT_PATH", &output_path);

        let running = spawn_command(command, None).unwrap();
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let handle =
            tokio::spawn(async move { run_running_agent(running, Some(&mut cancel_rx)).await });

        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_tx.send(true).unwrap();

        let result = handle.await.unwrap().unwrap();
        assert!(matches!(result, AgentRunResult::Cancelled));
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!output_path.exists());
    }

    #[cfg(unix)]
    fn write_fake_git(script_path: &Path) {
        std::fs::write(
            script_path,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$GIT_WRAPPER_OUT"
"#,
        )
        .unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(script_path, perms).unwrap();
    }

    #[cfg(unix)]
    async fn run_wrapped_git(
        fake_git_path: &Path,
        state_dir: &Path,
        command_line: &str,
        output_path: &Path,
    ) -> std::process::Output {
        let bin_dir = state_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.arg("-c").arg(command_line);
        command.env("GIT_WRAPPER_OUT", output_path);
        apply_git_wrapper_with_executable(
            &mut command,
            &bin_dir,
            &bin_dir.join("git"),
            fake_git_path,
        )
        .unwrap();
        command.env("PATH", &bin_dir);
        command.output().await.unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_wrapper_blocks_only_commit_and_push_subcommands() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let fake_git_dir = temp.path().join("fake-git");
        std::fs::create_dir_all(&fake_git_dir).unwrap();
        let fake_git_path = fake_git_dir.join("git");
        write_fake_git(&fake_git_path);

        let allowed_output = temp.path().join("allowed.txt");
        let allowed = run_wrapped_git(
            &fake_git_path,
            &state_dir,
            "git log --grep commit",
            &allowed_output,
        )
        .await;
        assert!(allowed.status.success());
        assert_eq!(
            std::fs::read_to_string(&allowed_output).unwrap(),
            "log\n--grep\ncommit\n"
        );

        let allowed_global_flag_output = temp.path().join("allowed-global-flag.txt");
        let allowed_global_flag = run_wrapped_git(
            &fake_git_path,
            &state_dir,
            "git -C /tmp status --short",
            &allowed_global_flag_output,
        )
        .await;
        assert!(allowed_global_flag.status.success());
        assert_eq!(
            std::fs::read_to_string(&allowed_global_flag_output).unwrap(),
            "-C\n/tmp\nstatus\n--short\n"
        );

        let commit_output = temp.path().join("commit.txt");
        let commit = run_wrapped_git(
            &fake_git_path,
            &state_dir,
            "git -C /tmp commit",
            &commit_output,
        )
        .await;
        assert!(!commit.status.success());
        assert!(
            String::from_utf8_lossy(&commit.stderr)
                .contains("Git commit/push blocked by Board of Directors.")
        );
        assert!(!commit_output.exists());

        let push_output = temp.path().join("push.txt");
        let push = run_wrapped_git(
            &fake_git_path,
            &state_dir,
            "git push origin main",
            &push_output,
        )
        .await;
        assert!(!push.status.success());
        assert!(
            String::from_utf8_lossy(&push.stderr)
                .contains("Git commit/push blocked by Board of Directors.")
        );
        assert!(!push_output.exists());
    }

    #[test]
    fn detects_rate_limit_text() {
        assert!(is_rate_limited_text("HTTP 429: Too Many Requests"));
        assert!(is_rate_limited_text("resource_exhausted, try again later"));
        assert!(!is_rate_limited_text("syntax error"));
        // Ensure "429" embedded in other tokens does not spuriously match
        assert!(!is_rate_limited_text("version 1.429.0"));
    }

    #[test]
    fn extracts_retry_delay_from_phrases() {
        assert_eq!(
            extract_retry_delay("Rate limit hit. Retry after 90 seconds.").map(|d| d.as_secs()),
            Some(90)
        );
        assert_eq!(
            extract_retry_delay("Too many requests. Try again in 2 minutes.").map(|d| d.as_secs()),
            Some(120)
        );
    }

    #[test]
    fn extracts_retry_delay_from_generic_json_style_values() {
        assert_eq!(
            extract_retry_delay("{\"error\":\"429\",\"retryDelay\":\"42s\"}").map(|d| d.as_secs()),
            Some(42)
        );
    }

    #[test]
    fn parses_retry_after_header() {
        let hdr = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 10\r\n";
        assert_eq!(extract_retry_delay(hdr).map(|d| d.as_secs()), Some(10));
    }

    #[test]
    fn merge_node_options_appends_heap_limit_when_missing() {
        assert_eq!(
            merge_node_options(Some("--trace-warnings"), "8192"),
            "--trace-warnings --max-old-space-size=8192"
        );
    }

    #[test]
    fn merge_node_options_preserves_existing_heap_limit() {
        assert_eq!(
            merge_node_options(Some("--max-old-space-size=4096 --trace-warnings"), "8192"),
            "--max-old-space-size=4096 --trace-warnings"
        );
    }
}
