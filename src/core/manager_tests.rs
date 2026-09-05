// Author: Dustin Pilgrim
// License: GPL-3.0-only

use crate::core::action::Action;
use crate::core::blame::{DbusHold, Login1IdleHold};
use crate::core::config::{
    Config, ConfigFile, PartialConfig, Pattern, PlanSource, PlanStep, PlanStepKind, Profile,
    ProfileMode,
};
use crate::core::events::{ActivityKind, Event, LockSource};
use crate::core::manager::Manager;
use crate::core::state::State;

fn cfg_with_plan(plan: Vec<PlanStep>) -> ConfigFile {
    let mut cfg = Config::disabled();

    // effective_for() selects plan_* into cfg.plan; tests must populate plan_desktop.
    cfg.plan_desktop = plan;

    ConfigFile {
        default: cfg,
        profiles: vec![],
        active_profile: None,
    }
}

fn cfg_with_plan_and_notify(
    plan: Vec<PlanStep>,
    debounce_seconds: u64,
    notify_before_action: bool,
) -> ConfigFile {
    let mut cfg = Config::disabled();

    cfg.plan_desktop = plan;
    cfg.debounce_seconds = debounce_seconds;
    cfg.notify_before_action = notify_before_action;

    ConfigFile {
        default: cfg,
        profiles: vec![],
        active_profile: None,
    }
}

fn step(kind: PlanStepKind, timeout: u64, cmd: &str) -> PlanStep {
    PlanStep {
        kind,
        timeout_seconds: timeout,
        command: Some(cmd.to_string()),
        resume_command: None,
        notification: None,
        notification_icon: None,
        notify_seconds_before: None,
    }
}

fn enter_idle(mgr: &mut Manager, state: &mut State, now_ms: u64) {
    let _ = mgr
        .handle_event(state, Event::CompositorIdled { now_ms })
        .unwrap();
}

#[test]
fn watch_event_reports_only_shell_facing_state() {
    let mgr = Manager::new(cfg_with_plan(vec![]));
    let mut state = State::new(0);

    assert_eq!(mgr.watch_event(&state).state, "waiting");
    assert_eq!(mgr.watch_event(&state).profile, "default");

    state.set_manually_paused(true);
    state.set_paused(true);
    state.set_active_profile(Some("work".to_string()));

    let event = mgr.watch_event(&state);
    assert_eq!(event.state, "manual");
    assert!(event.paused);
    assert!(event.manually_paused);
    assert_eq!(event.profile, "work");
}

#[test]
fn blame_snapshot_reports_named_sources_and_live_dbus_metadata() {
    let mut mgr = Manager::new(cfg_with_plan(vec![]));
    let mut state = State::new(0);

    mgr.handle_event(
        &mut state,
        Event::AppInhibitorCount {
            count: 1,
            suspend_count: 0,
            now_ms: 1_000,
        },
    )
    .unwrap();
    mgr.handle_event(
        &mut state,
        Event::AppInhibitorSources {
            sources: vec!["handbrake".to_string()],
            suspend_sources: Vec::new(),
            now_ms: 1_000,
        },
    )
    .unwrap();
    mgr.handle_event(
        &mut state,
        Event::DbusInhibitorsChanged {
            holds: vec![DbusHold {
                status: "live".to_string(),
                protocol: "org.freedesktop.ScreenSaver".to_string(),
                source: "legacy-cookie".to_string(),
                sender: ":1.9".to_string(),
                application: Some("Voice chat".to_string()),
                process: Some("chatgpt".to_string()),
                pid: Some(99),
                reason: Some("screen sharing".to_string()),
                flags: None,
                started_at_ms: 1_000,
                age_ms: 0,
                cookie: Some(4),
                handle: None,
            }],
            now_ms: 1_000,
        },
    )
    .unwrap();

    let blame = mgr.blame_snapshot(&state, 6_000);
    assert_eq!(blame.schema_version, 1);
    assert_eq!(blame.app_inhibitors.sources, ["handbrake"]);
    assert_eq!(blame.dbus_holds[0].age_ms, 5_000);
    assert_eq!(blame.dbus_holds[0].cookie, Some(4));
}

