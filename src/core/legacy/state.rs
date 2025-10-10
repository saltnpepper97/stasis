use std::time::{Duration, Instant};
use tokio::time::sleep;

use super::tasks::{cleanup_tasks, spawn_task_limited};
use crate::core::brightness::restore_brightness;
use crate::log::log_message;
use super::LegacyIdleTimer;

impl LegacyIdleTimer {
    /// Resets idle timer activity and sets a short debounce window.
    pub fn reset(&mut self) {
        self.last_activity = Instant::now();
        self.apply_reset();

        let debounce_delay = Duration::from_secs(self.cfg.debounce_seconds as u64);
        self.debounce_until = Some(Instant::now() + debounce_delay);
    }

    /// Internal helper for clearing idle flags, brightness state, and resuming.
    pub(crate) fn apply_reset(&mut self) {
        let was_idle = self.is_idle_flags.iter().any(|&b| b);
        self.last_activity = Instant::now();
        cleanup_tasks(&mut self.spawned_tasks);
        self.is_idle_flags.fill(false);

        // cancel any pending post-idle debounce when user becomes active again
        self.idle_debounce_until = None;

        if was_idle {
            if let Some(state) = &self.previous_brightness {
                restore_brightness(state);
            }

            if self.suspend_occurred {
                if let Some(cmd) = &self.resume_command {
                    let cmd_clone = cmd.clone();
                    spawn_task_limited(&mut self.spawned_tasks, async move {
                        let _ = super::actions::run_command_silent(&cmd_clone).await;
                    });
                }
                self.suspend_occurred = false;
            }
        }

        self.active_kinds.clear();
        self.previous_brightness = None;
    }

    /// Returns whether manual inhibition is currently active.
    pub fn is_manually_inhibited(&self) -> bool {
        self.manually_paused
    }

    /// Sets manual inhibition flag (async-safe wrapper).
    pub async fn set_manual_inhibit(&mut self, inhibit: bool) {
        if inhibit {
            self.pause(true);
        } else {
            self.resume(true);
        }
    }

    /// Pauses idle timers manually or automatically.
    pub fn pause(&mut self, manually: bool) {
        if manually {
            self.manually_paused = true;
            self.paused = false; // Clear automatic pause when manually pausing
            log_message("Idle timers manually paused");
        } else if !self.manually_paused {
            self.paused = true;
            log_message("Idle timers automatically paused");
        }
    }

    /// Resumes idle timers manually or automatically.
    pub fn resume(&mut self, manually: bool) {
        if manually {
            if self.manually_paused {
                self.manually_paused = false;
                self.paused = false;
                log_message("Idle timers manually resumed");
                self.reset_state_after_resume();
            }
        } else if !self.manually_paused && self.paused {
            self.paused = false;
            log_message("Idle timers automatically resumed");
            self.reset_state_after_resume();
        }
    }

    /// Internal helper for restoring brightness and running resume command.
    fn reset_state_after_resume(&mut self) {
        let was_idle = self.is_idle_flags.iter().any(|&b| b);
        self.last_activity = Instant::now();
        cleanup_tasks(&mut self.spawned_tasks);
        self.is_idle_flags.fill(false);

        if was_idle {
            if let Some(state) = &self.previous_brightness {
                restore_brightness(state);
            }

            if let Some(cmd) = &self.resume_command {
                let cmd_clone = cmd.clone();
                spawn_task_limited(&mut self.spawned_tasks, async move {
                    sleep(Duration::from_millis(200)).await;
                    let _ = super::actions::run_command_silent(&cmd_clone).await;
                });
            }
        }

        self.active_kinds.clear();
        self.previous_brightness = None;
    }
}

