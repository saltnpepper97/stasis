// Author: Dustin Pilgrim
// License: GPL-3.0-only

use crate::core::{
    config::{Config, Pattern, PlanStepKind},
    state::State,
};

pub struct RenderedInfo {
    pub pretty: String,
    pub tooltip: String,
}

pub fn render_info(cfg_opt: Option<&Config>, state: &State, now_ms: u64) -> RenderedInfo {
    let mut pretty = String::new();

    // Always render full info now.
    pretty.push_str("◆ STATUS\n");
    pretty.push_str(&render_status(state, cfg_opt, now_ms));
    pretty.push('\n');
    pretty.push_str("◆ CONFIGURATION\n");
    pretty.push_str(&render_config(cfg_opt, state));

    // Waybar tooltip: compact, always “status-like”
    let tooltip = render_tooltip_compact(state, cfg_opt, now_ms);

    RenderedInfo {
        pretty: pretty.trim_end().to_string(),
        tooltip: tooltip.trim_end().to_string(),
    }
}

fn render_status(state: &State, cfg_opt: Option<&Config>, now_ms: u64) -> String {
    let mut out = String::new();

    out.push_str(&format!("Profile: {}\n", profile_label(state)));
    out.push_str(&format!("Plan Source: {:?}\n", state.plan_source()));

    let paused_reason = if state.is_locked() {
        Some("locked")
    } else if state.manually_paused() {
        Some("manual")
    } else if state.system_paused() {
        Some("system")
    } else if state.inhibitors_active() {
        Some("inhibitors")
    } else {
        None
    };

    match paused_reason {
        Some("locked") => out.push_str("State: locked\n"),
        Some("manual") => out.push_str("State: manual\n"),
        Some(_) => out.push_str("State: inhibited\n"),
        None => {
            if state.debounce_pending() {
                out.push_str("State: waiting\n");
            } else {
                out.push_str("State: active\n");
            }
        }
    }

    let app = state.app_inhibitor_count();
    let media = state.media_inhibitor_count();
    let suspend_app = state.suspend_app_inhibitor_count();
    let suspend_media = state.suspend_media_inhibitor_count();

    out.push_str(&format!(
        "Manual Pause: {}\n",
        yesno(state.manually_paused())
    ));
    out.push_str(&format!("Paused: {}\n", yesno(state.paused())));
    out.push_str(&format!("Apps Inhibiting: {}\n", app));
    out.push_str(&format!("Media Players Playing: {}\n", media));
    out.push_str(&format!("Apps Blocking Suspend: {}\n", suspend_app));
    out.push_str(&format!(
        "Media Players Blocking Suspend: {}\n",
        suspend_media
    ));
    out.push_str(&format!(
        "D-Bus Inhibiting: {}\n",
        yesno(state.browser_activity_active(now_ms))
    ));

    if let Some(cfg) = cfg_opt {
        if let Some(line) = next_step_line(cfg, state, now_ms) {
            out.push_str(&line);
            out.push('\n');
        }
    } else {
        out.push_str("Next: (config selection failed)\n");
    }

    out
}

fn render_tooltip_compact(state: &State, cfg_opt: Option<&Config>, now_ms: u64) -> String {
    let mut t = String::new();

    t.push_str(&format!("Profile: {}\n", profile_label(state)));
    t.push_str(&format!("Plan Source: {:?}\n", state.plan_source()));

    if state.is_locked() {
        t.push_str("State: locked\n");
    } else if state.manually_paused() {
        t.push_str("State: manual\n");
    } else if state.system_paused() {
        t.push_str("State: inhibited\n");
    } else if state.inhibitors_active() {
        t.push_str("State: inhibited\n");
    } else if state.debounce_pending() {
        t.push_str("State: waiting\n");
    } else {
        t.push_str("State: active\n");
    }

    let app = state.app_inhibitor_count();
    let media = state.media_inhibitor_count();
    let suspend_app = state.suspend_app_inhibitor_count();
    let suspend_media = state.suspend_media_inhibitor_count();

    // Keep tooltip compact but consistent.
    t.push_str(&format!(
        "Manual Pause: {}\n",
        yesno(state.manually_paused())
    ));
    t.push_str(&format!("Paused: {}\n", yesno(state.paused())));
    t.push_str(&format!("Apps Inhibiting: {}\n", app));
    t.push_str(&format!("Media Players Playing: {}\n", media));
    t.push_str(&format!("Apps Blocking Suspend: {}\n", suspend_app));
    t.push_str(&format!(
        "Media Players Blocking Suspend: {}\n",
        suspend_media
    ));
    t.push_str(&format!(
        "D-Bus Inhibiting: {}\n",
        yesno(state.browser_activity_active(now_ms))
    ));

    if let Some(cfg) = cfg_opt {
        if let Some(line) = next_step_line(cfg, state, now_ms) {
            t.push_str(&line);
        }
    } else {
        t.push_str("Next: (config selection failed)");
    }

    t
}