#[test]
fn per_step_timers_chain_from_previous_fire() {
    let plan = vec![
        step(PlanStepKind::Startup, 5, "a"),
        step(PlanStepKind::Dpms, 7, "b"),
    ];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 4000 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 5000 })
        .unwrap();
    assert_eq!(actions.len(), 1);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 11999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 12000 })
        .unwrap();
    assert_eq!(actions.len(), 1);
}

#[test]
fn skips_disabled_steps() {
    let mut disabled = step(PlanStepKind::Startup, 0, "nope");
    disabled.command = None;

    let plan = vec![disabled, step(PlanStepKind::Dpms, 1, "yes")];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1000 })
        .unwrap();
    assert_eq!(actions.len(), 1);
}

#[test]
fn lock_step_skipped_if_already_locked() {
    let plan = vec![
        step(PlanStepKind::LockScreen, 1, "lock"),
        step(PlanStepKind::Dpms, 1, "dpms"),
    ];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    state.set_locked(true);
    enter_idle(&mut mgr, &mut state, 0);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1000 })
        .unwrap();

    assert_eq!(actions.len(), 1);
}

#[test]
fn locked_hint_keeps_veila_episode_locked_after_client_exit() {
    let plan = vec![
        step(PlanStepKind::LockScreen, 1, "veila lock --wait-ready"),
        step(PlanStepKind::Dpms, 1, "dpms"),
    ];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1_000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunLockScreen {
            command: "veila lock --wait-ready".to_string()
        }]
    );

    mgr.handle_event(
        &mut state,
        Event::SessionLocked {
            source: LockSource::LockerProcess,
            now_ms: 1_001,
        },
    )
    .unwrap();
    mgr.handle_event(
        &mut state,
        Event::SessionLocked {
            source: LockSource::LockedHint,
            now_ms: 1_002,
        },
    )
    .unwrap();

    let actions = mgr
        .handle_event(
            &mut state,
            Event::SessionUnlocked {
                source: LockSource::LockerProcess,
                now_ms: 1_003,
            },
        )
        .unwrap();

    assert!(actions.is_empty());
    assert!(state.is_locked());
    assert!(state.system_lock_confirmed());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 2_000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "dpms".to_string()
        }]
    );
    assert!(state.is_locked());

    mgr.handle_event(
        &mut state,
        Event::SessionUnlocked {
            source: LockSource::LockedHint,
            now_ms: 2_001,
        },
    )
    .unwrap();

    assert!(!state.is_locked());
    assert!(!state.system_lock_confirmed());
}

#[test]
fn foreground_locker_process_remains_the_fallback_authority() {
    let mut mgr = Manager::new(cfg_with_plan(vec![]));
    let mut state = State::new(0);

    mgr.handle_event(
        &mut state,
        Event::SessionLocked {
            source: LockSource::LockerProcess,
            now_ms: 1,
        },
    )
    .unwrap();
    assert!(state.is_locked());
    assert!(!state.system_lock_confirmed());

    mgr.handle_event(
        &mut state,
        Event::SessionUnlocked {
            source: LockSource::LockedHint,
            now_ms: 2,
        },
    )
    .unwrap();
    assert!(
        state.is_locked(),
        "unconfirmed LockedHint=false must not override process tracking"
    );

    mgr.handle_event(
        &mut state,
        Event::SessionUnlocked {
            source: LockSource::LockerProcess,
            now_ms: 3,
        },
    )
    .unwrap();
    assert!(!state.is_locked());
}

#[test]
fn locked_hint_tracks_an_external_lock_without_a_locker_process() {
    let mut mgr = Manager::new(cfg_with_plan(vec![]));
    let mut state = State::new(0);

    mgr.handle_event(
        &mut state,
        Event::SessionLocked {
            source: LockSource::LockedHint,
            now_ms: 1,
        },
    )
    .unwrap();
    assert!(state.is_locked());
    assert!(state.system_lock_confirmed());

    mgr.handle_event(
        &mut state,
        Event::SessionUnlocked {
            source: LockSource::LockedHint,
            now_ms: 2,
        },
    )
    .unwrap();
    assert!(!state.is_locked());
    assert!(!state.system_lock_confirmed());
}

