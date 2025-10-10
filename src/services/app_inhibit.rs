use std::{collections::HashSet, sync::Arc};
use tokio::sync::Mutex;
use tokio::process::Command;
use serde_json::Value;
use sysinfo::{System, RefreshKind, ProcessRefreshKind, ProcessesToUpdate};

use crate::config::IdleConfig;
use crate::log::log_message;
use crate::core::legacy::timer::LegacyIdleTimer;

/// Tracks currently running apps to inhibit idle
pub struct AppInhibitor {
    cfg: Arc<IdleConfig>,
    system: System,
    active_apps: HashSet<String>,
    desktop: String,
    checks_since_reset: u32,
    #[allow(dead_code)]
    idle_timer: Arc<Mutex<LegacyIdleTimer>>,
}

impl AppInhibitor {
    pub fn new(cfg: Arc<IdleConfig>, idle_timer: Arc<Mutex<LegacyIdleTimer>>) -> Self {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();

        // Minimal refresh - only process list, no memory/cpu stats
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing())
        );

        log_message(&format!("XDG_CURRENT_DESKTOP detected: {}", desktop));

        Self {
            cfg,
            system,
            active_apps: HashSet::new(),
            desktop,
            checks_since_reset: 0,
            idle_timer,
        }
    }

    /// Returns true if any app in inhibit_apps is currently running
    pub async fn is_any_app_running(&mut self) -> bool {
        let mut new_active_apps = HashSet::new();

        let running = match self.check_compositor_windows().await {
            Ok(result_apps) => {
                new_active_apps = result_apps;
                !new_active_apps.is_empty()
            },
            Err(_) => self.check_processes_with_tracking(&mut new_active_apps),
        };

        for app in &new_active_apps {
            if !self.active_apps.contains(app) {
                log_message(&format!("App inhibit active: {}", app));
            }
        }

        self.active_apps = new_active_apps;
        running
    }

    /// Process-based fallback - only refresh what we need
    fn check_processes_with_tracking(&mut self, new_active_apps: &mut HashSet<String>) -> bool {
        const RESET_THRESHOLD: u32 = 150; // Approx 10 mins (150 checks * 4s/check)

        self.checks_since_reset += 1;

        if self.checks_since_reset >= RESET_THRESHOLD {
            log_message("Periodic process tracker reset to reclaim memory.");
            self.system = System::new_with_specifics(
                RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing())
            );
            self.checks_since_reset = 0;
        }

        // Only refresh process list, nothing else
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All, 
            false, // Don't update all data
            ProcessRefreshKind::nothing() // Minimal refresh
        );

        let mut any_running = false;

        for process in self.system.processes().values() {
            let proc_name = process.name().to_string_lossy();
            let exe_path = process.exe()
                .map(|p| p.to_string_lossy())
                .unwrap_or_default();

            for pattern in &self.cfg.inhibit_apps {
                let matched = match pattern {
                    crate::config::AppPattern::Literal(s) => {
                        proc_name.eq_ignore_ascii_case(s) || exe_path.eq_ignore_ascii_case(s)
                    }
                    crate::config::AppPattern::Regex(r) => {
                        r.is_match(&proc_name) || r.is_match(&exe_path)
                    }
                };
                if matched {
                    new_active_apps.insert(proc_name.to_string());
                    any_running = true;
                    break; // No need to check other patterns for this process
                }
            }
        }

        any_running
    }

    /// Check compositor windows via IPC
    async fn check_compositor_windows(&self) -> Result<HashSet<String>, Box<dyn std::error::Error + Send + Sync>> {
        match self.desktop.as_str() {
            "niri" => {
                let app_ids = self.try_niri_ipc().await?;
                Ok(app_ids.into_iter()
                    .filter(|app| self.should_inhibit_for_app(app))
                    .collect())
            }
            "hyprland" => {
                let windows = self.try_hyprland_ipc().await?;
                Ok(windows.into_iter()
                    .filter_map(|win| win.get("app_id").and_then(|v| v.as_str()).map(|s| s.to_string()))
                    .filter(|app| self.should_inhibit_for_app(app))
                    .collect())
            }
            _ => Err("No IPC available, fallback to process scan".into())
        }
    }

    async fn try_niri_ipc(&self) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let output = Command::new("niri").args(&["msg", "windows"]).output().await?;
        if !output.status.success() {
            return Err(format!("niri command failed: {}", String::from_utf8_lossy(&output.stderr)).into());
        }
        let text = String::from_utf8(output.stdout)?;
        Ok(text.lines()
            .filter_map(|line| line.strip_prefix("  App ID: "))
            .map(|s| s.trim_matches('"').to_string())
            .collect())
    }

    async fn try_hyprland_ipc(&self) -> Result<Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
        let output = Command::new("hyprctl").args(&["clients", "-j"]).output().await?;
        if !output.status.success() {
            return Err(format!("hyprctl command failed: {}", String::from_utf8_lossy(&output.stderr)).into());
        }

        let clients: Vec<Value> = serde_json::from_slice(&output.stdout)?;
        let windows = clients.into_iter().map(|mut client| {
            if let Some(class) = client.get("class").cloned() {
                client.as_object_mut().unwrap().insert("app_id".to_string(), class);
            }
            client
        }).collect();

        Ok(windows)
    }

    fn should_inhibit_for_app(&self, app_id: &str) -> bool {
        for pattern in &self.cfg.inhibit_apps {
            let matched = match pattern {
                crate::config::AppPattern::Literal(s) => self.app_id_matches(s, app_id),
                crate::config::AppPattern::Regex(r) => r.is_match(app_id),
            };
            if matched { return true; }
        }
        false
    }

    fn app_id_matches(&self, pattern: &str, app_id: &str) -> bool {
        if pattern.eq_ignore_ascii_case(app_id) { return true; }
        if app_id.ends_with(".exe") {
            let name = app_id.strip_suffix(".exe").unwrap_or(app_id);
            if pattern.eq_ignore_ascii_case(name) { return true; }
        }
        if let Some(last) = pattern.split('.').last() {
            if last.eq_ignore_ascii_case(app_id) { return true; }
        }
        false
    }

    /// Gracefully stop the inhibitor
    pub async fn shutdown(&mut self) {
        log_message("Shutting down AppInhibitor...");
        self.active_apps.clear();
    }
}

pub fn spawn_app_inhibit_task(
    idle_timer: Arc<Mutex<LegacyIdleTimer>>,
    cfg: Arc<IdleConfig>
) -> Arc<Mutex<AppInhibitor>> {
    let inhibitor = Arc::new(Mutex::new(AppInhibitor::new(cfg, Arc::clone(&idle_timer))));
 
    let inhibitor_clone = Arc::clone(&inhibitor);
    tokio::spawn(async move {
        loop {
            {
                let mut guard = inhibitor_clone.lock().await;
                let was_running = !guard.active_apps.is_empty();
                let any_running = guard.is_any_app_running().await;

                let mut timer = idle_timer.lock().await;
                if any_running && !was_running {
                    timer.pause(false);
                } else if !any_running && was_running {
                    timer.resume(false);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        }
    });


    inhibitor
}