fn render_config(cfg_opt: Option<&Config>, state: &State) -> String {
    let Some(cfg) = cfg_opt else {
        return "Config: (selection failed)\n".to_string();
    };

    let mut out = String::new();

    out.push_str(&format!("Debounce: {}s\n", cfg.debounce_seconds));
    out.push_str(&format!(
        "NotifyBeforeAction: {}\n",
        yesno(cfg.notify_before_action)
    ));
    out.push_str(&format!(
        "NotifyOnUnpause: {}\n",
        yesno(cfg.notify_on_unpause)
    ));

    out.push_str(&format!("MonitorMedia: {}\n", yesno(cfg.monitor_media)));
    out.push_str(&format!("MediaInhibitScope: {}\n", cfg.media_inhibit_scope));
    out.push_str(&format!(
        "IgnoreRemoteMedia: {}\n",
        yesno(cfg.ignore_remote_media)
    ));
    out.push_str(&format!(
        "ListenDbusInhibit: {}\n",
        yesno(cfg.enable_dbus_inhibit)
    ));

    if !cfg.inhibit_apps.is_empty() {
        out.push_str(&format!(
            "InhibitApps: {}\n",
            join_patterns(&cfg.inhibit_apps)
        ));
    } else {
        out.push_str("InhibitApps: none\n");
    }

    if !cfg.suspend_inhibit_apps.is_empty() {
        out.push_str(&format!(
            "SuspendInhibitApps: {}\n",
            join_patterns(&cfg.suspend_inhibit_apps)
        ));
    } else {
        out.push_str("SuspendInhibitApps: none\n");
    }

    if !cfg.media_blacklist.is_empty() {
        out.push_str(&format!(
            "MediaBlacklist: {}\n",
            join_patterns(&cfg.media_blacklist)
        ));
    } else {
        out.push_str("MediaBlacklist: none\n");
    }

    if !cfg.suspend_inhibit_media.is_empty() {
        out.push_str(&format!(
            "SuspendInhibitMedia: {}\n",
            join_patterns(&cfg.suspend_inhibit_media)
        ));
    } else {
        out.push_str("SuspendInhibitMedia: none\n");
    }

    if let Some(cmd) = cfg.pre_suspend_command.as_deref() {
        out.push_str(&format!("PreSuspendCommand: {cmd}\n"));
    } else {
        out.push_str("PreSuspendCommand: none\n");
    }

    out.push_str("\nPlan:\n");
    out.push_str(&render_plan(cfg, state));

    out
}