#[test]
fn activity_resets_cycle() {
    let plan = vec![
        step(PlanStepKind::Startup, 1, "a"),
        step(PlanStepKind::Dpms, 1, "b"),
    ];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    let _ = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1000 })
        .unwrap();
    assert_eq!(state.step_index(), 1);

    let _ = mgr
        .handle_event(
            &mut state,
            Event::UserActivity {
                kind: ActivityKind::Any,
                now_ms: 1500,
            },
        )
        .unwrap();

    assert_eq!(state.step_index(), 0);
    assert_eq!(state.step_base_ms(), 1500);
}

#[test]
fn notify_then_run_with_delay() {
    let mut s = step(PlanStepKind::Dpms, 5, "doit");
    s.notification = Some("warn".to_string());
    s.notify_seconds_before = Some(3);

    let mut mgr = Manager::new(cfg_with_plan_and_notify(vec![s], 2, true));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 4999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 5000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::Notify {
            message: "warn".to_string(),
            icon: Some("stasis".to_string()),
        }]
    );

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 7999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 8000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "doit".to_string()
        }]
    );
}

#[test]
fn late_tick_runs_notify_then_command_on_later_tick() {
    // With current semantics:
    // - Notify is emitted first (when we first observe we're past base_due).
    // - Run happens notify_seconds_before AFTER the notify emission time.
    //
    // debounce=1s, timeout=4s => base_due=5s
    // notify_seconds_before=2s
    // Late tick at 9000ms:
    // - Notify emitted at 9000ms
    // - Run due at 11000ms (9000 + 2000)

    let mut s = step(PlanStepKind::Startup, 4, "go");
    s.notification = Some("heads up".to_string());
    s.notify_seconds_before = Some(2);

    let mut mgr = Manager::new(cfg_with_plan_and_notify(vec![s], 1, true));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 9000 })
        .unwrap();

    assert_eq!(
        actions,
        vec![Action::Notify {
            message: "heads up".to_string(),
            icon: Some("stasis".to_string()),
        }]
    );

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 10999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 11000 })
        .unwrap();

    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "go".to_string()
        }]
    );
}

#[test]
fn no_notification_text_ignores_notify_seconds_before() {
    let mut s = step(PlanStepKind::Dpms, 5, "doit");
    s.notification = None;
    s.notify_seconds_before = Some(999);

    let mut mgr = Manager::new(cfg_with_plan_and_notify(vec![s], 2, true));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 4999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 5000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "doit".to_string()
        }]
    );
}

#[test]
fn compositor_idled_without_resumed_fires_resume_and_restarts_window() {
    // Regression test for the niri workaround: when CompositorIdled arrives
    // while debounce is already cleared (no CompositorResumed in between),
    // stasis must (a) emit resume commands for any previously-fired step and
    // (b) restart the idle window from the new Idled timestamp.

    let mut s = step(PlanStepKind::Dpms, 5, "dpms_off");
    s.resume_command = Some("dpms_on".to_string());

    let mut mgr = Manager::new(cfg_with_plan(vec![s]));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);

    // First idle window: step fires at t=5000.
    enter_idle(&mut mgr, &mut state, 0);
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 5000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "dpms_off".to_string()
        }]
    );
    // debounce_pending is still false; no CompositorResumed arrives (niri scenario).

    // Second CompositorIdled without any CompositorResumed in between.
    let actions = mgr
        .handle_event(&mut state, Event::CompositorIdled { now_ms: 10_000 })
        .unwrap();

    // (a) Resume command must fire for the previously-fired Dpms step.
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::RunResumeCommand { command } if command == "dpms_on"
        )),
        "expected RunResumeCommand(dpms_on) in {actions:?}"
    );

    // (b) Idle window restarts from the new Idled timestamp.
    assert_eq!(state.step_base_ms(), 10_000);
    assert_eq!(state.step_index(), 0);

    // Step must not fire before new_base + timeout.
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 14_999 })
        .unwrap();
    assert!(actions.is_empty());

    // Step fires exactly at 10_000 + 5_000 = 15_000.
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 15_000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "dpms_off".to_string()
        }]
    );
}

