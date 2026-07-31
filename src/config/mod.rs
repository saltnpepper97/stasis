// Author: Dustin Pilgrim
// License: GPL-3.0-only

pub mod bootstrap;
pub mod migrate;

use std::env;
use std::path::{Path, PathBuf};

use rune_cfg::{RuneConfig, Value};

use crate::core::config::{
    ActionBlock, Config, ConfigFile, LockBlock, PartialConfig, Pattern, PlanSource, PlanStep,
    PlanStepKind, Profile, ProfileMode,
};

/// Loaded config + the concrete path that succeeded.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub path: PathBuf,
    pub cfg: ConfigFile,
}

/// Resolve the concrete config path Stasis should use when `--config` is not provided.
///
/// Rules:
/// - Prefer user config if it exists
/// - Otherwise use /etc fallback if it exists
/// - Otherwise return the user path (so errors mention where we'd expect it)
pub fn resolve_default_config_path() -> PathBuf {
    let primary = default_user_config_path();
    let fallback = PathBuf::from("/etc/stasis/stasis.rune");

    if primary.exists() {
        primary
    } else if fallback.exists() {
        fallback
    } else {
        primary
    }
}

/// Load from an explicit path, *but still apply fallbacks*.
///
/// Semantics you wanted for reload:
/// - try the currently-configured path first
/// - if that fails (IO or parse/semantics), fall back to the defaults
pub fn load_from_path(path: impl AsRef<Path>) -> Result<LoadedConfig, String> {
    let primary = path.as_ref().to_path_buf();

    // Default fallbacks (ordered)
    let user = default_user_config_path();
    let etc = PathBuf::from("/etc/stasis/stasis.rune");

    // Avoid trying the same path multiple times.
    let mut fallbacks: Vec<PathBuf> = Vec::new();
    if user != primary {
        fallbacks.push(user);
    }
    if etc != primary && etc != fallbacks.get(0).cloned().unwrap_or_default() {
        fallbacks.push(etc);
    }

    load_with_fallbacks(Some(&primary), &fallbacks)
}

/// Load a config by trying `primary` first (if provided), then each fallback path in order.
///
/// Any failure (read OR parse/semantics) triggers trying the next candidate.
/// If all candidates fail, returns a combined error report.
pub fn load_with_fallbacks(
    primary: Option<&Path>,
    fallbacks: &[PathBuf],
) -> Result<LoadedConfig, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(p) = primary {
        candidates.push(p.to_path_buf());
    }
    candidates.extend_from_slice(fallbacks);

    // Dedupe while preserving order
    let mut uniq: Vec<PathBuf> = Vec::new();
    for c in candidates {
        if !uniq.iter().any(|x| x == &c) {
            uniq.push(c);
        }
    }

    let mut errors: Vec<(PathBuf, String)> = Vec::new();

    for path in uniq {
        match try_load_single(&path) {
            Ok(cfg) => {
                return Ok(LoadedConfig { path, cfg });
            }
            Err(e) => {
                errors.push((path, e));
            }
        }
    }

    // Build a helpful combined message.
    let mut msg = String::new();
    msg.push_str("failed to load config from all candidate locations:\n");
    for (p, e) in errors {
        msg.push_str(&format!("  - {}: {}\n", p.display(), e.trim_end()));
    }
    Err(msg.trim_end().to_string())
}

fn try_load_single(path: &Path) -> Result<ConfigFile, String> {
    let rc = RuneConfig::from_file(path).map_err(|e| format!("failed to read: {e}"))?;
    parse_config_file(&rc).map_err(|e| format!("failed to parse: {e}"))
}

