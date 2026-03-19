use crate::claude_cli;
use crate::config::Backend;
use crate::copilot_cli;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Null device path, platform-specific. Used to override git config paths.
pub const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// Git subcommands denied by the Copilot backend (`--deny-tool=shell(git <subcmd>)`).
/// The Claude backend uses a blanket `Bash(git:*)` pattern instead (see claude_cli.rs).
pub const DENIED_GIT_SUBCOMMANDS: &[&str] = &[
    "commit",
    "push",
    "pull",
    "fetch",
    "remote",
    "rebase",
    "reset",
    "clean",
    "merge",
    "checkout",
    "switch",
    "restore",
    "apply",
    "cherry-pick",
    "revert",
    "rm",
    "branch",
    "tag",
    "stash",
    "config",
    "add",
    "update-index",
    "mv",
    "worktree",
    "init",
    "submodule",
];

/// Build and execute an agent command, returning its output.
///
/// For the Claude backend, the prompt is delivered via stdin to avoid leaking
/// source code in `ps` output on multi-user systems and to sidestep OS `ARG_MAX`
/// limits for large diffs.
///
/// For the Copilot backend, the prompt is passed as a CLI argument (`-p`).
/// This is visible in `ps` output -- a known limitation documented in the README.
/// Default agent timeout: 10 minutes.
const AGENT_TIMEOUT_SECS: u64 = 600;

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
    repo_root: &Path,
    state_dir: &Path,
) -> std::io::Result<std::process::Output> {
    match run_agent_inner_with_cancel(backend, prompt, model, repo_root, state_dir, None).await? {
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
    repo_root: &Path,
    state_dir: &Path,
    cancel_rx: &mut watch::Receiver<bool>,
) -> std::io::Result<AgentRunResult> {
    run_agent_inner_with_cancel(backend, prompt, model, repo_root, state_dir, Some(cancel_rx))
        .await
}

async fn run_agent_inner_with_cancel(
    backend: &Backend,
    prompt: &str,
    model: &str,
    repo_root: &Path,
    state_dir: &Path,
    mut cancel_rx: Option<&mut watch::Receiver<bool>>,
) -> std::io::Result<AgentRunResult> {
    let running = match backend {
        Backend::Copilot => {
            // Copilot passes prompt as a CLI argument; warn about ARG_MAX risk.
            // macOS ARG_MAX is ~1 MiB total (including environment). Warn at
            // a lower threshold there to account for environment overhead.
            let warn_threshold: usize = if cfg!(target_os = "macos") {
                250_000
            } else {
                900_000
            };
            if prompt.len() > warn_threshold {
                eprintln!(
                    "Warning: prompt is very large ({} bytes). \
                     This may exceed OS argument-size limits and cause the agent to fail to start.",
                    prompt.len()
                );
            }
            let cmd = copilot_cli::command(prompt, model, repo_root, state_dir);
            spawn_command(cmd, None)?
        }
        Backend::ClaudeCode => {
            let cmd = claude_cli::command(model, repo_root, state_dir);
            spawn_command(cmd, Some(prompt.as_bytes().to_vec()))?
        }
    };

    run_running_agent(running, cancel_rx.as_deref_mut()).await
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
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(AGENT_TIMEOUT_SECS));
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
            "Agent timed out after {} seconds. The child process may be hung \
             (network stall, API outage, or infinite tool-use loop).",
            AGENT_TIMEOUT_SECS
        ),
    )
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

    // Catch bare control bytes that aren't escape sequences.
    static CTRL: OnceLock<Regex> = OnceLock::new();
    let ctrl = CTRL.get_or_init(|| {
        Regex::new(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]").unwrap()
    });
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
        let handle = tokio::spawn(async move {
            run_running_agent(running, Some(&mut cancel_rx)).await
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancel_tx.send(true).unwrap();

        let result = handle.await.unwrap().unwrap();
        assert!(matches!(result, AgentRunResult::Cancelled));
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert!(!output_path.exists());
    }
}