#[test]
fn browser_inactive_uses_a_current_verified_idle_state() {
    let plan = vec![step(PlanStepKind::Dpms, 1, "dpms")];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);

    // The compositor was already genuinely idle before browser policy engaged.
    enter_idle(&mut mgr, &mut state, 0);

    let actions = mgr
        .handle_event(&mut state, Event::BrowserActivity { now_ms: 100 })
        .unwrap();
    assert!(actions.is_empty());
    assert!(state.compositor_idle());

    // While browser inhibit is active, long ticks must not advance or fire steps.
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 60_000 })
        .unwrap();
    assert!(actions.is_empty());

    // Explicit inactive edge starts idle timing from this point.
    let actions = mgr
        .handle_event(&mut state, Event::BrowserInactive { now_ms: 60_000 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 60_999 })
        .unwrap();
    assert!(actions.is_empty());

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 61_000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "dpms".to_string()
        }]
    );
}

#[test]
fn browser_inactive_cannot_start_timing_after_compositor_resumed() {
    let plan = vec![step(PlanStepKind::Dpms, 1, "dpms")];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);

    mgr.handle_event(&mut state, Event::BrowserActivity { now_ms: 0 })
        .unwrap();
    mgr.handle_event(&mut state, Event::CompositorIdled { now_ms: 1_000 })
        .unwrap();

    // An inhibitor-aware Resumed edge invalidates the earlier idle observation.
    mgr.handle_event(&mut state, Event::CompositorResumed { now_ms: 2_000 })
        .unwrap();
    assert!(!state.compositor_idle());

    mgr.handle_event(&mut state, Event::BrowserInactive { now_ms: 60_000 })
        .unwrap();

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 120_000 })
        .unwrap();
    assert!(actions.is_empty());
    assert!(state.debounce_pending());

    // Only a later real idle edge can arm the plan.
    mgr.handle_event(&mut state, Event::CompositorIdled { now_ms: 120_000 })
        .unwrap();
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 121_000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "dpms".to_string()
        }]
    );
}

#[test]
fn lid_open_waits_for_a_fresh_compositor_idle_edge() {
    let plan = vec![step(PlanStepKind::Dpms, 1, "dpms")];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    mgr.handle_event(&mut state, Event::LidClosed { now_ms: 100 })
        .unwrap();
    mgr.handle_event(&mut state, Event::LidOpened { now_ms: 200 })
        .unwrap();

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 60_000 })
        .unwrap();
    assert!(actions.is_empty());
    assert!(state.debounce_pending());
    assert!(!state.compositor_idle());

    mgr.handle_event(&mut state, Event::CompositorIdled { now_ms: 60_000 })
        .unwrap();
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 61_000 })
        .unwrap();
    assert_eq!(actions.len(), 1);
}

#[test]
fn sleep_resume_waits_for_a_fresh_compositor_idle_edge() {
    let plan = vec![step(PlanStepKind::Dpms, 1, "dpms")];

    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    mgr.handle_event(&mut state, Event::PrepareForSleep { now_ms: 100 })
        .unwrap();
    mgr.handle_event(&mut state, Event::ResumedFromSleep { now_ms: 200 })
        .unwrap();

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 60_000 })
        .unwrap();
    assert!(actions.is_empty());
    assert!(state.debounce_pending());
    assert!(!state.compositor_idle());

    mgr.handle_event(&mut state, Event::CompositorIdled { now_ms: 60_000 })
        .unwrap();
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 61_000 })
        .unwrap();
    assert_eq!(actions.len(), 1);
}

#[test]
fn low_power_fires_after_dpms_then_timeout() {
    let plan = vec![
        step(PlanStepKind::Dpms, 1, "dpms"),
        step(PlanStepKind::Suspend, 100, "suspend"),
    ];

    let mut cfg_file = cfg_with_plan(plan);
    cfg_file.default.low_power_when_idle = true;
    cfg_file.default.low_power_when_idle_timeout = 5;

    let mut mgr = Manager::new(cfg_file);
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    // Fire the DPMS step at 1000ms.
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "dpms".to_string()
        }]
    );
    assert!(state.low_power_armed());

    // Low power should NOT fire before the 5s timeout (armed at 1000ms, due 6000ms).
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 5999 })
        .unwrap();
    assert!(actions.is_empty());

    // Fires exactly at the timeout boundary.
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 6000 })
        .unwrap();
    assert_eq!(actions, vec![Action::EnterLowPower]);
    assert!(state.low_power_active());
    assert!(!state.low_power_armed());
}

