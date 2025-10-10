
use eyre::Result;
use regex::Regex;
use rune_cfg::{RuneConfig, Value};
use std::collections::HashMap;
use crate::config::model::*;
use crate::log::log_message;
use crate::utils::is_laptop;

// --- helpers ---
fn parse_app_pattern(s: &str) -> Result<AppPattern> {
    let regex_meta = ['.', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\', '^', '$'];
    if s.chars().any(|c| regex_meta.contains(&c)) {
        let re = Regex::new(s)?;
        Ok(AppPattern::Regex(re))
    } else {
        Ok(AppPattern::Literal(s.to_string()))
    }
}

// Helper to try both - and _ variants of a key
fn try_get_string(config: &RuneConfig, base_path: &str) -> Option<String> {
    // Try hyphenated version first
    let hyphenated = base_path.replace('_', "-");
    if let Ok(val) = config.get::<String>(&hyphenated) {
        return Some(val);
    }
    
    // Try underscored version
    let underscored = base_path.replace('-', "_");
    if let Ok(val) = config.get::<String>(&underscored) {
        return Some(val);
    }
    
    None
}

fn try_get_bool(config: &RuneConfig, base_path: &str, default: bool) -> bool {
    // Try hyphenated version first
    let hyphenated = base_path.replace('_', "-");
    if let Ok(val) = config.get::<bool>(&hyphenated) {
        return val;
    }
    
    // Try underscored version
    let underscored = base_path.replace('-', "_");
    if let Ok(val) = config.get::<bool>(&underscored) {
        return val;
    }
    
    default
}

fn try_get_value(config: &RuneConfig, base_path: &str) -> Option<Value> {
    // Try hyphenated version first
    let hyphenated = base_path.replace('_', "-");
    if let Ok(val) = config.get_value(&hyphenated) {
        return Some(val);
    }
    
    // Try underscored version
    let underscored = base_path.replace('-', "_");
    if let Ok(val) = config.get_value(&underscored) {
        return Some(val);
    }
    
    None
}

fn try_get_keys(config: &RuneConfig, base_path: &str) -> Vec<String> {
    let mut keys = Vec::new();

    // Try underscored version first
    let underscored = base_path.replace('-', "_");
    if let Ok(k) = config.get_keys(&underscored) {
        keys.extend(k);
    }

    // Try hyphenated version next
    let hyphenated = base_path.replace('_', "-");
    if let Ok(k) = config.get_keys(&hyphenated) {
        for key in k {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }

    keys
}

// Normalize a key for consistent storage (use hyphens)
fn normalize_key(key: &str) -> String {
    key.replace('_', "-")
}

fn is_special_key(key: &str) -> bool {
    matches!(
        key,
        "resume_command" | "resume-command"
            | "pre_suspend_command" | "pre-suspend-command"
            | "monitor_media" | "monitor-media"
            | "respect_idle_inhibitors" | "respect-idle-inhibitors"
            | "inhibit_apps" | "inhibit-apps"
    )
}

fn collect_actions(config: &RuneConfig, path: &str, prefix: &str) -> HashMap<String, IdleAction> {
    let mut actions = HashMap::new();
    let keys = try_get_keys(config, path);

    for key in keys {
        if is_special_key(&key) {
            continue;
        }

        // Command must exist
        let command = match try_get_string(config, &format!("{}.{}.command", path, key)) {
            Some(cmd) => cmd,
            None => continue,
        };

        // Timeout must exist and parse, otherwise skip
        let timeout_seconds = match try_get_value(config, &format!("{}.{}.timeout", path, key)) {
            Some(Value::Number(n)) => n as u64,
            Some(Value::String(s)) => match s.parse::<u64>() {
                Ok(n) => n,
                _ => continue,
            },
            _ => continue,
        };

        // Determine kind
        let kind = match key.as_str() {
            "lock_screen" | "lock-screen" => IdleActionKind::LockScreen,
            "suspend" => IdleActionKind::Suspend,
            "dpms" => IdleActionKind::Dpms,
            "brightness" => IdleActionKind::Brightness,
            _ => IdleActionKind::Custom,
        };

        actions.insert(
            format!("{}.{}", prefix, normalize_key(&key)),
            IdleAction {
                timeout_seconds,
                command,
                kind,
            },
        );
    }

    actions
}

// --- main loader ---
pub fn load_config(path: &str) -> Result<IdleConfig> {
    let config = RuneConfig::from_file(path)?;

    let resume_command = try_get_string(&config, "idle.resume_command");
    let pre_suspend_command = try_get_string(&config, "idle.pre_suspend_command");
    let monitor_media = try_get_bool(&config, "idle.monitor_media", true);
    let respect_idle_inhibitors = try_get_bool(&config, "idle.respect_idle_inhibitors", true);

    let debounce_seconds = match try_get_value(&config, "idle.debounce_seconds") {
        Some(Value::Number(n)) => n as u8,
        Some(Value::String(s)) => s.parse::<u8>().unwrap_or(3),
        _ => 3,
    };

    let inhibit_apps: Vec<AppPattern> = match try_get_value(&config, "idle.inhibit_apps") {
        Some(Value::Array(arr)) => arr.iter().filter_map(|v| match v {
            Value::String(s) => parse_app_pattern(s).ok(),
            Value::Regex(s) => Regex::new(s).ok().map(AppPattern::Regex),
            _ => None,
        }).collect(),
        _ => Vec::new(),
    };

    let laptop = is_laptop();
    let actions = if laptop {
        let mut map = HashMap::new();
        map.extend(collect_actions(&config, "idle.on_ac", "ac"));
        map.extend(collect_actions(&config, "idle.on_battery", "battery"));
        map
    } else {
        collect_actions(&config, "idle", "desktop")
    };

    log_message("Parsed Config:");
    log_message(&format!("  resume_command = {:?}", resume_command));
    log_message(&format!("  pre_suspend_command = {:?}", pre_suspend_command));
    log_message(&format!("  monitor_media = {:?}", monitor_media));
    log_message(&format!("  respect_idle_inhibitors = {:?}", respect_idle_inhibitors));
    log_message(&format!("  debounce_seconds = {:?}", debounce_seconds));
    log_message(&format!(
        "  inhibit_apps = [{}]",
        inhibit_apps.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    ));
    log_message("  actions:");
    for (key, action) in &actions {
        log_message(&format!(
            "    {}: timeout={}s, kind={:?}, command=\"{}\"",
            key, action.timeout_seconds, action.kind, action.command
        ));
    }

    Ok(IdleConfig {
        actions,
        resume_command,
        pre_suspend_command,
        monitor_media,
        respect_idle_inhibitors,
        inhibit_apps,
        debounce_seconds,
    })
}