fn parse_config_file(rc: &RuneConfig) -> Result<ConfigFile, String> {
    eventline::scope!(
        "config",
        success = "loaded",
        failure = "failed",
        aborted = "aborted",
        {
            if !rc.has("default") {
                return Err("config must contain a `default:` block".into());
            }

            // NOTE: profile selection is runtime-only (IPC). The config never selects a profile.

            // ---- parse default ----
            let mut cfg = Config::disabled();

            // Global login1 sleep/wake integration toggle. Lock state uses
            // always-on LockedHint plus process-lifetime fallback.
            cfg.enable_loginctl_integration =
                rc.get_or("default.enable_loginctl_integration", false);
            // Session D-Bus inhibit monitor toggle.
            cfg.enable_dbus_inhibit = rc.get_or("default.enable_dbus_inhibit", true);

            // scalars/lists under default
            cfg.pre_suspend_command = opt_nullable_string(rc, "default.pre_suspend_command")?;
            cfg.prepare_sleep_command = opt_nullable_string(rc, "default.prepare_sleep_command")?;

            cfg.low_power_when_idle = rc.get_or("default.low_power_when_idle", false);
            cfg.low_power_when_idle_timeout =
                rc.get_or("default.low_power_when_idle_timeout", 0u64);

            cfg.monitor_media = rc.get_or("default.monitor_media", false);
            cfg.ignore_remote_media = rc.get_or("default.ignore_remote_media", false);

            // allow strings OR /regex/ entries (keep compiled regex)
            cfg.media_blacklist = get_vec_pattern(rc, "default.media_blacklist", Vec::new())?;

            cfg.debounce_seconds = rc.get_or("default.debounce_seconds", 0u64);

            cfg.notify_on_unpause = rc.get_or("default.notify_on_unpause", false);
            cfg.notify_before_action = rc.get_or("default.notify_before_action", false);
            if rc.has("default.notification_icon") {
                cfg.notification_icon = opt_empty_string_as_none(rc, "default.notification_icon")?;
            }

            // allow strings OR /regex/ entries (keep compiled regex)
            cfg.inhibit_apps = get_vec_pattern(rc, "default.inhibit_apps", Vec::new())?;

            // lid actions: plain shell command strings (optional)
            cfg.lid_close_action = parse_lid_action(rc, "default.lid_close_action")?;
            cfg.lid_open_action = parse_lid_action(rc, "default.lid_open_action")?;

            // mirror named blocks for info/debug output and compatibility fields
            cfg.startup = ActionBlock::disabled();
            cfg.brightness = ActionBlock::disabled();
            cfg.dpms = ActionBlock::disabled();
            cfg.suspend = ActionBlock::disabled();
            cfg.lock_screen = LockBlock::disabled();

            // ---- plans ----
            // desktop plan: blocks directly under default (EXCEPT ac/battery containers + globals)
            let plan_desktop = parse_plan_block(
                rc, "default", /*allow_ac_battery_containers=*/ true, &mut cfg,
            )?;

            // laptop plan sources
            let plan_ac = parse_named_plan(rc, "default.ac")?;
            let plan_battery = parse_named_plan(rc, "default.battery")?;

            cfg.plan_desktop = plan_desktop;
            cfg.plan_ac = plan_ac;
            cfg.plan_battery = plan_battery;

            // Do not select cfg.plan here; daemon chooses PlanSource at runtime.
            // But ensure some desktop fallback exists for very old shapes:
            if cfg.plan_desktop.is_empty() {
                cfg.rebuild_plan_default_order();
            }

            // ---- profiles ----
            let profiles = parse_profiles(rc)?;

            let cfg_file = ConfigFile {
                default: cfg,
                profiles,
                active_profile: None,
            };

            log_config_debug(&cfg_file);

            Ok(cfg_file)
        }
    )
}

