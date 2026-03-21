use std::path::Path;
use tokio::process::Command;

pub async fn command(
    prompt: &str,
    model: &str,
    working_dir: &Path,
    allow_repo_access: bool,
    repo_root: &Path,
    state_dir: &Path,
) -> std::io::Result<Command> {
    let mut command = Command::new("copilot");
    crate::backend::apply_node_heap_limit(&mut command);
    command.current_dir(working_dir);
    command
        .arg("-p")
        .arg(prompt)
        .arg("--model")
        .arg(model)
        .arg("--allow-all")
        .arg("--add-dir")
        .arg(state_dir);
    if allow_repo_access {
        command.arg("--add-dir").arg(repo_root);
    }
    command.arg("--no-ask-user").arg("--autopilot");
    Ok(command)
}
