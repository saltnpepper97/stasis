// Author: Dustin Pilgrim
// License: GPL-3.0-only

use regex::Regex;
use std::fmt;

pub const DEFAULT_NOTIFICATION_ICON: &str = "stasis";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileMode {
    /// Start from a fully disabled config; only fields set in the profile apply.
    Fresh,
    /// Start from `default`, then override fields set in the profile (globals + blocks).
    Overlay,
}

impl Default for ProfileMode {
    fn default() -> Self {
        // If mode is omitted, overlay is least surprising.
        ProfileMode::Overlay
    }
}

/// Which plan source should be active right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSource {
    Desktop,
    Ac,
    Battery,
}

/// Which automatic idle actions media playback should inhibit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaInhibitScope {
    /// Preserve the historical behavior: media pauses the entire idle plan.
    #[default]
    All,
    /// Allow earlier idle actions, but prevent the automatic suspend step.
    Suspend,
}

impl fmt::Display for MediaInhibitScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::Suspend => write!(f, "suspend"),
        }
    }
}

/// What kind of step this is in the ordered execution plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanStepKind {
    Startup,
    Brightness,
    LockScreen,
    Dpms,
    Suspend,

    /// Future-proofing: arbitrary custom blocks.
    Custom(String),
}

/// One step in the ordered plan.
///
/// Canonical representation the manager consumes.
/// Loader should preserve file order when building plans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub kind: PlanStepKind,

    /// When this step triggers relative to the previous step firing (per-step timers).
    /// `0` means instant one-shot (fires immediately once when plan starts).
    pub timeout_seconds: u64,

    /// Primary command to run when the step fires (if any).
    pub command: Option<String>,

    /// Optional resume command to run on user activity after this step has fired.
    pub resume_command: Option<String>,

    /// Optional notification emitted before firing this step.
    pub notification: Option<String>,

    /// Optional icon name/path for this step's notification.
    pub notification_icon: Option<String>,

    /// Notify N seconds before this step fires (only if notification is Some).
    pub notify_seconds_before: Option<u64>,
}

impl PlanStep {
    /// Enabled if it has a command.
    ///
    /// NOTE: `timeout_seconds == 0` is *not* disabled; it's an instant one-shot.
    pub fn enabled(&self) -> bool {
        self.command.is_some()
    }

    /// Instant one-shot step: fires immediately once when the plan starts.
    pub fn is_instant(&self) -> bool {
        self.enabled() && self.timeout_seconds == 0
    }
}

/// Pattern used for inhibit lists (app inhibit + media blacklist).
///
/// - Literals expected to already be normalized lowercase by the config loader.
/// - Regex values are compiled by rune-cfg already (Value::Regex).
#[derive(Debug, Clone)]
pub enum Pattern {
    Literal(String),
    Regex(Regex),
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pattern::Literal(s) => write!(f, "{s}"),
            Pattern::Regex(r) => write!(f, "/{}/", r.as_str()),
        }
    }
}

impl Pattern {
    /// Match against a lowercase haystack string.
    pub fn matches_lc(&self, hay_lc: &str) -> bool {
        match self {
            Pattern::Literal(s) => !s.is_empty() && hay_lc.contains(s),
            Pattern::Regex(r) => r.is_match(hay_lc),
        }
    }

    pub fn render(&self) -> String {
        match self {
            Pattern::Literal(s) => s.clone(),
            Pattern::Regex(r) => format!("/{}/", r.as_str()),
        }
    }
}

// NOTE: We cannot derive Eq/PartialEq for Config anymore because Regex
// doesn't implement those traits. Debug+Clone is enough for daemon flow.
#[derive(Debug, Clone)]
pub struct Config {
    // ---- globals ----
    pub enable_loginctl_integration: bool,
    pub enable_dbus_inhibit: bool,
    pub pre_suspend_command: Option<String>,
    pub prepare_sleep_command: Option<String>,

    /// When true, Stasis applies conservative hardware power-saving (GPU runtime
    /// PM, amdgpu DPM) after the DPMS step fires and the idle timeout elapses.
    /// Snapshot-based: everything changed is restored exactly on any resume path.
    pub low_power_when_idle: bool,

    /// Seconds to wait after DPMS fires before entering low-power mode.
    pub low_power_when_idle_timeout: u64,

    /// Shell command to run immediately when the lid is closed (if any).
    pub lid_close_action: Option<String>,
    /// Shell command to run immediately when the lid is opened (if any).
    pub lid_open_action: Option<String>,