#[test]
fn low_power_restores_before_resume_command_on_activity() {
    let plan = vec![
        step(PlanStepKind::Dpms, 1, "dpms"),
        step(PlanStepKind::Suspend, 100, "suspend"),
    ];

    let mut cfg_file = cfg_with_plan(plan);
    cfg_file.default.low_power_when_idle = true;
    cfg_file.default.low_power_when_idle_timeout = 5;

    let mut mgr = Manager::new(cfg_file);
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    // Fire DPMS + enter low power.
    let _ = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1000 })
        .unwrap();
    let _ = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 6000 })
        .unwrap();
    assert!(state.low_power_active());

    // Activity must restore hardware FIRST, then any resume commands.
    let actions = mgr
        .handle_event(
            &mut state,
            Event::UserActivity {
                kind: ActivityKind::Any,
                now_ms: 7000,
            },
        )
        .unwrap();

    assert!(!state.low_power_active());
    assert_eq!(actions.first(), Some(&Action::ExitLowPower));
}

#[test]
fn low_power_disabled_never_fires() {
    let plan = vec![step(PlanStepKind::Dpms, 1, "dpms")];

    let cfg_file = cfg_with_plan(plan);

    let mut mgr = Manager::new(cfg_file);
    let mut state = State::new(0);
    state.set_plan_source(PlanSource::Desktop);
    enter_idle(&mut mgr, &mut state, 0);

    let _ = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1000 })
        .unwrap();

    // Even far in the future, no EnterLowPower when disabled.
    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 1_000_000 })
        .unwrap();
    assert!(actions.iter().all(|a| !matches!(a, Action::EnterLowPower)));
}

#[test]
fn suspend_only_app_allows_dpms_then_resumes_remaining_suspend_timeout() {
    let plan = vec![
        step(PlanStepKind::Dpms, 5, "dpms"),
        step(PlanStepKind::Suspend, 10, "suspend"),
    ];
    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);

    mgr.handle_event(
        &mut state,
        Event::AppInhibitorCount {
            count: 0,
            suspend_count: 1,
            now_ms: 1_000,
        },
    )
    .unwrap();
    enter_idle(&mut mgr, &mut state, 0);

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 5_000 })
        .unwrap();
    assert_eq!(
        actions,
        vec![Action::RunCommand {
            command: "dpms".to_string()
        }]
    );
    assert_eq!(state.suspend_hold_started_ms(), Some(5_000));

    let actions = mgr
        .handle_event(&mut state, Event::Tick { now_ms: 20_000 })
        .unwrap();
    assert!(actions.is_empty());

    mgr.handle_event(
        &mut state,
        Event::AppInhibitorCount {
            count: 0,
            suspend_count: 0,
            now_ms: 20_000,
        },
    )
    .unwrap();

    assert!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 29_999 })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 30_000 })
            .unwrap(),
        vec![Action::RunCommand {
            command: "suspend".to_string()
        }]
    );
}

#[test]
fn suspend_hold_preserves_elapsed_time_when_it_starts_mid_timeout() {
    let plan = vec![step(PlanStepKind::Suspend, 10, "suspend")];
    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    enter_idle(&mut mgr, &mut state, 0);

    mgr.handle_event(
        &mut state,
        Event::AppInhibitorCount {
            count: 0,
            suspend_count: 1,
            now_ms: 4_000,
        },
    )
    .unwrap();
    mgr.handle_event(
        &mut state,
        Event::AppInhibitorCount {
            count: 0,
            suspend_count: 0,
            now_ms: 14_000,
        },
    )
    .unwrap();

    assert!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 19_999 })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 20_000 })
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn global_and_suspend_holds_do_not_double_pause_the_timer() {
    let plan = vec![step(PlanStepKind::Suspend, 10, "suspend")];
    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    enter_idle(&mut mgr, &mut state, 0);

    for (now_ms, count, suspend_count) in
        [(2_000, 0, 1), (5_000, 1, 1), (10_000, 0, 1), (14_000, 0, 0)]
    {
        mgr.handle_event(
            &mut state,
            Event::AppInhibitorCount {
                count,
                suspend_count,
                now_ms,
            },
        )
        .unwrap();
    }

    assert!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 21_999 })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 22_000 })
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn low_power_still_fires_while_suspend_is_held() {
    let plan = vec![
        step(PlanStepKind::Dpms, 1, "dpms"),
        step(PlanStepKind::Suspend, 100, "suspend"),
    ];
    let mut cfg_file = cfg_with_plan(plan);
    cfg_file.default.low_power_when_idle = true;
    cfg_file.default.low_power_when_idle_timeout = 2;
    let mut mgr = Manager::new(cfg_file);
    let mut state = State::new(0);

    mgr.handle_event(
        &mut state,
        Event::AppInhibitorCount {
            count: 0,
            suspend_count: 1,
            now_ms: 0,
        },
    )
    .unwrap();
    enter_idle(&mut mgr, &mut state, 0);
    mgr.handle_event(&mut state, Event::Tick { now_ms: 1_000 })
        .unwrap();

    assert_eq!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 3_000 })
            .unwrap(),
        vec![Action::EnterLowPower]
    );
    assert!(!state.paused());
}