fn render_plan(cfg: &Config, state: &State) -> String {
    let mut out = String::new();

    // current effective “cursor” (skip disabled)
    let mut cur = state.step_index();
    while cur < cfg.plan.len() && !step_enabled(cfg, cur) {
        cur += 1;
    }

    // Build enabled rows first so we can compute widths for alignment.
    // (idx, name, timeout_seconds, notify_before_opt, has_cmd)
    let mut rows: Vec<(usize, String, u64, Option<u64>, bool)> = Vec::new();

    for (i, step) in cfg.plan.iter().enumerate() {
        if !step_enabled(cfg, i) {
            continue;
        }

        let name = match &step.kind {
            PlanStepKind::Custom(s) => format!("custom:{s}"),
            other => format!("{other:?}").to_ascii_lowercase(),
        };

        let notify_before = if cfg.notify_before_action && step.notification.is_some() {
            Some(step.notify_seconds_before.unwrap_or(0))
        } else {
            None
        };

        let has_cmd = step.command.is_some();

        rows.push((i, name, step.timeout_seconds, notify_before, has_cmd));
    }

    if rows.is_empty() {
        out.push_str("  (no enabled steps)\n");
        return out;
    }

    // Column widths
    let name_w = rows
        .iter()
        .map(|(_, n, _, _, _)| n.len())
        .max()
        .unwrap_or(0)
        .max(8);

    let secs_w = rows
        .iter()
        .map(|(_, _, s, _, _)| s.to_string().len())
        .max()
        .unwrap_or(1)
        .max(1);

    for (i, name, timeout_seconds, notify_before, has_cmd) in rows {
        let marker = if i == cur { "→" } else { " " };

        out.push_str(&format!(
            "  {marker} {:>2}. {:<name_w$}  {:>secs_w$}s",
            i + 1,
            name,
            timeout_seconds,
            name_w = name_w,
            secs_w = secs_w
        ));

        if let Some(n) = notify_before {
            out.push_str(&format!("  notify+{n}s"));
        }

        if has_cmd {
            out.push_str("  cmd");
        }

        out.push('\n');
    }

    out
}

fn profile_label(state: &State) -> &str {
    state.active_profile().unwrap_or("none")
}

fn yesno(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

fn step_enabled(cfg: &Config, idx: usize) -> bool {
    if idx >= cfg.plan.len() {
        return false;
    }

    cfg.plan[idx].enabled()
}

fn next_step_line(cfg: &Config, state: &State, now_ms: u64) -> Option<String> {
    // 1) If paused, Next should ONLY be "paused for Xs" (no due calculations).
    if state.paused() {
        if let Some(t0) = state.pause_started_ms() {
            let s = now_ms.saturating_sub(t0) / 1000;
            return Some(format!("Next: paused for {s}s"));
        }
        return Some("Next: paused".to_string());
    }

    // 2) If waiting (debounce pending), Next should ONLY be "waiting for idle".
    if state.debounce_pending() {
        return Some("Next: waiting for idle".to_string());
    }

    // Otherwise show computed next step timing as before.
    let mut idx = state.step_index();
    while idx < cfg.plan.len() && !step_enabled(cfg, idx) {
        idx += 1;
    }
    if idx >= cfg.plan.len() {
        return Some("Next: (end of plan)".to_string());
    }

    let step = &cfg.plan[idx];
    if matches!(step.kind, PlanStepKind::Suspend)
        && let Some(started_ms) = state.suspend_hold_started_ms()
    {
        let seconds = now_ms.saturating_sub(started_ms) / 1000;
        return Some(format!("Next: Suspend inhibited for {seconds}s"));
    }

    let name = match &step.kind {
        PlanStepKind::Custom(s) => format!("Custom({s})"),
        other => format!("{other:?}"),
    };

    let debounce_ms = cfg.debounce_seconds.saturating_mul(1000);
    let timeout_ms = step.timeout_seconds.saturating_mul(1000);
    let base_due_ms = state
        .step_base_ms()
        .saturating_add(debounce_ms)
        .saturating_add(timeout_ms);

    let has_notification = cfg.notify_before_action && step.notification.is_some();
    let notify_wait_ms = step.notify_seconds_before.unwrap_or(0).saturating_mul(1000);

    if has_notification {
        if !state.pre_action_notify_sent() {
            if now_ms >= base_due_ms {
                return Some(format!("Next: {name} (notify due now)"));
            }
            let s = (base_due_ms - now_ms) / 1000;
            return Some(format!("Next: {name} (notify in {s}s)"));
        }

        let due_after_notify_ms = state.pre_action_notify_ms().saturating_add(notify_wait_ms);
        if now_ms >= due_after_notify_ms {
            return Some(format!("Next: {name} (due now)"));
        }
        let s = (due_after_notify_ms - now_ms) / 1000;
        return Some(format!("Next: {name} (runs in {s}s)"));
    }

    if now_ms >= base_due_ms {
        return Some(format!("Next: {name} (due now)"));
    }

    let s = (base_due_ms - now_ms) / 1000;
    Some(format!("Next: {name} in {s}s"))
}

fn join_patterns(v: &[Pattern]) -> String {
    v.iter().map(|p| p.render()).collect::<Vec<_>>().join(", ")
}
