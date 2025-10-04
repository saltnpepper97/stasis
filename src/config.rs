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
    /// Pretty-print config, optionally including runtime info
    pub fn pretty_print(
        &self,
        idle_time: Option<std::time::Duration>,
        uptime: Option<std::time::Duration>,
        is_inhibited: Option<bool>,
    ) -> String {
        let mut out = String::new();

        // General settings
        out.push_str("General:\n");
        out.push_str(&format!(
            "  ResumeCommand      = {}\n",
            self.resume_command.as_deref().unwrap_or("-")
        ));
        out.push_str(&format!(
            "  PreSuspendCommand  = {}\n",
            self.pre_suspend_command.as_deref().unwrap_or("-")
        ));
        out.push_str(&format!(
            "  MonitorMedia       = {}\n",
            if self.monitor_media { "true" } else { "false" }
        ));
        out.push_str(&format!(
            "  RespectInhibitors  = {}\n",
            if self.respect_idle_inhibitors { "true" } else { "false" }
        ));

        let apps = if self.inhibit_apps.is_empty() {
            "-".to_string()
        } else {
            self.inhibit_apps
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        out.push_str(&format!("  InhibitApps        = {}\n", apps));

        // Optional runtime info
        if let Some(idle) = idle_time {
            out.push_str(&format!("  IdleTime           = {}\n", crate::utils::format_duration(idle)));
        }
        if let Some(up) = uptime {
            out.push_str(&format!("  Uptime             = {}\n", crate::utils::format_duration(up)));
        }
        if let Some(inhibited) = is_inhibited {
            out.push_str(&format!("  IdleInhibited      = {}\n", inhibited));
        }

        // Actions
        out.push_str("\nActions:\n");

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

        for (group, actions) in grouped {
            out.push_str(&format!("  [{}]\n", group));

            let mut sorted = actions.clone();
            sorted.sort_by(|a, b| a.0.cmp(b.0));

            for (key, action) in sorted {
                out.push_str(&format!(
                    "    {:<20} Timeout={} Kind={} Command=\"{}\"\n",
                    key,
                    action.timeout_seconds,
                    action.kind,
                    action.command
                ));
            }
        }

        out
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

pub fn load_config(path: &str) -> Result<IdleConfig> {
    let config = RuneConfig::from_file(path)?;

    // --- General Settings ---
    let resume_command = try_get_string(&config, "idle.resume_command");
    let pre_suspend_command = try_get_string(&config, "idle.pre_suspend_command");
    let monitor_media = try_get_bool(&config, "idle.monitor_media", true);
    let respect_idle_inhibitors = try_get_bool(&config, "idle.respect_idle_inhibitors", true);

    // --- Inhibited Apps ---
    let inhibit_apps: Vec<AppPattern> = match try_get_value(&config, "idle.inhibit_apps") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => parse_app_pattern(s).ok(),
                Value::Regex(s) => Regex::new(s).ok().map(AppPattern::Regex),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };

    // --- Actions ---
    let laptop = is_laptop();
    let actions = if laptop {
        // Laptop: only AC/Battery
        let mut map = HashMap::new();
        map.extend(collect_actions(&config, "idle.on_ac", "ac"));
        map.extend(collect_actions(&config, "idle.on_battery", "battery"));
        map
    } else {
        // Desktop: load only top-level idle actions that are not AC/Battery blocks
        collect_actions(&config, "idle", "desktop")
    };

    // --- Logging ---
    log_message("Parsed Config:");
    log_message(&format!("  resume_command = {:?}", resume_command));
    log_message(&format!("  pre_suspend_command = {:?}", pre_suspend_command));
    log_message(&format!("  monitor_media = {:?}", monitor_media));
    log_message(&format!("  respect_idle_inhibitors = {:?}", respect_idle_inhibitors));
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

#[test]
fn test_rune_config_parsing() {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("examples/stasis.rune");

    let config: IdleConfig = match load_config(path.to_str().unwrap()) {
        Ok(cfg) => cfg,
        Err(e) => {
            panic!("Failed to load config: {:?}", e);
        }
    };

    println!("{}", config.pretty_print());
}
