// Author: Dustin Pilgrim
// License: GPL-3.0-only

use super::{AnyError, Daemon};
use crate::core::{
    action::Action,
    events::{Event, LockSource},
    manager_msg::ManagerMsg,
};
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::mpsc;

impl Daemon {
    pub(super) async fn exec_action_with_tx(
        &mut self,
        action: Action,
        tx: mpsc::Sender<ManagerMsg>,
    ) -> Result<(), AnyError> {
        match action {
            Action::RunLockScreen { command } => {
                // Already correct: spawns a task, awaits child exit, sends Locked/Unlocked.
                Self::spawn_lock_screen(tx, command);
            }

            Action::RunCommand { command } => {
                eventline::info!("run: {}", command);
                let status = Command::new("sh")
                    .arg("-lc")
                    .arg(&command)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .map_err(|e| format!("run failed to spawn '{command}': {e}"))?;

                if !status.success() {
                    eventline::warn!(
                        "run: '{}' exited with {}",
                        command,
                        status.code().unwrap_or(-1)
                    );
                }
            }

            Action::RunResumeCommand { command } => {
                // Same issue as RunCommand — must not block the executor.
                eventline::info!("resume: {}", command);
                let status = Command::new("sh")
                    .arg("-lc")
                    .arg(&command)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .map_err(|e| format!("resume failed to spawn '{command}': {e}"))?;

                if !status.success() {
                    eventline::warn!(
                        "resume: '{}' exited with {}",
                        command,
                        status.code().unwrap_or(-1)
                    );
                }
            }

            Action::Notify { message, icon } => {
                eventline::info!("notify: {}", message);
                let mut cmd = std::process::Command::new("notify-send");
                cmd.arg("-a").arg("Stasis");

                if let Some(icon) = icon.as_deref().filter(|s| !s.trim().is_empty()) {
                    cmd.arg("-i").arg(icon);
                }

                let child = cmd
                    .arg("--")
                    .arg(&message)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();

                match child {
                    Ok(child) => {
                        // Reap in a background blocking task so we don't block here
                        // and don't leak the zombie.
                        tokio::task::spawn_blocking(move || {
                            // into_inner gives us the std Child; wait() reaps it.
                            let _ = child.wait_with_output();
                        });
                    }
                    Err(e) => {
                        eventline::warn!("notify: failed to spawn notify-send: {e}");
                    }
                }
            }

            Action::Suspend => {
                // The original code only logged here and never actually suspended.
                eventline::info!("suspend: requesting system suspend via systemctl");
                let status = Command::new("systemctl")
                    .arg("suspend")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;

                match status {
                    Ok(s) if s.success() => {}
                    Ok(s) => {
                        eventline::warn!(
                            "suspend: systemctl suspend exited with {}",
                            s.code().unwrap_or(-1)
                        );
                    }
                    Err(e) => {
                        eventline::error!("suspend: failed to spawn systemctl: {e}");
                    }
                }
            }

            Action::EnterLowPower => {
                eventline::info!("low_power: entering (snapshot + apply)");
                self.low_power.enter();
            }

            Action::ExitLowPower => {
                eventline::info!("low_power: exiting (restore snapshot)");
                self.low_power.exit();
            }
        }

        Ok(())
    }

    fn spawn_lock_screen(tx: mpsc::Sender<ManagerMsg>, command: String) {
        tokio::spawn(async move {
            // Process lifetime is the compatibility fallback. A positive
            // login1 LockedHint can promote this episode to system tracking.
            let _ = tx
                .send(ManagerMsg::Event(Event::SessionLocked {
                    source: LockSource::LockerProcess,
                    now_ms: crate::core::utils::now_ms(),
                }))
                .await;

            eventline::info!("lock: {} (await exit)", command);

            let mut child = match Command::new("sh")
                .arg("-lc")
                .arg(command)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    eventline::error!("lock spawn failed: {e}");
                    let _ = tx
                        .send(ManagerMsg::Event(Event::SessionUnlocked {
                            source: LockSource::LockerProcess,
                            now_ms: crate::core::utils::now_ms(),
                        }))
                        .await;
                    return;
                }
            };

            let _ = child.wait().await;

            let _ = tx
                .send(ManagerMsg::Event(Event::SessionUnlocked {
                    source: LockSource::LockerProcess,
                    now_ms: crate::core::utils::now_ms(),
                }))
                .await;
        });
    }
}
