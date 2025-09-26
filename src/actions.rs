use std::process::Command;
use eyre::Result;

use crate::utils::log_to_cache;
use crate::config::IdleAction;

/// Called when the idle timer reaches any timeout
pub fn on_idle_timeout(action: &IdleAction) {
    log_to_cache(&format!("[Stasis] Idle timeout reached for '{}'", action.command));

    let cmd = action.command.clone();

    tokio::spawn(async move {
        if let Err(e) = run_command_silent(&cmd) {
            eprintln!("[Stasis] Failed to run command '{}': {}", cmd, e);
        }
    });
}

/// Helper: run command asynchronously and pipe output to a temp log
pub fn run_command_silent(cmd: &str) -> Result<()> {
    let log_file = "/tmp/stasis.log";

    Command::new("sh")
        .arg("-c")
        .arg(format!("{cmd} >> {log_file} 2>&1"))
        .spawn()?; // spawn async
    Ok(())
}