/// Parse plan steps directly under a block (`default` or a profile block),
/// excluding global knob keys. Optionally treats `ac`/`battery` as containers.
fn parse_plan_block(
    rc: &RuneConfig,
    block_name: &str,
    allow_ac_battery_containers: bool,
    named_blocks_out: &mut Config,
) -> Result<Vec<PlanStep>, String> {
    let keys = rc.get_keys(block_name).unwrap_or_default();
    let mut plan: Vec<PlanStep> = Vec::new();

    fn norm_key(k: &str) -> String {
        k.trim().replace('-', "_").to_lowercase()
    }

    fn is_non_step_key(norm: &str, allow_ac_battery_containers: bool) -> bool {
        if allow_ac_battery_containers && (norm == "ac" || norm == "battery") {
            return true;
        }

        matches!(
            norm,
            "mode"
                | "enable_loginctl_integration"
                | "enable_dbus_inhibit"
                | "pre_suspend_command"
                | "prepare_sleep_command"
                | "low_power_when_idle"
                | "low_power_when_idle_timeout"
                | "monitor_media"
                | "ignore_remote_media"
                | "media_blacklist"
                | "debounce_seconds"
                | "notify_on_unpause"
                | "notify_before_action"
                | "notification_icon"
                | "inhibit_apps"
                | "lid_close_action"
                | "lid_open_action"
        )
    }

    fn looks_like_step(rc: &RuneConfig, base: &str) -> bool {
        rc.has(&format!("{base}.timeout"))
            || rc.has(&format!("{base}.command"))
            || rc.has(&format!("{base}.resume_command"))
            || rc.has(&format!("{base}.notification"))
            || rc.has(&format!("{base}.notification_icon"))
            || rc.has(&format!("{base}.notify_seconds_before"))
    }

    for raw_k in keys {
        let k_norm = norm_key(&raw_k);

        if is_non_step_key(&k_norm, allow_ac_battery_containers) {
            continue;
        }

        let base = format!("{block_name}.{raw_k}");
        if !looks_like_step(rc, &base) {
            continue;
        }

        // Legacy key guard: lock_command no longer exists.
        if rc.has(&format!("{base}.lock_command")) {
            eventline::warn!(
                "config: `{}` uses lock-command, but lock-command was removed; use `lock_screen.command` (locker) instead",
                base
            );
        }

        // Legacy key guard: use_loginctl removed (global enable_loginctl_integration replaces it).
        if rc.has(&format!("{base}.use_loginctl")) {
            eventline::warn!(
                "config: `{}` uses use-loginctl, but use-loginctl was removed; use `default.enable_loginctl_integration` instead",
                base
            );
        }

        match k_norm.as_str() {
            "lock_screen" => {
                let lb = parse_lock_block(rc, &base)?;
                named_blocks_out.lock_screen = lb.clone();

                plan.push(PlanStep {
                    kind: PlanStepKind::LockScreen,
                    timeout_seconds: lb.timeout_seconds,
                    command: lb.command,
                    resume_command: lb.resume_command,
                    notification: lb.notification,
                    notification_icon: lb.notification_icon,
                    notify_seconds_before: lb.notify_seconds_before,
                });
            }
            "startup" => {
                let ab = parse_action_block(rc, &base)?;
                named_blocks_out.startup = ab.clone();
                plan.push(step_from_action_block(PlanStepKind::Startup, ab));
            }
            "brightness" => {
                let ab = parse_action_block(rc, &base)?;
                named_blocks_out.brightness = ab.clone();
                plan.push(step_from_action_block(PlanStepKind::Brightness, ab));
            }
            "dpms" => {
                let ab = parse_action_block(rc, &base)?;
                named_blocks_out.dpms = ab.clone();
                plan.push(step_from_action_block(PlanStepKind::Dpms, ab));
            }
            "suspend" => {
                let ab = parse_action_block(rc, &base)?;
                named_blocks_out.suspend = ab.clone();
                plan.push(step_from_action_block(PlanStepKind::Suspend, ab));
            }
            other => {
                let ab = parse_action_block(rc, &base)?;
                plan.push(step_from_action_block(
                    PlanStepKind::Custom(other.to_string()),
                    ab,
                ));
            }
        }
    }

    Ok(plan)
}

