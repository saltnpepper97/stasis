use std::fs::{OpenOptions, create_dir_all};
use std::path::PathBuf;
use std::io::Write;
use chrono::Local;

pub fn log_to_cache(message: &str) {
    let mut path = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    path.push("stasis");
    if !path.exists() {
        let _ = create_dir_all(&path);
    }
    path.push("stasis.log");

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();

    // Add timestamp
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    let _ = writeln!(file, "[{}] {}", timestamp, message);
}
