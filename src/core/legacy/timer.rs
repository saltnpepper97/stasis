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
use super::brightness::{capture_brightness, restore_brightness, BrightnessState};
use super::pre_suspend::run_pre_suspend_sync;
use super::tasks::{cleanup_tasks, spawn_task_limited};

pub struct LegacyIdleTimer {
    pub(crate) cfg: IdleConfig,
    pub(crate) last_activity: Instant,
    pub(crate) debounce_until: Option<Instant>,
    pub(crate) idle_debounce_until: Option<Instant>,
    pub(crate) paused: bool,
    pub(crate) manually_paused: bool,
    pub(crate) resume_command: Option<String>,
    pub(crate) previous_brightness: Option<BrightnessState>,
    pub(crate) suspend_occurred: bool,
    pub(crate) spawned_tasks: Vec<JoinHandle<()>>,
    pub(crate) is_idle_flags: Vec<bool>,
    pub(crate) active_kinds: HashSet<String>,
    pub on_ac: bool,
    pub start_time: Instant,
    idle_task_handle: Option<JoinHandle<()>>,
    actions: Vec<IdleAction>,
    ac_actions: Vec<IdleAction>,
    battery_actions: Vec<IdleAction>,
    pre_suspend_command: Option<String>,
    compositor_managed: bool,
}

impl LegacyIdleTimer {
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
            idle_debounce_until: None,
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

                let requests = super::actions::prepare_action(&action).await;
                for req in requests {
                    match req {
                        super::actions::ActionRequest::PreSuspend => {
                            self.trigger_pre_suspend(false, false).await;
                        }
                        super::actions::ActionRequest::RunCommand(cmd) => {
                            let cmd_clone = cmd.clone();
                            spawn_task_limited(&mut self.spawned_tasks, async move {
                                if let Err(e) = super::actions::run_command_silent(&cmd_clone).await {
                                    log_error_message(&format!("Failed to run command '{}': {}", cmd_clone, e));
                                }
                            });
                        }
                        super::actions::ActionRequest::Skip(_) => {}
                    }
                }
            }
        })
    }

    pub async fn check_idle(&mut self) {
        if self.paused {
            return;
        }

        // post-activity debounce (unchanged)
        if let Some(until) = self.debounce_until {
            if Instant::now() < until {
                return;
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

            let timeout = Duration::from_secs(action.timeout_seconds);

            if elapsed >= timeout {
                // ---- idle debounce handling (one-shot) ----
                if let Some(until) = self.idle_debounce_until {
                    if Instant::now() < until {
                        return;
                    } else {
                        self.idle_debounce_until = None;
                    }
                } else {
                    // first time we crossed the threshold: start the one-shot debounce
                    let debounce_delay = Duration::from_secs(self.cfg.debounce_seconds as u64);
                    self.idle_debounce_until = Some(Instant::now() + debounce_delay);
                    return;
                }

                // ---- actual triggering (debounce expired) ----
                self.is_idle_flags[i] = true;
                self.active_kinds.insert(key.clone());

                if action.kind == IdleActionKind::Brightness && self.previous_brightness.is_none() {
                    if let Some(state) = capture_brightness() {
                        self.previous_brightness = Some(state.clone());
                    }
                }

                let requests = super::actions::prepare_action(action).await;
                for req in requests {
                    match req {
                        super::actions::ActionRequest::PreSuspend => {
                            self.trigger_pre_suspend(false, false).await;
                        }
                        super::actions::ActionRequest::RunCommand(cmd) => {
                            let cmd_clone = cmd.clone();
                            spawn_task_limited(&mut self.spawned_tasks, async move {
                                if let Err(e) = super::actions::run_command_silent(&cmd_clone).await {
                                    log_error_message(&format!("Failed to run command '{}': {}", cmd_clone, e));
                                }
                            });
                        }
                        super::actions::ActionRequest::Skip(_) => {}
                    }
                }
            }
        }

        cleanup_tasks(&mut self.spawned_tasks);
    }


    pub async fn update_power_source(&mut self, on_ac: bool) {
        if self.on_ac == on_ac {
            return;
        }

        self.on_ac = on_ac;
        cleanup_tasks(&mut self.spawned_tasks);

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
                let requests = super::actions::prepare_action(&action).await;
                for req in requests {
                    match req {
                        super::actions::ActionRequest::PreSuspend => {
                            self.trigger_pre_suspend(false, false).await;
                        }
                        super::actions::ActionRequest::RunCommand(cmd) => {
                            let cmd_clone = cmd.clone();
                            spawn_task_limited(&mut self.spawned_tasks, async move {
                                if let Err(e) = super::actions::run_command_silent(&cmd_clone).await {
                                    log_error_message(&format!("Failed to run command '{}': {}", cmd_clone, e));
                                }
                            });
                        }
                        super::actions::ActionRequest::Skip(_) => {}
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
 
    pub fn shortest_timeout(&self) -> Duration {
        self.actions
            .iter()
            .filter(|a| a.timeout_seconds > 0)
            .map(|a| Duration::from_secs(a.timeout_seconds))
            .min()
            .unwrap_or_else(|| Duration::from_secs(60))
    }

    pub fn set_compositor_managed(&mut self, value: bool) { self.compositor_managed = value; }

    pub fn is_compositor_managed(&self) -> bool { self.compositor_managed }

    pub fn mark_all_idle(&mut self) { self.is_idle_flags.fill(true); }

    pub async fn update_from_config(&mut self, cfg: &IdleConfig) {
        cleanup_tasks(&mut self.spawned_tasks);

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

/// Spawn main idle monitor task
pub async fn spawn_idle_task(idle_timer: Arc<Mutex<LegacyIdleTimer>>) -> JoinHandle<()> {
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