/// Parse a named plan container like `default.ac` or `profile_name.battery`.
fn parse_named_plan(rc: &RuneConfig, base: &str) -> Result<Vec<PlanStep>, String> {
    if !rc.has(base) {
        return Ok(Vec::new());
    }

    let mut dummy_cfg = Config::disabled();
    parse_plan_block(
        rc,
        base,
        /*allow_ac_battery_containers=*/ false,
        &mut dummy_cfg,
    )
}

/// Parse profiles (top-level blocks other than `default`).
/// A name is considered a profile ONLY if it is an object/block (i.e. it has subkeys).
/// If it's a scalar/array/etc, it is treated as a global and ignored here.
fn parse_profiles(rc: &RuneConfig) -> Result<Vec<Profile>, String> {
    let top = rc.get_keys("").unwrap_or_default();

    let mut profiles: Vec<Profile> = Vec::new();

    for name in top {
        // Non-profile keys
        if name.is_empty() || name == "default" {
            continue;
        }
        if name.starts_with('@') {
            continue; // metadata
        }

        // If it doesn't exist at all, skip.
        if !rc.has(&name) {
            continue;
        }

        // Only treat as a profile if this top-level key is a block/object
        // (i.e. it has subkeys). Scalars/globals will have no subkeys here.
        let subkeys = rc.get_keys(&name).unwrap_or_default();
        if subkeys.is_empty() {
            continue; // global scalar/array/etc, not a profile block
        }

        let mode_s =
            opt_string(rc, format!("{name}.mode"))?.unwrap_or_else(|| "overlay".to_string());

        let mode = match mode_s.trim().to_lowercase().as_str() {
            "overlay" => ProfileMode::Overlay,
            "fresh" => ProfileMode::Fresh,
            other => {
                return Err(format!(
                    "config error at {}.mode: expected \"overlay\" or \"fresh\", got \"{}\"",
                    name, other
                ));
            }
        };

        let mut pc = PartialConfig::default();

        // globals (profile-level overrides)
        pc.enable_loginctl_integration =
            opt_bool(rc, format!("{name}.enable_loginctl_integration"))?;
        pc.enable_dbus_inhibit = opt_bool(rc, format!("{name}.enable_dbus_inhibit"))?;
        pc.pre_suspend_command = opt_nullable_string2(rc, format!("{name}.pre_suspend_command"))?;
        pc.prepare_sleep_command =
            opt_nullable_string2(rc, format!("{name}.prepare_sleep_command"))?;

        pc.low_power_when_idle = opt_bool(rc, format!("{name}.low_power_when_idle"))?;
        pc.low_power_when_idle_timeout =
            opt_u64(rc, format!("{name}.low_power_when_idle_timeout"))?;

        pc.monitor_media = opt_bool(rc, format!("{name}.monitor_media"))?;
        pc.ignore_remote_media = opt_bool(rc, format!("{name}.ignore_remote_media"))?;

        pc.media_blacklist = opt_vec_pattern(rc, &format!("{name}.media_blacklist"))?;
        pc.debounce_seconds = opt_u64(rc, format!("{name}.debounce_seconds"))?;
        pc.notify_on_unpause = opt_bool(rc, format!("{name}.notify_on_unpause"))?;
        pc.notify_before_action = opt_bool(rc, format!("{name}.notify_before_action"))?;
        pc.notification_icon =
            opt_empty_string_as_none_override(rc, &format!("{name}.notification_icon"))?;
        pc.inhibit_apps = opt_vec_pattern(rc, &format!("{name}.inhibit_apps"))?;

        // lid action profile overrides (None = no override, Some(None) = clear, Some(Some(cmd)) = set)
        pc.lid_close_action = parse_lid_action_override(rc, &format!("{name}.lid_close_action"))?;
        pc.lid_open_action = parse_lid_action_override(rc, &format!("{name}.lid_open_action"))?;

        // plan overrides
        let mut named_blocks_dummy = Config::disabled();
        let plan_desktop = parse_plan_block(
            rc,
            &name,
            /*allow_ac_battery_containers=*/ true,
            &mut named_blocks_dummy,
        )?;
        let plan_ac = parse_named_plan(rc, &format!("{name}.ac"))?;
        let plan_battery = parse_named_plan(rc, &format!("{name}.battery"))?;

        if !plan_desktop.is_empty() {
            pc.plan_desktop = Some(plan_desktop);
        }
        if !plan_ac.is_empty() {
            pc.plan_ac = Some(plan_ac);
        }
        if !plan_battery.is_empty() {
            pc.plan_battery = Some(plan_battery);
        }

        profiles.push(Profile {
            name: name.clone(),
            mode,
            config: pc,
        });
    }

    Ok(profiles)
}