#[test]
fn suspend_scoped_media_does_not_reset_the_idle_cycle_when_playback_ends() {
    let plan = vec![step(PlanStepKind::Suspend, 10, "suspend")];
    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    enter_idle(&mut mgr, &mut state, 0);

    mgr.handle_event(
        &mut state,
        Event::MediaInhibitorCount {
            count: 0,
            suspend_count: 1,
            now_ms: 4_000,
        },
    )
    .unwrap();
    mgr.handle_event(
        &mut state,
        Event::MediaInhibitorCount {
            count: 0,
            suspend_count: 0,
            now_ms: 14_000,
        },
    )
    .unwrap();

    assert!(!state.debounce_pending());
    assert!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 19_999 })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 20_000 })
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn full_media_ending_resets_the_idle_cycle_even_if_suspend_media_remains() {
    let plan = vec![step(PlanStepKind::Suspend, 10, "suspend")];
    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    enter_idle(&mut mgr, &mut state, 0);

    mgr.handle_event(
        &mut state,
        Event::MediaInhibitorCount {
            count: 1,
            suspend_count: 1,
            now_ms: 4_000,
        },
    )
    .unwrap();
    assert!(state.paused());

    mgr.handle_event(
        &mut state,
        Event::MediaInhibitorCount {
            count: 0,
            suspend_count: 1,
            now_ms: 14_000,
        },
    )
    .unwrap();

    assert!(!state.paused());
    assert!(state.debounce_pending());
    assert_eq!(state.media_inhibitor_count(), 0);
    assert_eq!(state.suspend_media_inhibitor_count(), 1);
}

#[test]
fn manual_suspend_trigger_bypasses_suspend_inhibitors() {
    let plan = vec![step(PlanStepKind::Suspend, 100, "suspend")];
    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    enter_idle(&mut mgr, &mut state, 0);
    mgr.handle_event(
        &mut state,
        Event::AppInhibitorCount {
            count: 0,
            suspend_count: 1,
            now_ms: 1,
        },
    )
    .unwrap();

    assert_eq!(
        mgr.handle_event(
            &mut state,
            Event::ManualTrigger {
                name: "suspend".to_string(),
                now_ms: 2,
            },
        )
        .unwrap(),
        vec![Action::RunCommand {
            command: "suspend".to_string()
        }]
    );
}

