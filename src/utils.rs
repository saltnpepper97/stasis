use std::fs::{OpenOptions, create_dir_all, metadata, remove_file};
use std::io::Write;
use std::path::PathBuf;
use chrono::Local;
use once_cell::sync::Lazy;
use std::sync::{Mutex, Once};

/// Maximum log file size in bytes before rotation (50 MB)
const MAX_LOG_SIZE: u64 = 50 * 1024 * 1024;

/// Global runtime config
pub struct Config {
    pub verbose: bool,
}

pub static GLOBAL_CONFIG: Lazy<Mutex<Config>> = Lazy::new(|| {
    Mutex::new(Config {
        verbose: false, // default
    })
});

/// Ensures session separator is only added once per program run
static SESSION_SEPARATOR: Once = Once::new();

pub fn set_verbose(enabled: bool) {
    let mut config = GLOBAL_CONFIG.lock().unwrap();
    config.verbose = enabled;
}

/// Get log file path
fn log_path() -> PathBuf {
    let mut path = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    path.push("stasis");
    if !path.exists() {
        let _ = create_dir_all(&path);
    }
    path.push("stasis.log");
    path
}

/// Rotate the log if too big
fn rotate_log_if_needed(path: &PathBuf) {
    if let Ok(meta) = metadata(path) {
        if meta.len() >= MAX_LOG_SIZE {
            // Simple rotation: delete old log
            let _ = remove_file(path);
        }
    }
}

/// Ensure newline is added only once per session, and only if file has content
fn ensure_session_newline_once(path: &PathBuf) {
    SESSION_SEPARATOR.call_once(|| {
        if let Ok(meta) = metadata(path) {
            if meta.len() > 0 {
                // File exists and has content → append a blank line to separate sessions
                if let Ok(mut file) = OpenOptions::new().append(true).open(path) {
                    let _ = writeln!(file);
                }
            }
        }
    });
}

pub fn log_to_cache(message: &str) {
    let path = log_path();    
    // Rotate old log if too big
    rotate_log_if_needed(&path);
    // Add a separating blank line if file is not empty (only once per program run)
    ensure_session_newline_once(&path);
    
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    let _ = writeln!(file, "[{}] {}", timestamp, message);
}

pub fn log_message(message: &str) {
    let msg = format!("[Stasis] {}", message);
    log_to_cache(&msg);
    if GLOBAL_CONFIG.lock().unwrap().verbose {
        println!("{}", &msg);
    }
}

pub fn log_error_message(message: &str) {
    let error_msg = format!("[ERROR] {}", message);
    log_to_cache(&error_msg);
    if GLOBAL_CONFIG.lock().unwrap().verbose {
        eprintln!("{}", &error_msg);
    }
}
