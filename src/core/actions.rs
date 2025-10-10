use std::time::Duration;
use eyre::Result;
use tokio::process::Command;

use crate::config::{IdleAction, IdleActionKind};
use crate::log::log_message;

#[derive(Debug, Clone)]
pub enum ActionRequest {
    RunCommand(String),
    PreSuspend,
    #[allow(dead_code)]
    Skip(String),
}

pub async fn prepare_action(action: &IdleAction) -> Vec<ActionRequest> {
    let cmd = action.command.clone();
    let kind = action.kind.clone();

    match kind {
        IdleActionKind::Suspend => {
            let mut reqs = Vec::new();
            reqs.push(ActionRequest::PreSuspend);
            if !cmd.trim().is_empty() {
                reqs.push(ActionRequest::RunCommand(cmd));
            }
            reqs
        }

        IdleActionKind::LockScreen => {
            if is_process_running(&cmd).await {
                log_message("Lockscreen already running, skipping action.");
                vec![ActionRequest::Skip(cmd)]
            } else {
                vec![ActionRequest::RunCommand(cmd)]
            }
        }

        _ => {
            // Default: run the configured command if any.
            if cmd.trim().is_empty() {
                vec![]
            } else {
                vec![ActionRequest::RunCommand(cmd)]
            }
        }
    }
}

/// Run a shell command, redirecting stdout/stderr to a small log file.
pub async fn run_command_silent(cmd: &str) -> Result<()> {
    let log_file = "/tmp/stasis.log";
    let fut = async {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("{cmd} >> {log_file} 2>&1"))
            .envs(std::env::vars())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let status = child.wait().await?;
        if !status.success() {
            eyre::bail!("Command '{}' exited with status {:?}", cmd, status.code());
        }
        Ok::<(), eyre::Report>(())
    };

    tokio::time::timeout(Duration::from_secs(30), fut).await??;
    Ok(())
}


pub async fn is_process_running(cmd: &str) -> bool {
    if cmd.trim().is_empty() {
        return false;
    }
    let first_word = cmd.split_whitespace().next().unwrap_or("");
    if first_word.is_empty() {
        return false;
    }

    match Command::new("pgrep").arg(first_word).output().await {
        Ok(output) => !output.stdout.is_empty(),
        Err(_) => false,
    }
}

