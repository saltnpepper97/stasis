use std::time::{Duration, Instant};
use crate::config::{IdleConfig, IdleAction};
use tokio::sync::Mutex;
use std::sync::Arc;

use crate::utils::log_to_cache;

/// Tracks user idle time and triggers callbacks on timeout and resume
pub struct IdleTimer {
    last_activity: Instant,
    actions: Vec<IdleAction>,        // multiple idle actions
    resume_command: Option<String>,  // just store the command string
    is_idle_flags: Vec<bool>,        // track each action separately
    compositor_managed: bool,
}

impl IdleTimer {
    pub fn new(cfg: &IdleConfig) -> Self {
        let actions: Vec<IdleAction> = cfg.actions.values().cloned().collect();
        let action_count = actions.len();

        Self {
            last_activity: Instant::now(),
            actions,
            resume_command: cfg.resume_command.clone(),
            is_idle_flags: vec![false; action_count],
            compositor_managed: false,
        }
    }

    pub fn reset(&mut self) {
        let was_idle = self.is_idle_flags.iter().any(|&b| b);
        self.last_activity = Instant::now();
        for flag in self.is_idle_flags.iter_mut() {
            *flag = false;
        }

        if was_idle {
            if let Some(cmd) = &self.resume_command {
            log_to_cache(&format!("[Stasis] Running resume command: {}", cmd));
                crate::actions::run_command_silent(cmd).ok();
            }
        }
    }

    pub fn check_idle(&mut self) {
        let elapsed = Instant::now().duration_since(self.last_activity);

        for (i, action) in self.actions.iter().enumerate() {
            if elapsed >= Duration::from_secs(action.timeout_seconds) && !self.is_idle_flags[i] {
                self.is_idle_flags[i] = true;
                log_to_cache(
                    &format!("[Stasis] Idle action triggered: {} ({}s elapsed)",
                    action.command,
                    elapsed.as_secs())
                );
                crate::actions::on_idle_timeout(action);
            }
        }
    }

    pub fn trigger_idle(&mut self) {
        let elapsed_secs = Instant::now().duration_since(self.last_activity).as_secs();

        for (i, action) in self.actions.iter().enumerate() {
            if !self.is_idle_flags[i] {
                self.is_idle_flags[i] = true;
                log_to_cache(
                    &format!("[Stasis] Forced idle action: {} ({}s)",
                    action.command,
                    elapsed_secs)
                );
                crate::actions::on_idle_timeout(action);
            }
        }
    }

    pub fn set_compositor_managed(&mut self, value: bool) {
        self.compositor_managed = value;
    }

    pub fn is_compositor_managed(&self) -> bool {
        self.compositor_managed
    }

    /// Return shortest timeout among actions (for Wayland notifier)
    pub fn shortest_timeout(&self) -> Duration {
        self.actions
            .iter()
            .map(|a| Duration::from_secs(a.timeout_seconds))
            .min()
            .unwrap_or_else(|| Duration::from_secs(60))
    }

    pub fn mark_all_idle(&mut self) {
        for flag in self.is_idle_flags.iter_mut() {
            *flag = true;
        }
    }
}

/// Spawn Tokio task to monitor idle/user activity
pub fn spawn_idle_task(idle_timer: Arc<Mutex<IdleTimer>>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(500));
        loop {
            ticker.tick().await;
            let mut timer = idle_timer.lock().await;
            timer.check_idle();
        }
    });
}