fn step_from_action_block(kind: PlanStepKind, ab: ActionBlock) -> PlanStep {
    PlanStep {
        kind,
        timeout_seconds: ab.timeout_seconds,
        command: ab.command,
        resume_command: ab.resume_command,
        notification: ab.notification,
        notification_icon: ab.notification_icon,
        notify_seconds_before: ab.notify_seconds_before,
    }
}

fn parse_action_block(rc: &RuneConfig, base: &str) -> Result<ActionBlock, String> {
    let timeout_seconds = rc.get_or(&format!("{base}.timeout"), 0u64);

    let command = opt_string(rc, format!("{base}.command"))?;
    let resume_command = opt_string(rc, format!("{base}.resume_command"))?;

    // allow notifications on ANY action block (custom, dpms, suspend, etc.)
    let notification = opt_string(rc, format!("{base}.notification"))?;
    let notification_icon = opt_empty_string_as_none(rc, format!("{base}.notification_icon"))?;
    let notify_seconds_before = opt_u64(rc, format!("{base}.notify_seconds_before"))?;

    Ok(ActionBlock {
        timeout_seconds,
        command,
        resume_command,
        notification,
        notification_icon,
        notify_seconds_before,
    })
}

fn parse_lock_block(rc: &RuneConfig, base: &str) -> Result<LockBlock, String> {
    let timeout_seconds = rc.get_or(&format!("{base}.timeout"), 0u64);

    let command = opt_string(rc, format!("{base}.command"))?;
    let resume_command = opt_string(rc, format!("{base}.resume_command"))?;

    let notification = opt_string(rc, format!("{base}.notification"))?;
    let notification_icon = opt_empty_string_as_none(rc, format!("{base}.notification_icon"))?;
    let notify_seconds_before = opt_u64(rc, format!("{base}.notify_seconds_before"))?;

    Ok(LockBlock {
        timeout_seconds,
        command,
        resume_command,
        notification,
        notification_icon,
        notify_seconds_before,
    })
}

// ---- lid action parsing ----

/// Read an optional lid action command string.
/// Empty string is treated the same as absent (returns None).
fn parse_lid_action(rc: &RuneConfig, path: &str) -> Result<Option<String>, String> {
    match opt_string(rc, path)? {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => Ok(Some(s)),
    }
}

/// For profile overrides:
/// - missing key        => None            (no override; leave default as-is)
/// - present but empty  => Some(None)      (clear the action)
/// - present non-empty  => Some(Some(cmd)) (set a new command)
fn parse_lid_action_override(
    rc: &RuneConfig,
    path: &str,
) -> Result<Option<Option<String>>, String> {
    match opt_string(rc, path)? {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(Some(None)),
        Some(s) => Ok(Some(Some(s))),
    }
}

// ---- minimal typed helpers ----

fn opt_string(rc: &RuneConfig, path: impl AsRef<str>) -> Result<Option<String>, String> {
    let p = path.as_ref();
    rc.get_optional::<String>(p)
        .map_err(|e| format!("config error at {}: {e}", p))
}

