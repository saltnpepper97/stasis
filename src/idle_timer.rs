use std::{collections::HashSet, time::{Duration, Instant}};
use crate::config::{IdleConfig, IdleAction};
use tokio::sync::Mutex;
use std::sync::Arc;
use crate::utils::log_message;

/// Tracks user idle time and triggers callbacks on timeout and resume
pub struct IdleTimer {
    last_activity: Instant,
    actions: Vec<IdleAction>,        // multiple idle actions
    resume_command: Option<String>, 
    pre_suspend_command: Option<String>,
    is_idle_flags: Vec<bool>,        // track each action separately
    compositor_managed: bool,
    active_kinds: HashSet<String>,   // active kinds
}

impl IdleTimer {
    pub fn new(cfg: &IdleConfig) -> Self {
        let actions: Vec<IdleAction> = cfg.actions.values().cloned().collect();
        let active_kinds = HashSet::new(); // nothing is active at startup
        let is_idle_flags = vec![false; actions.len()];

        Self {
            last_activity: Instant::now(),
            actions,
            resume_command: cfg.resume_command.clone(),
            pre_suspend_command: cfg.pre_suspend_command.clone(),
            is_idle_flags,
            compositor_managed: false,
            active_kinds,
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
                log_message(&format!("Running resume command: {}", cmd));
                crate::actions::run_command_silent(cmd).ok();
            }
        }
  
        self.active_kinds.clear(); // reset all kinds to inactive
    }

    /// Check which idle actions should trigger 
    pub fn check_idle(&mut self) {
        let elapsed = Instant::now().duration_since(self.last_activity);

        for (i, action) in self.actions.iter().enumerate() {
            let key = action.kind.to_string();

            if elapsed >= Duration::from_secs(action.timeout_seconds)
                && !self.is_idle_flags[i]
                && !self.active_kinds.contains(&key)
            {
                self.is_idle_flags[i] = true;
                self.active_kinds.insert(key.clone());

                log_message(&format!(
                    "Idle action triggered: {} ({}s elapsed)",
                    action.command,
                    elapsed.as_secs()
                ));

                crate::actions::on_idle_timeout(action, Some(self));
            }
        }
    }

    pub fn trigger_idle(&mut self) {
        let elapsed_secs = Instant::now().duration_since(self.last_activity).as_secs();

        for (i, action) in self.actions.iter().enumerate() {
            if !self.is_idle_flags[i] {
                self.is_idle_flags[i] = true;
                log_message(&format!("Forced idle action: {} ({}s)", action.command, elapsed_secs));
                crate::actions::on_idle_timeout(action, Some(self));
            }
        }
    }

    pub fn trigger_pre_suspend(&self) {
        if let Some(cmd) = &self.pre_suspend_command {
            log_message(&format!("Running pre-suspend command: {}", cmd));
            if let Err(e) = run_pre_suspend_sync(cmd) {
                log_message(&format!("Pre-suspend command failed: {}", e));
            }
        }
    }

    pub fn set_compositor_managed(&mut self, value: bool) {
        self.compositor_managed = value;
    }

    pub fn is_compositor_managed(&self) -> bool {
        self.compositor_managed
    }

    pub fn shortest_timeout(&self) -> Duration {
        self.actions.iter().map(|a| Duration::from_secs(a.timeout_seconds)).min().unwrap_or_else(|| Duration::from_secs(60))
    }

    pub fn mark_all_idle(&mut self) {
        for flag in self.is_idle_flags.iter_mut() {
            *flag = true;
        }
    }

    pub fn update_from_config(&mut self, cfg: &IdleConfig) {
        self.actions = cfg.actions.values().cloned().collect();
        self.is_idle_flags = vec![false; self.actions.len()];
        self.resume_command = cfg.resume_command.clone();
        self.last_activity = Instant::now();
        self.active_kinds = HashSet::new();

        log_message("Idle timers reloaded from config");
    }
}

/// Synchronously run pre-suspend command with 5s timeout
fn run_pre_suspend_sync(cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Command;
    use std::time::{Duration, Instant};
    
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
        std::thread::sleep(Duration::from_millis(100));
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

