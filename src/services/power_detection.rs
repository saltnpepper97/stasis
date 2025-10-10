use std::{
    fs,
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use crate::core::legacy::LegacyIdleTimer;
use crate::log::log_message;

/// Detect initial power state on laptop (called once at startup)
pub fn detect_initial_power_state(is_laptop: bool) -> bool {
    if !is_laptop {
        log_message("Desktop detected, skipping power source check");
        return true;
    }

    // Simply check AC adapters once
    let on_ac = is_on_ac_power(is_laptop);
    log_message(&format!("Initial power detection: {}", if on_ac { "AC" } else { "Battery" }));
    on_ac
}

/// Check if system is currently on AC power
pub fn is_on_ac_power(is_laptop: bool) -> bool {
    if !is_laptop {
        return true;
    }

    // Scan /sys/class/power_supply
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply/") {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();

            if let Ok(supply_type) = fs::read_to_string(path.join("type")) {
                let supply_type = supply_type.trim();
                if supply_type == "Mains" {
                    if let Ok(status) = fs::read_to_string(path.join("online")) {
                        if status.trim() == "1" {
                            return true;
                        }
                    }
                }
            }

            // Optional: fallback on legacy AC names
            let legacy_ac_names = ["AC", "ADP", "ACAD", "AC0", "ADP0"];
            if legacy_ac_names.iter().any(|n| name.starts_with(n)) {
                if let Ok(status) = fs::read_to_string(path.join("online")) {
                    if status.trim() == "1" {
                        return true;
                    }
                }
            }
        }
    }

    // If no AC detected, assume battery
    false
}

pub async fn spawn_power_monitor(idle_timer: Arc<Mutex<LegacyIdleTimer>>) {
    let is_laptop = crate::utils::is_laptop();
    let mut last_on_ac = detect_initial_power_state(is_laptop);

    {
        let mut timer = idle_timer.lock().await;
        timer.on_ac = last_on_ac;
    }

    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    loop {
        ticker.tick().await;

        if !is_laptop {
            continue;
        }

        let on_ac = is_on_ac_power(is_laptop);
        if on_ac != last_on_ac {
            last_on_ac = on_ac;
            log_message(&format!("Power source changed: {}", if on_ac { "AC" } else { "Battery" }));
            idle_timer.lock().await.update_power_source(on_ac).await;
        }
    }
}
