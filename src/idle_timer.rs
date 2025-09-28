use std::{collections::HashSet, fs, time::{Duration, Instant}};
use tokio::sync::Mutex;
use std::sync::Arc;

use crate::config::{IdleAction, IdleActionKind, IdleConfig};
use crate::log::log_message;
use crate::brightness::{capture_brightness, restore_brightness, BrightnessState};

#[cfg(feature = "wlroots_virtual_keyboard")]
use crate::wayland::wlroots_virtual_keyboard::VirtualKeyboard;

/// Tracks user idle time and triggers callbacks on timeout and resume
pub struct IdleTimer {
    last_activity: Instant,
    actions: Vec<IdleAction>,         // currently active actions (AC/Battery)
    #[allow(dead_code)]
    ac_actions: Vec<IdleAction>,      // only OnAc actions
    #[allow(dead_code)]
    battery_actions: Vec<IdleAction>, // only OnBattery actions
    resume_command: Option<String>,
    pre_suspend_command: Option<String>,
    is_idle_flags: Vec<bool>,
    compositor_managed: bool,
    active_kinds: HashSet<String>,
    previous_brightness: Option<BrightnessState>,
    previous_dpms: bool,               // marker: true if DPMS was triggered
    on_ac: bool,
    paused: bool,
    #[cfg(feature = "wlroots_virtual_keyboard")]
    virtual_keyboard: Option<VirtualKeyboard>,
}

impl IdleTimer {
    pub fn new(cfg: &IdleConfig) -> Self {
        // All normal idle actions
        let default_actions = cfg
            .actions
            .iter()
            .filter(|(k, _)| !k.starts_with("ac.") && !k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>();

        // Laptop AC/Battery actions
        let ac_actions = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("ac."))
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>();

        let battery_actions = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>();

        #[cfg(feature = "wlroots_virtual_keyboard")]
        let mut virtual_keyboard = Some(VirtualKeyboard::new());

        #[cfg(feature = "wlroots_virtual_keyboard")]
        if let Some(vk) = &mut virtual_keyboard {
            let conn = wayland_client::Connection::connect_to_env().unwrap();
            let mut queue = conn.new_event_queue();
            let qh = queue.handle();
            vk.init(&conn, &qh);
            queue.blocking_dispatch(vk).unwrap();
            vk.send_key(28);
        }

        Self {
            last_activity: Instant::now(),
            actions: default_actions.clone(),
            ac_actions,
            battery_actions,
            resume_command: cfg.resume_command.clone(),
            pre_suspend_command: cfg.pre_suspend_command.clone(),
            is_idle_flags: vec![false; default_actions.len()],
            compositor_managed: false,
            active_kinds: HashSet::new(),
            previous_brightness: None,
            previous_dpms: false,
            on_ac: is_on_ac_power(),
            paused: false,
            #[cfg(feature = "wlroots_virtual_keyboard")]
            virtual_keyboard,
        }
    }
    pub fn reset(&mut self) {
        let was_idle = self.is_idle_flags.iter().any(|&b| b);
        self.last_activity = Instant::now();
        for flag in self.is_idle_flags.iter_mut() {
            *flag = false;
        }

        if was_idle {
            // Restore brightness if saved
            if let Some(state) = &self.previous_brightness {
                log_message(&format!("Restoring brightness to {} (device: {})", state.value, state.device));
                restore_brightness(state);
            }

            // Restore DPMS if it was triggered
            if self.previous_dpms {
                log_message("Restoring DPMS via compositor");

                #[cfg(feature = "wlroots_virtual_keyboard")]
                if let Some(vk) = &mut self.virtual_keyboard {
                    // Only send key to wake the screen
                    log_message("Sending virtual key to wake wlroots compositor");
                    vk.send_key(28); // Enter key
                }
            }

            // Global resume command (user-defined)
            if let Some(cmd) = &self.resume_command {
                log_message(&format!("Running resume command: {}", cmd));
                crate::actions::run_command_silent(cmd).ok();
            }
        }

        self.active_kinds.clear();
        self.previous_brightness = None;
        self.previous_dpms = false;
    }

