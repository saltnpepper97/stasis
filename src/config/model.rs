use regex::Regex;
use std::collections::HashMap;
use std::fmt;

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
    pub debounce_seconds: u8,
    // pub default_sequence: HashMap<String>,
    // pub ac_squence: HashMap<String>,
    // pub battery_sequence: HashMap<String>,
    // pub lock_command: String,
}

