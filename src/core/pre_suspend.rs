/// Run a pre-suspend shell command synchronously with a short timeout.
/// Returns Err on non-zero exit or on timeout.
pub fn run_pre_suspend_sync(cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Command;
    use std::time::{Duration, Instant};
    use std::thread::sleep;

    let mut child = Command::new("sh").arg("-c").arg(cmd).spawn()?;
    let timeout = Duration::from_secs(5);
    let start = Instant::now();

    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(format!("Command exited with status: {}", status).into());
            }
            return Ok(());
        }

        if start.elapsed() > timeout {
            child.kill()?;
            return Err("Pre-suspend command timed out".into());
        }

        sleep(Duration::from_millis(100));
    }
}
