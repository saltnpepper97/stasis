// Author: Dustin Pilgrim
// License: GPL-3.0-only

use crate::core::action::Action;
use crate::core::config::{Config, ConfigFile, PlanSource, PlanStep, PlanStepKind};
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
