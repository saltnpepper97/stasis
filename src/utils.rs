use std::fs;

/// Returns true if the system is likely a laptop/notebook/portable
pub fn is_laptop() -> bool {
    let chassis_path = "/sys/class/dmi/id/chassis_type";

    if let Ok(content) = fs::read_to_string(chassis_path) {
        match content.trim() {
            "8" | "9" | "10" => true, // 8=Portable, 9=Notebook, 10=Handheld
            _ => false,
        }
    } else {
        false // Could not read file, assume not a laptop
    }
}

pub fn format_duration(dur: std::time::Duration) -> String {
    let secs = dur.as_secs();

    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let minutes = secs / 60;
        let seconds = secs % 60;
        format!("{}m {}s", minutes, seconds)
    } else {
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        format!("{}h {}m", hours, minutes)
    }
}

