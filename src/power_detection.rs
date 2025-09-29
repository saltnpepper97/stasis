use std::{fs, time::Duration};
use crate::log::log_message;

/// Detect power state with retries for boot scenarios
pub fn detect_power_state_with_retry(is_laptop: bool) -> bool {
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