fn opt_empty_string_as_none(
    rc: &RuneConfig,
    path: impl AsRef<str>,
) -> Result<Option<String>, String> {
    Ok(opt_string(rc, path)?.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }))
}

fn opt_empty_string_as_none_override(
    rc: &RuneConfig,
    path: &str,
) -> Result<Option<Option<String>>, String> {
    match opt_string(rc, path)? {
        None => Ok(None),
        Some(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(Some(None))
            } else {
                Ok(Some(Some(trimmed.to_string())))
            }
        }
    }
}

fn opt_bool(rc: &RuneConfig, path: impl AsRef<str>) -> Result<Option<bool>, String> {
    let p = path.as_ref();
    rc.get_optional::<bool>(p)
        .map_err(|e| format!("config error at {}: {e}", p))
}

fn opt_u64(rc: &RuneConfig, path: impl AsRef<str>) -> Result<Option<u64>, String> {
    let p = path.as_ref();
    rc.get_optional::<u64>(p)
        .map_err(|e| format!("config error at {}: {e}", p))
}

/// Optional string list helper (still used by other knobs sometimes).
#[allow(dead_code)]
fn opt_vec_string(rc: &RuneConfig, path: impl AsRef<str>) -> Result<Option<Vec<String>>, String> {
    let p = path.as_ref();
    rc.get_optional::<Vec<String>>(p)
        .map_err(|e| format!("config error at {}: {e}", p))
}

fn opt_nullable_string(rc: &RuneConfig, path: impl AsRef<str>) -> Result<Option<String>, String> {
    let p = path.as_ref();
    rc.get_optional::<Option<String>>(p)
        .map_err(|e| format!("config error at {}: {e}", p))
        .map(|v| v.flatten())
}

fn opt_nullable_string2(
    rc: &RuneConfig,
    path: impl AsRef<str>,
) -> Result<Option<Option<String>>, String> {
    let p = path.as_ref();
    rc.get_optional::<Option<String>>(p)
        .map_err(|e| format!("config error at {}: {e}", p))
}

/// Read an optional array where each entry may be either `"string"` or `/regex/`.
/// Returns Vec<Pattern>, preserving compiled regex from rune-cfg.
fn opt_vec_pattern(rc: &RuneConfig, path: &str) -> Result<Option<Vec<Pattern>>, String> {
    if !rc.has(path) {
        return Ok(None);
    }

    let v = rc
        .get_value(path)
        .map_err(|e| format!("config error at {}: {e}", path))?;

    fn norm_lit(s: &str) -> String {
        s.trim().to_lowercase()
    }

    match v {
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                match item {
                    Value::String(s) => {
                        let lit = norm_lit(&s);
                        if !lit.is_empty() {
                            out.push(Pattern::Literal(lit));
                        }
                    }
                    Value::Regex(r) => out.push(Pattern::Regex(r)),
                    other => {
                        return Err(format!(
                            "config error at {}: expected string or regex, got {:?}",
                            path, other
                        ));
                    }
                }
            }
            Ok(Some(out))
        }
        other => Err(format!(
            "config error at {}: expected array, got {:?}",
            path, other
        )),
    }
}

fn get_vec_pattern(
    rc: &RuneConfig,
    path: &str,
    default: Vec<Pattern>,
) -> Result<Vec<Pattern>, String> {
    Ok(opt_vec_pattern(rc, path)?.unwrap_or(default))
}

/// Shared path helper used by bootstrap + default resolution.
pub(crate) fn default_user_config_path() -> PathBuf {
    let dir: PathBuf = if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else {
        let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".config")
    };

    dir.join("stasis").join("stasis.rune")
}

