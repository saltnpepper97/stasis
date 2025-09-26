use eyre::Result;
use rune_cfg::{RuneConfig, Value};
use regex::Regex;
use std::collections::HashMap;
use crate::utils::log_to_cache;

#[derive(Debug, Clone)]
pub struct IdleAction {
    pub timeout_seconds: u64,
    pub command: String,
}

/// New enum to represent literal or regex app patterns
#[derive(Debug, Clone)]
pub enum AppPattern {
    Literal(String),
    Regex(Regex),
}

#[derive(Debug, Clone)]
pub struct IdleConfig {
    pub actions: HashMap<String, IdleAction>,
    pub resume_command: Option<String>,
    pub monitor_media: bool,
    pub respect_idle_inhibitors: bool,
    pub inhibit_apps: Vec<AppPattern>,
}

/// Helper to convert a string to AppPattern
fn parse_app_pattern(s: &str) -> Result<AppPattern> {
    // If the string looks like a regex (starts with r" or contains regex-like escape), try to compile it
    if s.starts_with("r\"") && s.ends_with('"') {
        let inner = &s[2..s.len() - 1];
        let re = Regex::new(inner)?;
        Ok(AppPattern::Regex(re))
    } else if s.contains(".*") || s.contains("\\.") {
        // heuristic: treat strings with regex-like patterns as regex
        let re = Regex::new(s)?;
        Ok(AppPattern::Regex(re))
    } else {
        Ok(AppPattern::Literal(s.to_string()))
    }
}


#[allow(dead_code)]
/// Recursive helper to print nested blocks for debugging
fn print_block(config: &RuneConfig, path: &str, indent: usize) -> Result<()> {
    for key in config.get_keys(path)? {
        let full_path = if path.is_empty() { key.clone() } else { format!("{}.{}", path, key) };
        let value = config.get_value(&full_path)?;
        match value {
            Value::Object(_) => {
                println!("{}{}:", " ".repeat(indent), key);
                print_block(config, &full_path, indent + 2)?;
            }
            Value::Array(ref arr) => {
                println!("{}{} = {:?}", " ".repeat(indent), key, arr);
            }
            _ => {
                println!("{}{} = {:?}", " ".repeat(indent), key, value);
            }
        }
    }
    Ok(())
}

pub fn load_config(path: &str) -> Result<IdleConfig> {
    // Load the .rune config
    let config = RuneConfig::from_file(path)?;

    // ---- Globals ----
    let resume_command: Option<String> = config.get("idle.resume_command").ok();
    let monitor_media: bool = config.get("idle.monitor_media").unwrap_or(true);
    let respect_idle_inhibitors: bool = config.get("idle.respect_idle_inhibitors").unwrap_or(true);
    let inhibit_raw: Vec<String> = config.get("idle.inhibit_apps").unwrap_or_default();

    // Convert strings to AppPattern (handle regex)
    let inhibit_apps: Vec<AppPattern> = inhibit_raw
        .into_iter()
        .filter_map(|s| parse_app_pattern(&s).ok())
        .collect();

    // ---- Collect idle actions ----
    let mut actions = HashMap::new();
    if let Ok(action_keys) = config.get_keys("idle") {
        for key in action_keys {
            if matches!(
                key.as_str(),
                "resume_command" | "monitor_media" | "respect_idle_inhibitors" | "inhibit_apps"
            ) {
                continue;
            }

            let timeout_path = format!("idle.{}.timeout", key);
            let command_path = format!("idle.{}.command", key);

            let timeout_seconds: u64 = match config.get(&timeout_path) {
                Ok(n) => n,
                Err(_) => continue,
            };

            let command: String = match config.get(&command_path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            actions.insert(
                key.clone(),
                IdleAction {
                    timeout_seconds,
                    command,
                },
            );
        }
    }

    // ---- DEBUG PRINT ----
    log_to_cache("[Stasis]: Parsed Config:");
    if let Some(doc) = config.document() {
        log_to_cache(&format!("Globals: {:?}", doc.globals));
        log_to_cache(&format!("Metadata: {:?}", doc.metadata));
    }
    log_to_cache(&format!("  resume_command = {:?}", resume_command));
    log_to_cache(&format!("  monitor_media = {:?}", monitor_media));
    log_to_cache(&format!("  respect_idle_inhibitors = {:?}", respect_idle_inhibitors));
    log_to_cache(&format!("  inhibit_apps = {:?}", inhibit_apps));
    log_to_cache(&format!("  actions = {:#?}", actions));

    Ok(IdleConfig {
        actions,
        resume_command,
        monitor_media,
        respect_idle_inhibitors,
        inhibit_apps,
    })
}
