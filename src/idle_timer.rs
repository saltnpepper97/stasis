use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};
use futures::future::BoxFuture;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::config::{IdleAction, IdleActionKind, IdleConfig};
use crate::log::{log_error_message, log_message};
use crate::brightness::{capture_brightness, restore_brightness, BrightnessState};

const MAX_SPAWNED_TASKS: usize = 10;

pub struct IdleTimer {
    pub cfg: IdleConfig,
    pub start_time: Instant,
    pub last_activity: Instant,
    pub debounce_until: Option<Instant>,
    pub paused: bool,
    pub manually_paused: bool,
    pub resume_command: Option<String>,
    pub on_ac: bool,
    actions: Vec<IdleAction>,
    ac_actions: Vec<IdleAction>,
    battery_actions: Vec<IdleAction>,
    pre_suspend_command: Option<String>,
    is_idle_flags: Vec<bool>,
    compositor_managed: bool,
    active_kinds: HashSet<String>,
    previous_brightness: Option<BrightnessState>,
    suspend_occurred: bool,
    spawned_tasks: Vec<JoinHandle<()>>,
    idle_task_handle: Option<JoinHandle<()>>,
}

impl IdleTimer {
    pub fn new(cfg: &IdleConfig) -> Self {
        let on_ac = true;

        let default_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|(k, _)| !k.starts_with("ac.") && !k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect();

        let ac_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("ac."))
            .map(|(_, v)| v.clone())
            .collect();

        let battery_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect();

        let actions = if !ac_actions.is_empty() || !battery_actions.is_empty() {
            if on_ac { ac_actions.clone() } else { battery_actions.clone() }
        } else {
            default_actions.clone()
        };

        let actions_clone = actions.clone();
        let now = Instant::now();
        
        let timer = Self {
            cfg: cfg.clone(),
            start_time: now,
            last_activity: now,
            debounce_until: None,
            actions,
            ac_actions,
            battery_actions,
            resume_command: cfg.resume_command.clone(),
            pre_suspend_command: cfg.pre_suspend_command.clone(),
            is_idle_flags: vec![false; actions_clone.len()],
            compositor_managed: false,
            active_kinds: HashSet::new(),
            previous_brightness: None,
            on_ac,
            paused: false,
            manually_paused: false,
            suspend_occurred: false,
            spawned_tasks: Vec::new(),
            idle_task_handle: None,
        };