fn log_config_debug(cfg_file: &ConfigFile) {
    let cfg = &cfg_file.default;

    eventline::debug!("Parsed config:");
    eventline::debug!(
        "  enable_loginctl_integration = {:?}",
        cfg.enable_loginctl_integration
    );
    eventline::debug!("  enable_dbus_inhibit = {:?}", cfg.enable_dbus_inhibit);
    eventline::debug!("  pre_suspend_command = {:?}", cfg.pre_suspend_command);
    eventline::debug!("  prepare_sleep_command = {:?}", cfg.prepare_sleep_command);
    eventline::debug!(
        "  low_power_when_idle = {} (timeout {}s)",
        cfg.low_power_when_idle,
        cfg.low_power_when_idle_timeout
    );

    eventline::debug!("  lid_close_action = {:?}", cfg.lid_close_action);
    eventline::debug!("  lid_open_action  = {:?}", cfg.lid_open_action);

    eventline::debug!("  monitor_media = {:?}", cfg.monitor_media);
    eventline::debug!("  ignore_remote_media = {:?}", cfg.ignore_remote_media);
    eventline::debug!("  media_blacklist = {:?}", cfg.media_blacklist);

    eventline::debug!("  debounce_seconds = {:?}", cfg.debounce_seconds);

    eventline::debug!("  notify_on_unpause = {:?}", cfg.notify_on_unpause);
    eventline::debug!("  notify_before_action = {:?}", cfg.notify_before_action);
    eventline::debug!("  notification_icon = {:?}", cfg.notification_icon);

    eventline::debug!("  inhibit_apps = {:?}", cfg.inhibit_apps);

    eventline::debug!("Plan sources:");
    eventline::debug!("  desktop steps = {}", cfg.plan_desktop.len());
    eventline::debug!("  ac steps      = {}", cfg.plan_ac.len());
    eventline::debug!("  battery steps = {}", cfg.plan_battery.len());

    eventline::debug!("Desktop plan (enabled steps):");
    dump_plan(&cfg.plan_desktop);

    if !cfg.plan_ac.is_empty() {
        eventline::debug!("AC plan (enabled steps):");
        dump_plan(&cfg.plan_ac);
    }

    if !cfg.plan_battery.is_empty() {
        eventline::debug!("Battery plan (enabled steps):");
        dump_plan(&cfg.plan_battery);
    }

    if !cfg_file.profiles.is_empty() {
        eventline::debug!("Profiles:");
        for p in &cfg_file.profiles {
            eventline::debug!("  - {} mode={:?}", p.name, p.mode);
        }
    }

    let _ = PlanSource::Desktop;
}

fn dump_plan(plan: &[PlanStep]) {
    let mut n = 0usize;

    for step in plan {
        if step.command.is_none() && step.resume_command.is_none() && step.notification.is_none() {
            continue;
        }

        n += 1;

        let kind: String = match &step.kind {
            PlanStepKind::Startup => "startup".into(),
            PlanStepKind::Brightness => "brightness".into(),
            PlanStepKind::LockScreen => "lock_screen".into(),
            PlanStepKind::Dpms => "dpms".into(),
            PlanStepKind::Suspend => "suspend".into(),
            PlanStepKind::Custom(s) => s.clone(),
        };

        let mut line = if step.timeout_seconds == 0 {
            format!("  {:02} {}: instant", n, kind)
        } else {
            format!("  {:02} {}: timeout={}s", n, kind, step.timeout_seconds)
        };

        if let Some(cmd) = &step.command {
            line.push_str(&format!(", command=\"{}\"", cmd));
        }
        if let Some(resume_cmd) = &step.resume_command {
            line.push_str(&format!(", resume_command=\"{}\"", resume_cmd));
        }
        if let Some(notification) = &step.notification {
            line.push_str(&format!(", notification=\"{}\"", notification));
            if let Some(icon) = &step.notification_icon {
                line.push_str(&format!(", notification_icon=\"{}\"", icon));
            }
            if let Some(sec) = step.notify_seconds_before {
                line.push_str(&format!(", notify_seconds_before={}s", sec));
            }
        }

        eventline::debug!("{}", line);
    }
}
