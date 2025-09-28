use std::{collections::HashMap, fmt};
use eyre::Result;
use regex::Regex;
use rune_cfg::{RuneConfig, Value};
use crate::{log::log_message, utils::is_laptop};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IdleActionKind {
    LockScreen,
    Suspend,
    Dpms,
    Brightness,
    Custom,
}

impl fmt::Display for IdleActionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdleActionKind::LockScreen => write!(f, "lock_screen"),
            IdleActionKind::Suspend => write!(f, "suspend"),
            IdleActionKind::Dpms => write!(f, "dpms"),
            IdleActionKind::Brightness => write!(f, "brightness"),
            IdleActionKind::Custom => write!(f, "custom"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IdleAction {
    pub timeout_seconds: u64,
    pub command: String,
    pub kind: IdleActionKind,
}

#[derive(Debug, Clone)]
pub enum AppPattern {
    Literal(String),
    Regex(Regex),
}

#[derive(Debug, Clone)]
pub struct IdleConfig {
    pub actions: HashMap<String, IdleAction>,
    pub resume_command: Option<String>,
    pub pre_suspend_command: Option<String>,
    pub monitor_media: bool,
    pub respect_idle_inhibitors: bool,
    pub inhibit_apps: Vec<AppPattern>,
}

fn parse_app_pattern(s: &str) -> Result<AppPattern> {
    let regex_meta = ['.', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\', '^', '$'];
    if s.chars().any(|c| regex_meta.contains(&c)) {
        let re = Regex::new(s)?;
        Ok(AppPattern::Regex(re))
    } else {
        Ok(AppPattern::Literal(s.to_string()))
    }
}

fn collect_actions(config: &RuneConfig, path: &str) -> HashMap<String, IdleAction> {
    let mut actions = HashMap::new();
    if let Ok(keys) = config.get_keys(path) {
        for key in keys {
            if matches!(
                key.as_str(),
                "resume_command" | "pre_suspend_command" | "monitor_media" | "respect_idle_inhibitors" | "inhibit_apps"
            ) {
                continue;
            }

            let command_path = format!("{}.{}.command", path, key);
            let command: String = match config.get(&command_path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let kind = match key.as_str() {
                "lock_screen" | "lock-screen" => IdleActionKind::LockScreen,
                "suspend" => IdleActionKind::Suspend,
                "dpms" => IdleActionKind::Dpms,
                "brightness" => IdleActionKind::Brightness,
                _ => IdleActionKind::Custom,
            };

            let timeout_seconds: u64 =
                config.get(&format!("{}.{}.timeout", path, key)).unwrap_or(300);

            actions.insert(
                key.clone(),
                IdleAction {
                    timeout_seconds,
                    command,
                    kind,
                },
            );
        }
    }
    actions
}

pub fn load_config(path: &str) -> Result<IdleConfig> {
    let config = RuneConfig::from_file(path)?;

    fn get_array(config: &RuneConfig, path: &str) -> Vec<Value> {
        match config.get_value(path) {
            Ok(Value::Array(arr)) => arr,
            _ => Vec::new(),
        }
    }

    let resume_command: Option<String> = config.get("idle.resume_command").ok();
    let pre_suspend_command: Option<String> = config.get("idle.pre_suspend_command").ok();
    let monitor_media: bool = config.get("idle.monitor_media").unwrap_or(true);
    let respect_idle_inhibitors: bool = config.get("idle.respect_idle_inhibitors").unwrap_or(true);
    let inhibit_raw = get_array(&config, "idle.inhibit_apps");

    let inhibit_apps: Vec<AppPattern> = inhibit_raw
        .iter()
        .filter_map(|v| match v {
            Value::String(s) => parse_app_pattern(s).ok(),
            Value::Regex(s) => Regex::new(s).ok().map(AppPattern::Regex),
            _ => None,
        })
        .collect();

    // Determine if laptop or desktop
    let laptop = is_laptop();

    // Inside load_config
    let actions = if laptop {
        let mut map = HashMap::new();

        for ac_key in &["on_ac", "on-ac"] {
            if let Ok(keys) = config.get_keys(&format!("idle.{}", ac_key)) {
                for key in keys {
                    let command_path = format!("idle.{}.{}.command", ac_key, key);
                    if let Ok(command) = config.get::<String>(&command_path) {
                        let kind = match key.as_str() {
                            "lock_screen" | "lock-screen" => IdleActionKind::LockScreen,
                            "suspend" => IdleActionKind::Suspend,
                            "dpms" => IdleActionKind::Dpms,
                            "brightness" => IdleActionKind::Brightness,
                            _ => IdleActionKind::Custom,
                        };
                        let timeout_seconds: u64 =
                            config.get(&format!("idle.{}.{}.timeout", ac_key, key)).unwrap_or(0);
                        map.insert(format!("ac.{}", key), IdleAction { timeout_seconds, command, kind });
                    }
                }
            }
        }

        for bat_key in &["on_battery", "on-battery"] {
            if let Ok(keys) = config.get_keys(&format!("idle.{}", bat_key)) {
                for key in keys {
                    let command_path = format!("idle.{}.{}.command", bat_key, key);
                    if let Ok(command) = config.get::<String>(&command_path) {
                        let kind = match key.as_str() {
                            "lock_screen" | "lock-screen" => IdleActionKind::LockScreen,
                            "suspend" => IdleActionKind::Suspend,
                            "dpms" => IdleActionKind::Dpms,
                            "brightness" => IdleActionKind::Brightness,
                            _ => IdleActionKind::Custom,
                        };
                        let timeout_seconds: u64 =
                            config.get(&format!("idle.{}.{}.timeout", bat_key, key)).unwrap_or(0);
                        map.insert(format!("battery.{}", key), IdleAction { timeout_seconds, command, kind });
                    }
                }
            }
        }

        map
    } else {
        collect_actions(&config, "idle")
    };

    log_message("Parsed Config:");
    log_message(&format!("  resume_command = {:?}", resume_command));
    log_message(&format!("  monitor_media = {:?}", monitor_media));
    log_message(&format!("  respect_idle_inhibitors = {:?}", respect_idle_inhibitors));
    log_message(&format!("  inhibit_apps = {:?}", inhibit_apps));
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
    })
}
