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

impl fmt::Display for AppPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppPattern::Literal(s) => write!(f, "{}", s),
            AppPattern::Regex(r) => write!(f, "(regex) {}", r.as_str()),
        }
    }
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

impl IdleConfig {
    pub fn pretty_print(&self) -> String {
        let mut output = String::new();

        output.push_str("=== Stasis Configuration ===\n\n");

        // General settings
        output.push_str("General Settings:\n");
        output.push_str(&format!(
            "  Resume command      : {}\n",
            self.resume_command.as_deref().unwrap_or("None")
        ));
        output.push_str(&format!(
            "  Pre-suspend command : {}\n",
            self.pre_suspend_command.as_deref().unwrap_or("None")
        ));
        output.push_str(&format!("  Monitor media       : {}\n", self.monitor_media));
        output.push_str(&format!("  Respect inhibitors  : {}\n", self.respect_idle_inhibitors));

        // Inhibited apps
        output.push_str("  Inhibited Apps      : ");
        if self.inhibit_apps.is_empty() {
            output.push_str("None\n");
        } else {
            let app_list: Vec<String> = self.inhibit_apps.iter().map(|p| p.to_string()).collect();
            output.push_str(&format!("{}\n", app_list.join(", ")));
        }

        // Group actions by prefix
        let mut grouped: std::collections::BTreeMap<&str, Vec<(&String, &IdleAction)>> =
            std::collections::BTreeMap::new();

        for (key, action) in &self.actions {
            let prefix = if key.starts_with("ac.") {
                "AC"
            } else if key.starts_with("battery.") {
                "Battery"
            } else {
                "Desktop"
            };
            grouped.entry(prefix).or_default().push((key, action));
        }

        output.push_str("\nIdle Actions:\n");

        for (group, actions) in grouped {
            output.push_str(&format!("  [{}]\n", group));

            let mut sorted = actions.clone();
            sorted.sort_by(|a, b| a.0.cmp(b.0));

            for (key, action) in sorted {
                output.push_str(&format!(
                    "    {:<22} | timeout: {:>5}s | kind: {:<12} | command: {}\n",
                    key,
                    action.timeout_seconds,
                    action.kind,
                    action.command
                ));
            }
        }

        output
    }
}

// --- Helpers ---

fn parse_app_pattern(s: &str) -> Result<AppPattern> {
    let regex_meta = ['.', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\', '^', '$'];
    if s.chars().any(|c| regex_meta.contains(&c)) {
        let re = Regex::new(s)?;
        Ok(AppPattern::Regex(re))
    } else {
        Ok(AppPattern::Literal(s.to_string()))
    }
}

// Normalize keys for Rune: convert _ to -
fn normalize_key(key: &str) -> String {
    key.replace('_', "-")
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
            let command: String = match config.get(&normalize_key(&command_path)) {
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
                config.get(&normalize_key(&format!("{}.{}.timeout", path, key))).unwrap_or(300);

            actions.insert(
                normalize_key(&key),
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

    fn get_string(config: &RuneConfig, key: &str) -> Option<String> {
        config.get(&normalize_key(key)).ok()
    }

    fn get_bool(config: &RuneConfig, key: &str, default: bool) -> bool {
        config.get(&normalize_key(key)).unwrap_or(default)
    }

    // --- General fields ---
    let resume_command = get_string(&config, "idle.resume_command");
    let pre_suspend_command = get_string(&config, "idle.pre_suspend_command");
    let monitor_media = get_bool(&config, "idle.monitor_media", true);
    let respect_idle_inhibitors = get_bool(&config, "idle.respect_idle_inhibitors", true);

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

    // --- Actions ---
    let actions = if laptop {
        let mut map = HashMap::new();

        for ac_key in &["on_ac", "on-ac"] {
            if let Ok(keys) = config.get_keys(&normalize_key(&format!("idle.{}", ac_key))) {
                for key in keys {
                    let command_path = format!("idle.{}.{}.command", ac_key, key);
                    if let Ok(command) = config.get::<String>(&normalize_key(&command_path)) {
                        let kind = match key.as_str() {
                            "lock_screen" | "lock-screen" => IdleActionKind::LockScreen,
                            "suspend" => IdleActionKind::Suspend,
                            "dpms" => IdleActionKind::Dpms,
                            "brightness" => IdleActionKind::Brightness,
                            _ => IdleActionKind::Custom,
                        };
                        let timeout_seconds: u64 = config
                            .get(&normalize_key(&format!("idle.{}.{}.timeout", ac_key, key)))
                            .unwrap_or(0);
                        map.insert(
                            format!("ac.{}", normalize_key(&key)),
                            IdleAction {
                                timeout_seconds,
                                command,
                                kind,
                            },
                        );
                    }
                }
            }
        }

        for bat_key in &["on_battery", "on-battery"] {
            if let Ok(keys) = config.get_keys(&normalize_key(&format!("idle.{}", bat_key))) {
                for key in keys {
                    let command_path = format!("idle.{}.{}.command", bat_key, key);
                    if let Ok(command) = config.get::<String>(&normalize_key(&command_path)) {
                        let kind = match key.as_str() {
                            "lock_screen" | "lock-screen" => IdleActionKind::LockScreen,
                            "suspend" => IdleActionKind::Suspend,
                            "dpms" => IdleActionKind::Dpms,
                            "brightness" => IdleActionKind::Brightness,
                            _ => IdleActionKind::Custom,
                        };
                        let timeout_seconds: u64 = config
                            .get(&normalize_key(&format!("idle.{}.{}.timeout", bat_key, key)))
                            .unwrap_or(0);
                        map.insert(
                            format!("battery.{}", normalize_key(&key)),
                            IdleAction {
                                timeout_seconds,
                                command,
                                kind,
                            },
                        );
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
    log_message(&format!("  pre_suspend_command = {:?}", pre_suspend_command));
    log_message(&format!(
        "  inhibit_apps = [{}]",
        inhibit_apps
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
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
    })
}
