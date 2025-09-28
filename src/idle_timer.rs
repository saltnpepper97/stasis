use std::{collections::HashSet, fs, time::{Duration, Instant}};
use tokio::sync::Mutex;
use std::sync::Arc;

use crate::config::{IdleAction, IdleActionKind, IdleConfig};
use crate::log::log_message;
use crate::brightness::{capture_brightness, restore_brightness, BrightnessState};


pub struct IdleTimer {
    is_laptop: bool,
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

        // Try to detect power state, with retries for boot scenarios       
        let is_laptop = crate::utils::is_laptop();
        let on_ac = if is_laptop {
            detect_power_state_with_retry(is_laptop)
        } else {
            log_message("Desktop detected, skipping AC/Battery detection");
            false // or true, doesn't matter, just won't change
        };

        
        let actions = if !ac_actions.is_empty() || !battery_actions.is_empty() {
            if on_ac {
                log_message("Initializing with AC power actions");
                ac_actions.clone()
            } else {
                log_message("Initializing with battery power actions");
                battery_actions.clone()
            }
        } else {
            log_message("Initializing with default actions");
            default_actions.clone()
        };

        let is_idle_flags = vec![false; actions.len()];

        log_message(&format!("IdleTimer initialized: {} actions, on_ac: {}", actions.len(), on_ac));

        let mut timer = Self {
            is_laptop,
            last_activity: Instant::now(),
            actions,
            ac_actions,
            battery_actions,
            resume_command: cfg.resume_command.clone(),
            pre_suspend_command: cfg.pre_suspend_command.clone(),
            is_idle_flags,
            compositor_managed: false,
            active_kinds: HashSet::new(),
            previous_brightness: None,
            previous_dpms: false,
            on_ac,
            paused: false,
        };

        // Trigger all timeout=0 actions immediately during initialization
        timer.trigger_instant_actions();
        
        timer
    }

    /// Trigger all actions with timeout_seconds == 0 exactly once
    fn trigger_instant_actions(&mut self) {
        // Collect instant actions first to avoid borrow checker issues
        let mut instant_actions = Vec::new();
        for (i, action) in self.actions.iter().enumerate() {
            if action.timeout_seconds == 0 && !self.is_idle_flags[i] {
                instant_actions.push((i, action.clone()));
            }
        }

        // Now process the instant actions
        for (i, action) in instant_actions {
            self.is_idle_flags[i] = true;
            self.active_kinds.insert(action.kind.to_string());
            
            log_message(&format!("Instant action triggered: {}", action.command));
            
            // Handle brightness capture for instant brightness actions
            if action.kind == IdleActionKind::Brightness && self.previous_brightness.is_none() {
                if let Some(state) = capture_brightness() {
                    self.previous_brightness = Some(state.clone());
                    log_message(&format!("Captured current brightness: {} (device: {})", state.value, state.device));
                } else {
                    log_message("Warning: Could not capture current brightness");
                }
            }
            
            // Handle DPMS state for instant DPMS actions
            if action.kind == IdleActionKind::Dpms && !self.previous_dpms {
                self.previous_dpms = true;
            }
            
            crate::actions::on_idle_timeout(&action, Some(self));
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

        // Don't re-trigger instant actions on reset - they should only run once per power state/config
    }

    /// Check which idle actions should trigger (excluding timeout=0 actions)
    pub fn check_idle(&mut self) {
        if self.paused {
            return; // do nothing while paused
        }
        
        let elapsed = Instant::now().duration_since(self.last_activity);

        for i in 0..self.actions.len() {
            let action = &self.actions[i];
            let key = action.kind.to_string();

            // Skip timeout=0 actions - they should only be triggered once during init or power change
            if action.timeout_seconds == 0 {
                continue;
            }

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
        if !self.is_laptop {
            return; // skip desktops entirely
        }

        if self.on_ac != on_ac {
            log_message(&format!("Power source changed: {}", if on_ac { "AC" } else { "Battery" }));
            self.on_ac = on_ac;

            // Note: We're switching to a completely new action set, so we don't need to preserve flags

            // Restore any saved brightness state before switching
            if let Some(state) = self.previous_brightness.take() {
                restore_brightness(&state);
            }

            // Switch the current idle actions
            self.actions = if on_ac {
                self.ac_actions.clone()
            } else {
                self.battery_actions.clone()
            };

            // Reset flags for new action set
            self.is_idle_flags = vec![false; self.actions.len()];

            // Clear active kinds to allow new actions
            self.active_kinds.clear();
            self.previous_brightness = None;
            self.previous_dpms = false;

            // Trigger instant actions (timeout=0) for the new power state exactly once
            self.trigger_instant_actions();
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
                // Only re-trigger instant actions if explicitly rewinding timers
                self.trigger_instant_actions(); 
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
            // Reset timer state but don't re-trigger instant actions
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
            .filter(|a| a.timeout_seconds > 0) // Exclude timeout=0 actions from shortest timeout calculation
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
        let default_actions: Vec<_> = cfg.actions.iter()
            .filter(|(k, _)| !k.starts_with("ac.") && !k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect();

        self.ac_actions = cfg.actions.iter()
            .filter(|(k, _)| k.starts_with("ac."))
            .map(|(_, v)| v.clone())
            .collect();

        self.battery_actions = cfg.actions.iter()
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

        self.is_idle_flags = vec![false; self.actions.len()];
        self.resume_command = cfg.resume_command.clone();
        self.pre_suspend_command = cfg.pre_suspend_command.clone();
        self.last_activity = Instant::now();
        self.active_kinds.clear();
        self.previous_brightness = None;
        self.previous_dpms = false;

        // Trigger instant actions after config reload
        self.trigger_instant_actions();

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
            } else if action.timeout_seconds == 0 {
                "INSTANT"
            } else if elapsed >= Duration::from_secs(action.timeout_seconds) {
                "READY"
            } else {
                "WAITING"
            };
            log_message(&format!("  Action {}: {} ({}s) - {}", i, action.command, action.timeout_seconds, status));
        }
    }
}

/// Detect power state with retries for boot scenarios
fn detect_power_state_with_retry(is_laptop: bool) -> bool {
    if !crate::utils::is_laptop() {
        log_message("Desktop detected, assuming AC power");
        return true;
    }

    for attempt in 1..=6 {
        let on_ac = is_on_ac_power(is_laptop);
        log_message(&format!("Power detection attempt {}: {}", attempt, if on_ac { "AC" } else { "Battery" }));
        if on_ac { return true; }
        if attempt < 6 { std::thread::sleep(Duration::from_millis(500)); }
    }
    
    log_message("Could not detect AC power after retries, defaulting to battery");
    false
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
        
        // Initialize last_power_state with current state to prevent false changes
        let mut last_power_state = {
            let timer = idle_timer.lock().await;
            if timer.is_laptop {
                Some(timer.on_ac)  // Use the state that was already determined during initialization
            } else {
                Some(true)  // Desktop always on AC
            }
        };

        // Give the system a moment to settle on startup
        tokio::time::sleep(Duration::from_millis(1000)).await;

        loop {
            ticker.tick().await;
            let mut timer = idle_timer.lock().await;

            // --- check AC/Battery ---
            if timer.is_laptop {
                let on_ac = is_on_ac_power(timer.is_laptop);

                // Only update if there's an actual change
                if last_power_state != Some(on_ac) {
                    log_message(&format!("Power state change detected: {} -> {}", 
                        match last_power_state {
                            Some(true) => "AC",
                            Some(false) => "Battery", 
                            None => "Unknown"
                        },
                        if on_ac { "AC" } else { "Battery" }
                    ));
                    timer.update_power_source(on_ac);
                    last_power_state = Some(on_ac);
                }
            }

            // --- check idle ---
            timer.check_idle();
        }
    });
}

