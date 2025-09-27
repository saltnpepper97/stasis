use std::process::{Command, Stdio};
use eyre::Result;
use crate::utils::{log_message, log_error_message};
use crate::config::{IdleAction, IdleActionKind};
use crate::idle_timer::IdleTimer;

/// Called when the idle timer reaches any timeout
pub fn on_idle_timeout(action: &IdleAction, idle_timer: Option<&IdleTimer>) {
    log_message(&format!("Idle timeout reached for '{}'", action.command));

    let cmd = action.command.clone();
    let kind = action.kind.clone();

    // Trigger pre-suspend command if this action is Suspend
    if kind == IdleActionKind::Suspend {
        if let Some(timer) = idle_timer {
            timer.trigger_pre_suspend();
        }
    }

    // If this is a lock screen action, skip if already running
    if kind == IdleActionKind::LockScreen && is_process_running(&cmd) {
        log_message("Lockscreen already running, skipping action.");
        return;
    }

    // Run the main action command asynchronously
    tokio::spawn(async move {
        if let Err(e) = run_command_silent(&cmd) {
            log_error_message(&format!("Failed to run command '{}': {}", cmd, e));
        }
    });
}

/// Run command asynchronously and pipe output to a temp log
pub fn run_command_silent(cmd: &str) -> Result<()> {
    let log_file = "/tmp/stasis.log";
    Command::new("sh")
        .arg("-c")
        .arg(format!("{cmd} >> {log_file} 2>&1"))
        .envs(std::env::vars()) // propagate full env
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// Returns true if the first word of `cmd` is already running
fn is_process_running(cmd: &str) -> bool {
    if cmd.is_empty() {
        return false;
    }

    let first_word = cmd.split_whitespace().next().unwrap_or("");
    if first_word.is_empty() {
        return false;
    }

    match Command::new("pgrep").arg(first_word).output() {
        Ok(output) => !output.stdout.is_empty(),
        Err(_) => false,
    }
}