        timer
    }

    pub async fn init(&mut self) {
        self.trigger_instant_actions().await;
    }

    pub fn elapsed_idle(&self) -> Duration {
        if let Some(until) = self.debounce_until {
            if Instant::now() < until {
                // still in debounce → report 0
                return Duration::ZERO;
            }
        }
        Instant::now().duration_since(self.last_activity)
    }

    pub fn trigger_instant_actions(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut instant_actions = Vec::new();
            for (i, action) in self.actions.iter().enumerate() {
                if action.timeout_seconds == 0 && !self.is_idle_flags[i] {
                    instant_actions.push((i, action.clone()));
                }
            }

            for (i, action) in instant_actions {
                self.is_idle_flags[i] = true;
                self.active_kinds.insert(action.kind.to_string());

                 log_message(&format!(
                    "Instant action triggered: kind={} command=\"{}\"",
                    action.kind, action.command
                ));

                if action.kind == IdleActionKind::Brightness && self.previous_brightness.is_none() {
                    if let Some(state) = capture_brightness() {
                        self.previous_brightness = Some(state.clone());
                    } else {
                        log_error_message("Could not capture current brightness");
                    }
                }

                let requests = crate::actions::prepare_action(&action).await;
                for req in requests {
                    match req {
                        crate::actions::ActionRequest::PreSuspend => {
                            self.trigger_pre_suspend(false, false).await;
                        }
                        crate::actions::ActionRequest::RunCommand(cmd) => {
                            let cmd_clone = cmd.clone();
                            self.spawn_task_limited(async move {
                                if let Err(e) = crate::actions::run_command_silent(&cmd_clone).await {
                                    log_error_message(&format!("Failed to run command '{}': {}", cmd_clone, e));
                                }
                            });
                        }
                        crate::actions::ActionRequest::Skip(_) => {}
                    }
                }
            }
        })
    }

    pub fn is_manually_inhibited(&self) -> bool {
        self.manually_paused
    }

    pub async fn set_manual_inhibit(&mut self, inhibit: bool) {
        if inhibit {
            self.pause(true);
        } else {
            self.resume(true);
        }
    }

    pub async fn check_idle(&mut self) {
        if self.paused {
            return;
        }

        // handle debounce first
        if let Some(until) = self.debounce_until {
            if Instant::now() < until {
                return; // still debouncing, skip idle checks
            } else {
                self.debounce_until = None;
            }
        }

        let elapsed = self.elapsed_idle();

        for i in 0..self.actions.len() {
            let action = &self.actions[i];
            let key = action.kind.to_string();

            if action.timeout_seconds == 0 || self.is_idle_flags[i] || self.active_kinds.contains(&key)
            {
                continue;
            }

            if elapsed >= Duration::from_secs(action.timeout_seconds) {
                self.is_idle_flags[i] = true;
                self.active_kinds.insert(key.clone());

                if action.kind == IdleActionKind::Brightness && self.previous_brightness.is_none() {
                    if let Some(state) = capture_brightness() {
                        self.previous_brightness = Some(state.clone());
                    }
                }

                let requests = crate::actions::prepare_action(action).await;
                for req in requests {
                    match req {
                        crate::actions::ActionRequest::PreSuspend => {
                            self.trigger_pre_suspend(false, false).await;
                        }
                        crate::actions::ActionRequest::RunCommand(cmd) => {
                            let cmd_clone = cmd.clone();
                            self.spawn_task_limited(async move {
                                if let Err(e) = crate::actions::run_command_silent(&cmd_clone).await {
                                    log_error_message(&format!("Failed to run command '{}': {}", cmd_clone, e));
                                }
                            });
                        }
                        crate::actions::ActionRequest::Skip(_) => {}
                    }
                }

            }
        }

        self.cleanup_tasks();
    }

    pub fn reset(&mut self) {
        self.last_activity = Instant::now();
        self.apply_reset();

        let debounce_delay = Duration::from_secs(3);
        self.debounce_until = Some(Instant::now() + debounce_delay);
    }


    fn apply_reset(&mut self) {
        let was_idle = self.is_idle_flags.iter().any(|&b| b);
        self.last_activity = Instant::now();
        self.cleanup_tasks();
        self.is_idle_flags.fill(false);

        if was_idle {
            if let Some(state) = &self.previous_brightness {
                restore_brightness(state);
            }

            if self.suspend_occurred {
                if let Some(cmd) = &self.resume_command {
                    let cmd_clone = cmd.clone();
                    self.spawn_task_limited(async move {
                        let _ = crate::actions::run_command_silent(&cmd_clone).await;
                    });
                }
                self.suspend_occurred = false;
            }
        }

        self.active_kinds.clear();
        self.previous_brightness = None;
    }

    pub fn spawn_task_limited<F>(&mut self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.cleanup_tasks();
        if self.spawned_tasks.len() < MAX_SPAWNED_TASKS {
            self.spawned_tasks.push(tokio::spawn(fut));
        } else {
            log_message("Max spawned tasks reached, skipping task spawn");
        }
    }

    fn cleanup_tasks(&mut self) {
        self.spawned_tasks.retain(|h| !h.is_finished());
    }

    pub async fn update_power_source(&mut self, on_ac: bool) {
        if self.on_ac == on_ac {
            return;
        }

        self.on_ac = on_ac;
        self.cleanup_tasks();

        if let Some(state) = self.previous_brightness.take() {
            restore_brightness(&state);
        }

        self.actions = if on_ac { self.ac_actions.clone() } else { self.battery_actions.clone() };
        self.is_idle_flags = vec![false; self.actions.len()];
        self.active_kinds.clear();
        self.trigger_instant_actions().await;
    }

    pub async fn trigger_idle(&mut self) {
        for i in 0..self.actions.len() {
            if !self.is_idle_flags[i] {
                self.is_idle_flags[i] = true;
                let action = self.actions[i].clone();                
                let requests = crate::actions::prepare_action(&action).await;
                for req in requests {
                    match req {
                        crate::actions::ActionRequest::PreSuspend => {
                            self.trigger_pre_suspend(false, false).await;
                        }
                        crate::actions::ActionRequest::RunCommand(cmd) => {
                            let cmd_clone = cmd.clone();
                            self.spawn_task_limited(async move {
                                if let Err(e) = crate::actions::run_command_silent(&cmd_clone).await {
                                    log_error_message(&format!("Failed to run command '{}': {}", cmd_clone, e));
                                }
                            });
                        }
                        crate::actions::ActionRequest::Skip(_) => {}
                    }
                }

            }
        }
    }

    pub async fn trigger_pre_suspend(&mut self, rewind_timers: bool, manual: bool) {
        if !manual {
            self.suspend_occurred = true;
        }

        if let Some(cmd) = &self.pre_suspend_command {
            if let Err(e) = run_pre_suspend_sync(cmd) {
                log_message(&format!("Pre-suspend command failed: {}", e));
            }

            if rewind_timers {
                self.last_activity = Instant::now();
                self.is_idle_flags.iter_mut().for_each(|f| *f = false);
                self.active_kinds.clear();
                self.trigger_instant_actions().await;
            }
        }
    }

    pub fn pause(&mut self, manually: bool) {
        if manually {
            self.manually_paused = true;
            self.paused = false; // Clear automatic pause when manually pausing
            log_message("Idle timers manually paused");
        } else {
            // Don't auto-pause if manually paused
            if !self.manually_paused {
                self.paused = true;
                log_message("Idle timers automatically paused");
            } else {
                // Silently ignore automatic pause when manually paused
            }
        }
    }

    pub fn resume(&mut self, manually: bool) {
        if manually {
            if self.manually_paused {
                self.manually_paused = false;
                self.paused = false; // Also clear automatic pause
                log_message("Idle timers manually resumed");
                
                // Reset idle state when manually resuming
                let was_idle = self.is_idle_flags.iter().any(|&b| b);
                self.last_activity = Instant::now();
                self.cleanup_tasks();
                self.is_idle_flags.fill(false);

                if was_idle {
                    if let Some(state) = &self.previous_brightness {
                        restore_brightness(state);
                    }

                    if let Some(cmd) = &self.resume_command {
                        let cmd_clone = cmd.clone();
                        self.spawn_task_limited(async move {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            let _ = crate::actions::run_command_silent(&cmd_clone).await;
                        });
                    }
                }

                self.active_kinds.clear();
                self.previous_brightness = None;
            }
        } else {
            // Don't auto-resume if manually paused
            if !self.manually_paused && self.paused {
                self.paused = false;
                log_message("Idle timers automatically resumed");
                
                // Reset idle state when automatically resuming
                let was_idle = self.is_idle_flags.iter().any(|&b| b);
                self.last_activity = Instant::now();
                self.cleanup_tasks();
                self.is_idle_flags.fill(false);

                if was_idle {
                    if let Some(state) = &self.previous_brightness {
                        restore_brightness(state);
                    }

                    if let Some(cmd) = &self.resume_command {
                        let cmd_clone = cmd.clone();
                        self.spawn_task_limited(async move {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            let _ = crate::actions::run_command_silent(&cmd_clone).await;
                        });
                    }
                }

                self.active_kinds.clear();
                self.previous_brightness = None;
            } else {
                // Silently ignore automatic resume when manually paused
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
        self.actions
            .iter()
            .filter(|a| a.timeout_seconds > 0)
            .map(|a| Duration::from_secs(a.timeout_seconds))
            .min()
            .unwrap_or_else(|| Duration::from_secs(60))
    }

    pub fn mark_all_idle(&mut self) {
        self.is_idle_flags.fill(true);
    }

    pub async fn update_from_config(&mut self, cfg: &IdleConfig) {
        self.cleanup_tasks();

        let default_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|(k, _)| !k.starts_with("ac.") && !k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect();

        self.ac_actions = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("ac."))
            .map(|(_, v)| v.clone())
            .collect();

        self.battery_actions = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect();

        self.actions = if !self.ac_actions.is_empty() || !self.battery_actions.is_empty() {
            if self.on_ac {
                self.ac_actions.clone()
            } else {
                self.battery_actions.clone()
            }
        } else {
            default_actions
        };

        self.cfg = cfg.clone();
        self.is_idle_flags = vec![false; self.actions.len()];
        self.resume_command = cfg.resume_command.clone();
        self.pre_suspend_command = cfg.pre_suspend_command.clone();
        self.last_activity = Instant::now();
        self.active_kinds.clear();
        self.previous_brightness = None;

        self.trigger_instant_actions().await;
        log_message("Idle timers reloaded from config");
    }

    pub async fn shutdown(&mut self) {
        if let Some(handle) = self.idle_task_handle.take() {
            handle.abort();
        }

        for handle in self.spawned_tasks.drain(..) {
            handle.abort();
        }
    }
}

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

/// Spawn main idle monitor task
pub async fn spawn_idle_task(idle_timer: Arc<Mutex<IdleTimer>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));

        loop {
            ticker.tick().await;
            let mut timer = idle_timer.lock().await;

            // Only check idle if not manually paused
            if !timer.manually_paused {
                timer.check_idle().await;
            }
        }
    })
}