    pub monitor_media: bool,
    pub ignore_remote_media: bool,
    pub media_inhibit_scope: MediaInhibitScope,

    /// Media sources/apps to ignore for media inhibit (case-insensitive; loader normalizes).
    pub media_blacklist: Vec<Pattern>,

    /// Media sources/apps that should inhibit only automatic suspend.
    pub suspend_inhibit_media: Vec<Pattern>,

    /// Debounce window in seconds.
    pub debounce_seconds: u64,

    pub notify_on_unpause: bool,
    pub notify_before_action: bool,
    pub notification_icon: Option<String>,

    /// Process/class patterns or names that should inhibit idle behavior.
    pub inhibit_apps: Vec<Pattern>,

    /// Process/class patterns or names that should inhibit only automatic suspend.
    pub suspend_inhibit_apps: Vec<Pattern>,

    // ---- legacy named blocks (still useful for config authoring) ----
    pub startup: ActionBlock,
    pub brightness: ActionBlock,
    pub lock_screen: LockBlock,
    pub dpms: ActionBlock,
    pub suspend: ActionBlock,

    // ---- canonical plan sources ----
    pub plan_desktop: Vec<PlanStep>,
    pub plan_ac: Vec<PlanStep>,
    pub plan_battery: Vec<PlanStep>,

    // ---- active plan consumed by manager ----
    pub plan: Vec<PlanStep>,
}

impl Default for Config {
    fn default() -> Self {
        Self::disabled()
    }
}

impl Config {
    pub fn disabled() -> Self {
        Self {
            enable_loginctl_integration: false,
            enable_dbus_inhibit: true,
            pre_suspend_command: None,
            prepare_sleep_command: None,

            low_power_when_idle: false,
            low_power_when_idle_timeout: 0,

            lid_close_action: None,
            lid_open_action: None,

            monitor_media: false,
            ignore_remote_media: false,
            media_inhibit_scope: MediaInhibitScope::All,
            media_blacklist: Vec::new(),
            suspend_inhibit_media: Vec::new(),

            debounce_seconds: 0,

            notify_on_unpause: false,
            notify_before_action: false,
            notification_icon: Some(DEFAULT_NOTIFICATION_ICON.to_string()),

            inhibit_apps: Vec::new(),
            suspend_inhibit_apps: Vec::new(),

            startup: ActionBlock::disabled(),
            brightness: ActionBlock::disabled(),
            lock_screen: LockBlock::disabled(),
            dpms: ActionBlock::disabled(),
            suspend: ActionBlock::disabled(),

            plan_desktop: Vec::new(),
            plan_ac: Vec::new(),
            plan_battery: Vec::new(),

            plan: Vec::new(),
        }
    }

    /// Back-compat: if we have no explicit plans, build desktop plan from legacy blocks.
    pub fn rebuild_plan_default_order(&mut self) {
        if !self.plan_desktop.is_empty() {
            return;
        }

        self.plan_desktop = vec![
            PlanStep {
                kind: PlanStepKind::Startup,
                timeout_seconds: self.startup.timeout_seconds,
                command: self.startup.command.clone(),
                resume_command: self.startup.resume_command.clone(),
                notification: self.startup.notification.clone(),
                notification_icon: self.startup.notification_icon.clone(),
                notify_seconds_before: self.startup.notify_seconds_before,
            },
            PlanStep {
                kind: PlanStepKind::Brightness,
                timeout_seconds: self.brightness.timeout_seconds,
                command: self.brightness.command.clone(),
                resume_command: self.brightness.resume_command.clone(),
                notification: self.brightness.notification.clone(),
                notification_icon: self.brightness.notification_icon.clone(),
                notify_seconds_before: self.brightness.notify_seconds_before,
            },
            PlanStep {
                kind: PlanStepKind::LockScreen,
                timeout_seconds: self.lock_screen.timeout_seconds,
                command: self.lock_screen.command.clone(),
                resume_command: self.lock_screen.resume_command.clone(),
                notification: self.lock_screen.notification.clone(),
                notification_icon: self.lock_screen.notification_icon.clone(),
                notify_seconds_before: self.lock_screen.notify_seconds_before,
            },
            PlanStep {
                kind: PlanStepKind::Dpms,
                timeout_seconds: self.dpms.timeout_seconds,
                command: self.dpms.command.clone(),
                resume_command: self.dpms.resume_command.clone(),
                notification: self.dpms.notification.clone(),
                notification_icon: self.dpms.notification_icon.clone(),
                notify_seconds_before: self.dpms.notify_seconds_before,
            },
            PlanStep {
                kind: PlanStepKind::Suspend,
                timeout_seconds: self.suspend.timeout_seconds,
                command: self.suspend.command.clone(),
                resume_command: self.suspend.resume_command.clone(),
                notification: self.suspend.notification.clone(),
                notification_icon: self.suspend.notification_icon.clone(),
                notify_seconds_before: self.suspend.notify_seconds_before,
            },
        ];
    }

