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