/// Check if system is on AC power
pub fn is_on_ac_power(is_laptop: bool) -> bool {
    if !is_laptop {
        // Desktop: assume always on AC, no log spam
        return true;
    }

    // Method 1: Check AC adapters
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply/") {
        let mut ac_found = false;
        let mut battery_charging_or_full = false;
        
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy();
            
            // Read the type file to determine what kind of power supply this is
            let type_file = path.join("type");
            if let Ok(supply_type) = fs::read_to_string(&type_file) {
                let supply_type = supply_type.trim();
                
                // Check for AC/Mains power supplies
                if supply_type == "Mains" {
                    let online_file = path.join("online");
                    if let Ok(online_status) = fs::read_to_string(&online_file) {
                        if online_status.trim() == "1" {
                            log_message(&format!("AC adapter found online: {}", name));
                            ac_found = true;
                        }
                    }
                }
                
                // Check battery status as secondary indicator
                if supply_type == "Battery" {
                    let status_file = path.join("status");
                    if let Ok(status) = fs::read_to_string(&status_file) {
                        let status = status.trim();
                        if status == "Charging" || status == "Full" {
                            battery_charging_or_full = true;
                            log_message(&format!("Battery status: {} ({})", status, name));
                        }
                    }
                }
            }
        }
        
        // Primary method: AC adapter online
        if ac_found {
            return true;
        }
        
        // Fallback: If no AC adapter found but battery is charging/full, assume on AC
        // This helps with systems where AC detection is unreliable
        if battery_charging_or_full {
            log_message("No AC adapter detected, but battery is charging/full - assuming on AC");
            return true;
        }
    }
    
    // Method 2: Fallback - check legacy AC paths with broader naming
    let potential_ac_names = [
        "AC", "ADP", "ACAD", "AC0", "ADP1", "ACPI0003", 
        "ACPI0004", "ADP0", "AC1", "ACADAPTER"
    ];
    
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply/") {
        for entry in entries.filter_map(|e| e.ok()) {
            let name_os = entry.file_name();
            let name = name_os.to_string_lossy().to_string();
            
            // Check if name matches any known AC adapter patterns
            if potential_ac_names.iter().any(|&ac_name| 
                name.starts_with(ac_name) || name.contains(ac_name)) {
                
                let online_file = entry.path().join("online");
                if let Ok(status) = fs::read_to_string(&online_file) {
                    if status.trim() == "1" {
                        log_message(&format!("Legacy AC detection found: {}", name));
                        return true;
                    }
                }
                }
        }
    }
    
    false
}