    /// Select which plan source is active (after profile application).
    pub fn select_plan_source(&mut self, src: PlanSource) {
        let selected = match src {
            PlanSource::Desktop => &self.plan_desktop,
            PlanSource::Ac => &self.plan_ac,
            PlanSource::Battery => &self.plan_battery,
        };

        self.plan = selected.clone();

        // If selected plan empty, fall back to desktop plan.
        if self.plan.is_empty() {
            self.plan = self.plan_desktop.clone();
        }

        // If *still* empty, fall back to legacy default order.
        if self.plan.is_empty() {
            self.rebuild_plan_default_order();
            self.plan = self.plan_desktop.clone();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionBlock {
    pub timeout_seconds: u64,
    pub command: Option<String>,
    pub resume_command: Option<String>,

    /// Optional notification emitted before firing this block (only if notify_before_action is true).
    pub notification: Option<String>,

    /// Optional icon name/path for this block's notification.
    pub notification_icon: Option<String>,

    /// Wait N seconds after notification before firing the command.
    pub notify_seconds_before: Option<u64>,
}

impl ActionBlock {
    pub fn disabled() -> Self {
        Self {
            timeout_seconds: 0,
            command: None,
            resume_command: None,
            notification: None,
            notification_icon: None,
            notify_seconds_before: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockBlock {
    pub timeout_seconds: u64,
    pub command: Option<String>,
    pub resume_command: Option<String>,

    pub notification: Option<String>,
    pub notification_icon: Option<String>,
    pub notify_seconds_before: Option<u64>,
}

impl LockBlock {
    pub fn disabled() -> Self {
        Self {
            timeout_seconds: 0,
            command: None,
            resume_command: None,
            notification: None,
            notification_icon: None,
            notify_seconds_before: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub mode: ProfileMode,
    pub config: PartialConfig,
}

#[derive(Debug, Clone, Default)]
pub struct PartialConfig {
    // ---- globals ----
    pub enable_loginctl_integration: Option<bool>,
    pub enable_dbus_inhibit: Option<bool>,
    pub pre_suspend_command: Option<Option<String>>,
    pub prepare_sleep_command: Option<Option<String>>,

    pub low_power_when_idle: Option<bool>,
    pub low_power_when_idle_timeout: Option<u64>,

    /// `None` = no override; `Some(None)` = clear; `Some(Some(cmd))` = set command.
    pub lid_close_action: Option<Option<String>>,
    pub lid_open_action: Option<Option<String>>,

    pub monitor_media: Option<bool>,
    pub ignore_remote_media: Option<bool>,
    pub media_inhibit_scope: Option<MediaInhibitScope>,

    pub media_blacklist: Option<Vec<Pattern>>,
    pub suspend_inhibit_media: Option<Vec<Pattern>>,
    pub debounce_seconds: Option<u64>,

    pub notify_on_unpause: Option<bool>,
    pub notify_before_action: Option<bool>,
    pub notification_icon: Option<Option<String>>,

    pub inhibit_apps: Option<Vec<Pattern>>,
    pub suspend_inhibit_apps: Option<Vec<Pattern>>,

    // ---- plan sources ----
    pub plan_desktop: Option<Vec<PlanStep>>,
    pub plan_ac: Option<Vec<PlanStep>>,
    pub plan_battery: Option<Vec<PlanStep>>,

    // ---- legacy blocks ----
    pub startup: Option<ActionBlock>,
    pub brightness: Option<ActionBlock>,
    pub lock_screen: Option<LockBlock>,
    pub dpms: Option<ActionBlock>,
    pub suspend: Option<ActionBlock>,
}

fn same_kind(a: &PlanStepKind, b: &PlanStepKind) -> bool {
    match (a, b) {
        (PlanStepKind::Custom(x), PlanStepKind::Custom(y)) => x == y,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

/// Overlay merge: if step kind exists, replace it; otherwise append.
fn merge_plan(base: &mut Vec<PlanStep>, overlay: Vec<PlanStep>) {
    for s in overlay {
        if let Some(i) = base.iter().position(|b| same_kind(&b.kind, &s.kind)) {
            base[i] = s;
        } else {
            base.push(s);
        }
    }
}

impl PartialConfig {
    pub fn apply_to(&self, base: &mut Config, mode: ProfileMode) {
        // ---- globals ----
        if let Some(v) = self.enable_loginctl_integration {
            base.enable_loginctl_integration = v;
        }
        if let Some(v) = self.enable_dbus_inhibit {
            base.enable_dbus_inhibit = v;
        }

        if let Some(v) = &self.pre_suspend_command {
            base.pre_suspend_command = v.clone();
        }
        if let Some(v) = &self.prepare_sleep_command {
            base.prepare_sleep_command = v.clone();
        }

        if let Some(v) = self.low_power_when_idle {
            base.low_power_when_idle = v;
        }
        if let Some(v) = self.low_power_when_idle_timeout {
            base.low_power_when_idle_timeout = v;
        }

        if let Some(v) = &self.lid_close_action {
            base.lid_close_action = v.clone();
        }
        if let Some(v) = &self.lid_open_action {
            base.lid_open_action = v.clone();
        }

        if let Some(v) = self.monitor_media {
            base.monitor_media = v;
        }
        if let Some(v) = self.ignore_remote_media {
            base.ignore_remote_media = v;
        }
        if let Some(v) = self.media_inhibit_scope {
            base.media_inhibit_scope = v;
        }

        if let Some(v) = &self.media_blacklist {
            base.media_blacklist = v.clone();
        }
        if let Some(v) = &self.suspend_inhibit_media {
            base.suspend_inhibit_media = v.clone();
        }

        if let Some(v) = self.debounce_seconds {
            base.debounce_seconds = v;
        }

        if let Some(v) = self.notify_on_unpause {
            base.notify_on_unpause = v;
        }
        if let Some(v) = self.notify_before_action {
            base.notify_before_action = v;
        }
        if let Some(v) = &self.notification_icon {
            base.notification_icon = v.clone();
        }

        if let Some(v) = &self.inhibit_apps {
            base.inhibit_apps = v.clone();
        }
        if let Some(v) = &self.suspend_inhibit_apps {
            base.suspend_inhibit_apps = v.clone();
        }

        // ---- legacy blocks ----
        if let Some(v) = &self.startup {
            base.startup = v.clone();
        }
        if let Some(v) = &self.brightness {
            base.brightness = v.clone();
        }
        if let Some(v) = &self.lock_screen {
            base.lock_screen = v.clone();
        }
        if let Some(v) = &self.dpms {
            base.dpms = v.clone();
        }
        if let Some(v) = &self.suspend {
            base.suspend = v.clone();
        }

        // ---- plans ----
        match mode {
            ProfileMode::Fresh => {
                if let Some(v) = &self.plan_desktop {
                    base.plan_desktop = v.clone();
                }
                if let Some(v) = &self.plan_ac {
                    base.plan_ac = v.clone();
                }
                if let Some(v) = &self.plan_battery {
                    base.plan_battery = v.clone();
                }
            }
            ProfileMode::Overlay => {
                if let Some(v) = &self.plan_desktop {
                    merge_plan(&mut base.plan_desktop, v.clone());
                }
                if let Some(v) = &self.plan_ac {
                    merge_plan(&mut base.plan_ac, v.clone());
                }
                if let Some(v) = &self.plan_battery {
                    merge_plan(&mut base.plan_battery, v.clone());
                }
            }
        }

        // Active plan is re-selected later.
        base.plan.clear();
    }
}

#[derive(Debug, Clone)]
pub struct ConfigFile {
    pub default: Config,
    pub profiles: Vec<Profile>,
    pub active_profile: Option<String>,
}

impl ConfigFile {
    /// Apply profile (if any), then select plan source (desktop/ac/battery).
    pub fn effective_for(&self, profile_name: Option<&str>, src: PlanSource) -> Option<Config> {
        let name = profile_name.unwrap_or("default");

        let mut cfg = if name == "default" {
            self.default.clone()
        } else {
            let prof = self.profiles.iter().find(|p| p.name == name)?;
            let mut c = match prof.mode {
                ProfileMode::Fresh => Config::disabled(),
                ProfileMode::Overlay => self.default.clone(),
            };
            prof.config.apply_to(&mut c, prof.mode);
            c
        };

        // Ensure desktop fallback exists for very old config shapes.
        if cfg.plan_desktop.is_empty() {
            cfg.rebuild_plan_default_order();
        }

        cfg.select_plan_source(src);
        Some(cfg)
    }
}