#[test]
fn login1_idle_hold_blocks_only_suspend_and_cleans_up() {
    let plan = vec![
        step(PlanStepKind::Dpms, 1, "dpms"),
        step(PlanStepKind::Suspend, 1, "suspend"),
    ];
    let mut mgr = Manager::new(cfg_with_plan(plan));
    let mut state = State::new(0);
    enter_idle(&mut mgr, &mut state, 0);

    let hold = Login1IdleHold {
        status: "live".to_string(),
        what: "idle".to_string(),
        who: "codex".to_string(),
        why: "Codex is running an active turn".to_string(),
        mode: "block".to_string(),
        uid: 1000,
        pid: 42,
        process: Some("codex".to_string()),
        started_at_ms: 500,
        age_ms: 0,
    };
    mgr.handle_event(
        &mut state,
        Event::Login1IdleInhibitorsChanged {
            holds: vec![hold],
            now_ms: 500,
        },
    )
    .unwrap();

    assert!(!state.paused());
    assert_eq!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 1_000 })
            .unwrap(),
        vec![Action::RunCommand {
            command: "dpms".to_string()
        }]
    );
    assert!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 2_000 })
            .unwrap()
            .is_empty()
    );

    let snapshot = mgr.snapshot(&state, 4_000);
    assert_eq!(snapshot.waybar.login1_idle_inhibitors.len(), 1);
    assert!(
        snapshot
            .pretty_text
            .contains("login1 Idle Inhibitors Blocking Suspend: 1")
    );
    assert!(snapshot.pretty_text.contains("D-Bus Inhibiting: yes"));
    let blame = mgr.blame_snapshot(&state, 4_000);
    assert_eq!(blame.login1_idle_holds[0].who, "codex");
    assert_eq!(blame.login1_idle_holds[0].age_ms, 3_500);

    mgr.handle_event(
        &mut state,
        Event::Login1IdleInhibitorsChanged {
            holds: Vec::new(),
            now_ms: 5_000,
        },
    )
    .unwrap();
    assert!(state.login1_idle_holds().is_empty());
    assert!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 5_999 })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mgr.handle_event(&mut state, Event::Tick { now_ms: 6_000 })
            .unwrap(),
        vec![Action::RunCommand {
            command: "suspend".to_string()
        }]
    );
}

#[test]
fn info_reports_suspend_only_configuration_and_live_hold() {
    let plan = vec![step(PlanStepKind::Suspend, 100, "suspend")];
    let mut cfg_file = cfg_with_plan(plan);
    cfg_file.default.suspend_inhibit_media = vec![Pattern::Literal("spotify".to_string())];
    let mut mgr = Manager::new(cfg_file);
    let mut state = State::new(0);
    enter_idle(&mut mgr, &mut state, 0);
    mgr.handle_event(
        &mut state,
        Event::MediaInhibitorCount {
            count: 0,
            suspend_count: 1,
            now_ms: 1_000,
        },
    )
    .unwrap();

    let snapshot = mgr.snapshot(&state, 4_000);
    assert!(snapshot.pretty_text.contains("State: active"));
    assert!(
        snapshot
            .pretty_text
            .contains("Media Players Blocking Suspend: 1")
    );
    assert!(
        snapshot
            .pretty_text
            .contains("Next: Suspend inhibited for 3s")
    );
    assert!(
        snapshot
            .pretty_text
            .contains("SuspendInhibitMedia: spotify")
    );

    let watch = mgr.watch_event(&state);
    assert_eq!(watch.state, "active");
    assert!(!watch.paused);
}

#[test]
fn profile_change_clears_stale_counts_and_reclassifies_media() {
    let mut cfg_file = cfg_with_plan(vec![step(PlanStepKind::Suspend, 100, "suspend")]);
    cfg_file.profiles.push(Profile {
        name: "music".to_string(),
        mode: ProfileMode::Overlay,
        config: PartialConfig {
            suspend_inhibit_media: Some(vec![Pattern::Literal("spotify".to_string())]),
            ..PartialConfig::default()
        },
    });

    let mut mgr = Manager::new(cfg_file);
    let mut state = State::new(0);
    enter_idle(&mut mgr, &mut state, 0);
    mgr.handle_event(
        &mut state,
        Event::MediaInhibitorCount {
            count: 1,
            suspend_count: 0,
            now_ms: 1_000,
        },
    )
    .unwrap();
    assert!(state.paused());
    assert_eq!(state.media_inhibitor_count(), 1);

    mgr.handle_event(
        &mut state,
        Event::ProfileChanged {
            name: "music".to_string(),
            now_ms: 2_000,
        },
    )
    .unwrap();
    assert!(!state.paused());
    assert_eq!(state.media_inhibitor_count(), 0);
    assert_eq!(state.suspend_media_inhibitor_count(), 0);

    enter_idle(&mut mgr, &mut state, 3_000);
    mgr.handle_event(
        &mut state,
        Event::MediaInhibitorCount {
            count: 0,
            suspend_count: 1,
            now_ms: 4_000,
        },
    )
    .unwrap();
    assert!(!state.paused());
    assert_eq!(state.media_inhibitor_count(), 0);
    assert_eq!(state.suspend_media_inhibitor_count(), 1);
}