    /// Check which idle actions should trigger 
    pub fn check_idle(&mut self) {
        if self.paused {
            return; // do nothing while paused
        }
        
        let elapsed = Instant::now().duration_since(self.last_activity);

        for i in 0..self.actions.len() {
            let action = &self.actions[i];
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

                // Capture brightness only once
                if action.kind == IdleActionKind::Brightness && self.previous_brightness.is_none() {
                    if let Some(state) = capture_brightness() {
                        self.previous_brightness = Some(state.clone());
                        log_message(&format!("Captured current brightness: {} (device: {})", state.value, state.device));
                    } else {
                        log_message("Warning: Could not capture current brightness");
                    }
                }

                // Capture DPMS state only once
                if action.kind == IdleActionKind::Dpms && !self.previous_dpms {
                    self.previous_dpms = true;
                }

                // Trigger the idle action
                let action_clone = action.clone();
                crate::actions::on_idle_timeout(&action_clone, Some(self));
            }
        }
    }

    /// Switch actions when power source changes
    pub fn update_power_source(&mut self, on_ac: bool) {
        if self.on_ac != on_ac {
            log_message(&format!("Power source changed: {}", if on_ac { "AC" } else { "Battery" }));
            self.on_ac = on_ac;

            // Restore any saved brightness state
            if let Some(state) = self.previous_brightness.take() {
                restore_brightness(&state);
                self.previous_brightness = Some(state);
            }

            // Switch the current idle actions
            self.actions = if on_ac {
                self.ac_actions.clone()
            } else {
                self.battery_actions.clone()
            };

            for (i, action) in self.actions.iter().enumerate() {
                if action.timeout_seconds == 0 {
                    log_message(&format!("Instant trigger on power change: {}", action.command));
                    crate::actions::run_command_silent(&action.command).ok();
                    // Mark as triggered so check_idle won't re-run it
                    if i < self.is_idle_flags.len() {
                        self.is_idle_flags[i] = true;
                        self.active_kinds.insert(action.kind.to_string());
                    }
                }
            }
        }
    }

    /// Force all idle actions immediately
    pub fn trigger_idle(&mut self) {
        let elapsed_secs = Instant::now().duration_since(self.last_activity).as_secs();

        for i in 0..self.actions.len() {
            if !self.is_idle_flags[i] {
                self.is_idle_flags[i] = true;
                let action = &self.actions[i];

                log_message(&format!("Forced idle action: {} ({}s)", action.command, elapsed_secs));

                let action_clone = action.clone();
                crate::actions::on_idle_timeout(&action_clone, Some(self));
            }
        }
    }

    /// Run pre-suspend command; optionally rewind timers for manual trigger
    pub fn trigger_pre_suspend(&mut self, rewind_timers: bool) {
        if let Some(cmd) = &self.pre_suspend_command {
            log_message(&format!("Running pre-suspend command: {}", cmd));
            if let Err(e) = run_pre_suspend_sync(cmd) {
                log_message(&format!("Pre-suspend command failed: {}", e));
            }

            if rewind_timers {
                // Reset everything to start from first action
                self.last_activity = Instant::now();
                self.is_idle_flags.iter_mut().for_each(|f| *f = false);
                self.active_kinds.clear();
                log_message("Idle timer rewound to first action after manual pre-suspend");
            } else {
                // Preserve the exact timer state - don't change anything
                // The timer will continue naturally from where it was
                let elapsed = Instant::now().duration_since(self.last_activity);
                log_message(&format!("Pre-suspend executed, timer state preserved ({}s elapsed)", elapsed.as_secs()));
                
                // Debug: show what the next action should be
                if let Some((_i, action)) = self.get_next_action() {
                    log_message(&format!("Next action in queue: {} at {}s", action.command, action.timeout_seconds));
                } else {
                    log_message("All actions have been triggered");
                }
            }
        }
    }

    /// Get the next action that should be triggered based on current elapsed time
    pub fn get_next_action(&self) -> Option<(usize, &IdleAction)> {
        let _elapsed = Instant::now().duration_since(self.last_activity);
        
        // Create a list of actions with their index, sorted by timeout
        let mut sorted_actions: Vec<(usize, &IdleAction)> = self.actions.iter().enumerate().collect();
        sorted_actions.sort_by(|a, b| a.1.timeout_seconds.cmp(&b.1.timeout_seconds));
        
        // Find the next untriggered action in the sorted sequence
        for (original_index, action) in sorted_actions {
            if !self.is_idle_flags[original_index] {
                return Some((original_index, action));
            }
        }
        
        None // All actions have been triggered
    }

    pub fn pause(&mut self) {
        if !self.paused {
            self.paused = true;
            log_message("Idle timers paused");
        }
    }

    pub fn resume(&mut self) {
        if self.paused {
            self.paused = false;
            self.reset(); // treat resume like user activity
            log_message("Idle timers resumed");
        }
    }

    pub fn set_compositor_managed(&mut self, value: bool) {
        self.compositor_managed = value;
    }

    pub fn is_compositor_managed(&self) -> bool {
        self.compositor_managed
    }

    pub fn shortest_timeout(&self) -> Duration {
        self.actions.iter()
            .map(|a| Duration::from_secs(a.timeout_seconds))
            .min()
            .unwrap_or_else(|| Duration::from_secs(60))
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

    /// Debug method to show current timer state
    #[allow(dead_code)]
    pub fn debug_timer_state(&self) {
        let elapsed = Instant::now().duration_since(self.last_activity);
        log_message(&format!("Current elapsed time: {}s", elapsed.as_secs()));
        
        for (i, action) in self.actions.iter().enumerate() {
            let status = if self.is_idle_flags[i] {
                "TRIGGERED"
            } else if elapsed >= Duration::from_secs(action.timeout_seconds) {
                "READY"
            } else {
                "WAITING"
            };
            log_message(&format!("  Action {}: {} ({}s) - {}", i, action.command, action.timeout_seconds, status));
        }
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
        let mut last_power_state = None;

        loop {
            ticker.tick().await;
            let mut timer = idle_timer.lock().await;

            // --- check AC/Battery ---
            let on_ac = is_on_ac_power();
            if last_power_state != Some(on_ac) {
                timer.update_power_source(on_ac);
                last_power_state = Some(on_ac);
            }

            // --- check idle ---
            timer.check_idle();
        }
    });
}

/// Check if system is on AC power
pub fn is_on_ac_power() -> bool {
    let ac_paths = match fs::read_dir("/sys/class/power_supply/") {
        Ok(entries) => entries.filter_map(|e| e.ok())
                              .map(|e| e.path())
                              .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("AC"))
                              .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };

    for ac_path in ac_paths {
        let online_file = ac_path.join("online");
        if online_file.exists() {
            if let Ok(s) = fs::read_to_string(online_file) {
                if s.trim() == "1" {
                    return true;
                }
            }
        }
    }

    false
}
