use std::{collections::HashSet, sync::Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tokio::process::Command;
use serde_json::Value;
use sysinfo::{System, RefreshKind, ProcessRefreshKind, ProcessesToUpdate};

use crate::config::IdleConfig;
use crate::utils::log_to_cache;

/// Tracks currently running apps to inhibit idle
pub struct AppInhibitor {
    cfg: Arc<IdleConfig>,
    system: System,
    active_apps: HashSet<String>
}

impl AppInhibitor {
    pub fn new(cfg: Arc<IdleConfig>) -> Self {
        let mut system = System::new_with_specifics(RefreshKind::everything());
        system.refresh_all();
        Self { cfg, system, active_apps: HashSet::new() }
    }

    /// Returns true if any app in inhibit_apps is currently running
    pub async fn is_any_app_running(&mut self) -> bool {
        let mut new_active_apps = HashSet::new();

        let running = match self.check_compositor_windows().await {
            Ok(result_apps) => {
                new_active_apps = result_apps;
                !new_active_apps.is_empty()
            },
            Err(_) => {
                // Fallback to processes
                let running = self.check_processes_with_tracking(&mut new_active_apps);
                running
            }
        };

        // Only print new apps that weren't active last time
        for app in &new_active_apps {
            if !self.active_apps.contains(app) {
                log_to_cache(&format!("[Stasis] App inhibit active: {}", app));
            }
        }

        // Update active_apps for next check
        self.active_apps = new_active_apps;

        running
    }

    fn check_processes_with_tracking(&mut self, new_active_apps: &mut HashSet<String>) -> bool {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::everything(),
        );

        let mut any_running = false;

        for process in self.system.processes().values() {
            let exe_path = process.exe();
            let proc_name = exe_path
                .and_then(|p| p.file_name())
                .unwrap_or_else(|| std::ffi::OsStr::new(""))
                .to_string_lossy();
            let full_path = exe_path.map(|p| p.to_string_lossy()).unwrap_or_else(|| "".into());

            for pattern in &self.cfg.inhibit_apps {
                let matched = match pattern {
                    crate::config::AppPattern::Literal(s) => {
                        proc_name.eq_ignore_ascii_case(s) || full_path.eq_ignore_ascii_case(s)
                    }
                    crate::config::AppPattern::Regex(r) => {
                        r.is_match(&proc_name) || r.is_match(&full_path)
                    }
                };
                if matched {
                    new_active_apps.insert(proc_name.to_string());
                    any_running = true;
                }
            }
        }
        any_running
    }

    /// Check compositor windows via IPC (niri, Hyprland, River)
    async fn check_compositor_windows(&self) -> Result<HashSet<String>, Box<dyn std::error::Error + Send + Sync>> {
        static DESKTOP_PRINTED: AtomicBool = AtomicBool::new(false);

        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();

        // Log only once
        if !DESKTOP_PRINTED.load(Ordering::Relaxed) {
            log_to_cache(&format!("[Stasis] XDG_CURRENT_DESKTOP: {}", desktop));
            DESKTOP_PRINTED.store(true, Ordering::Relaxed);
        }

        match desktop.as_str() {
            "niri" => {
                let app_ids = self.try_niri_ipc().await?;
                let mut active = HashSet::new();
                for app in app_ids {
                    if self.should_inhibit_for_app(&app) {
                        active.insert(app);
                    }
                }
                Ok(active)
            }
            "hyprland" => {
                let windows = self.try_hyprland_ipc().await?;
                let mut active = HashSet::new();
                for window in windows {
                    if let Some(app_id) = window.get("app_id").and_then(|v| v.as_str()) {
                        if self.should_inhibit_for_app(app_id) {
                            active.insert(app_id.to_string());
                        }
                    }
                }
                Ok(active)
            }
            "river" => {
                let _ = self.try_river_ipc().await?;
                Ok(HashSet::new()) // fallback
            }
            _ => Err("Unsupported compositor".into())
        }
    }


    /// Try niri IPC - parse text output for App IDs
    async fn try_niri_ipc(&self) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let output = Command::new("niri")
            .args(&["msg", "windows"])
            .output()
            .await?;

        if !output.status.success() {
            return Err(format!("niri command failed: {}", 
                String::from_utf8_lossy(&output.stderr)).into());
        }

        let text = String::from_utf8(output.stdout)?;
        let mut app_ids = Vec::new();

        // Parse lines looking for "App ID: "
        for line in text.lines() {
            if let Some(app_id_line) = line.strip_prefix("  App ID: ") {
                // Remove quotes if present
                let app_id = app_id_line.trim_matches('"').to_string();
                //println!("[DEBUG] Found App ID: {}", app_id);
                app_ids.push(app_id);
            }
        }

        Ok(app_ids)
    }

    /// Try Hyprland IPC
    async fn try_hyprland_ipc(&self) -> Result<Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
        let output = Command::new("hyprctl")
            .args(&["clients", "-j"])
            .output()
            .await?;

        if !output.status.success() {
            return Err(format!("hyprctl command failed: {}", 
                String::from_utf8_lossy(&output.stderr)).into());
        }

        let json_str = String::from_utf8(output.stdout)?;
        let clients: Vec<Value> = serde_json::from_str(&json_str)?;
        
        // Convert Hyprland format to niri-like format
        let windows: Vec<Value> = clients.into_iter().map(|mut client| {
            // Hyprland uses "class" instead of "app_id"
            if let Some(class) = client.get("class").cloned() {
                client.as_object_mut().unwrap().insert("app_id".to_string(), class);
            }
            client
        }).collect();

        Ok(windows)
    }

    /// Try River IPC (limited support)
    async fn try_river_ipc(&self) -> Result<Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
        // River doesn't have comprehensive window enumeration like niri/Hyprland
        // We'll just check if riverctl exists and return empty to fall back to processes
        let output = Command::new("riverctl")
            .args(&["list-focused-tags"])
            .output()
            .await?;

        if !output.status.success() {
            return Err("riverctl command failed".into());
        }

        // River IPC is too limited for proper window enumeration
        Err("River IPC too limited for window enumeration".into())
    }


    /// Check if an app ID/name should inhibit idle
    fn should_inhibit_for_app(&self, app_id: &str) -> bool {
        for pattern in &self.cfg.inhibit_apps {
            let matched = match pattern {
                crate::config::AppPattern::Literal(s) => {
                    self.app_id_matches(s, app_id)
                }
                crate::config::AppPattern::Regex(r) => {
                    r.is_match(app_id)
                }
            };
            
            if matched {
                return true;
            }
        }
        false
    }

    /// Helper to match App IDs with various formats
    fn app_id_matches(&self, pattern: &str, app_id: &str) -> bool {
        // Direct match
        if pattern.eq_ignore_ascii_case(app_id) {
            return true;
        }

        // Handle .exe suffix for Wine apps
        if app_id.ends_with(".exe") {
            let app_name = app_id.strip_suffix(".exe").unwrap_or(app_id);
            if pattern.eq_ignore_ascii_case(app_name) {
                return true;
            }
        }

        // Handle reverse DNS style app IDs (com.app.name -> name)
        if let Some(last_part) = pattern.split('.').last() {
            if last_part.eq_ignore_ascii_case(app_id) {
                return true;
            }
        }

        // Handle common Steam/Wine game patterns
        pattern.to_lowercase() == app_id.to_lowercase()
    }
}

/// Spawn a periodic task to check apps and reset IdleTimer if needed
pub fn spawn_app_inhibit_task(
    idle_timer: Arc<Mutex<crate::idle_timer::IdleTimer>>,
    cfg: Arc<IdleConfig>,
) {
    let mut inhibitor = AppInhibitor::new(cfg);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            ticker.tick().await;
            if inhibitor.is_any_app_running().await {
                let mut timer = idle_timer.lock().await;
                timer.reset();
            }
        }
    });
}

