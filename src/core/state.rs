// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::collections::HashSet;

use crate::core::config::{PlanSource, PlanStep, PlanStepKind};
use crate::core::events::PowerState;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OneShotKey {
    kind: String,
    command: String,
}

impl OneShotKey {
    pub fn from_step(step: &PlanStep) -> Option<Self> {
        let cmd = step.command.as_ref()?.clone();

        let kind = match &step.kind {
            PlanStepKind::Custom(s) => format!("custom:{s}"),
            other => format!("{other:?}"),
        };

        Some(Self { kind, command: cmd })
    }
}

#[derive(Debug, Clone)]
pub struct State {
    // Inhibitors (counts provided by services)
    app_inhibitor_count: u64,
    media_inhibitor_count: u64,

    // Pause policy
    manually_paused: bool,

    // System pause (lid closed / preparing for sleep, etc.)
    system_paused: bool,

    // Derived pause (manual OR inhibitors OR system)
    paused: bool,
    pause_started_ms: Option<u64>,

    // Mirrors config; manager copies from effective config each event.
    debounce_seconds: u64,

    // Debounce latch (not tied to step_index anymore)
    // True means: apply debounce ONCE before the next non-instant step fires.
    debounce_pending: bool,

    // Session / profile
    is_locked: bool,
    // A positive login1 LockedHint promotes the current lock episode away
    // from process-lifetime fallback until LockedHint reports false.
    system_lock_confirmed: bool,
    // None means "default" (no profile overlay)
    active_profile: Option<String>,

    // Power/plan selection
    power_state: Option<PowerState>,
    plan_source: PlanSource,

    // Timing (ms since epoch, supplied by Tick/UserActivity/etc.)
    last_activity_ms: u64,
    last_action_ms: u64,
    browser_activity_until_ms: u64,

    // Sequential plan machine:
    step_index: usize,
    step_base_ms: u64,

    // One-shot notification (per step / per idle cycle)
    sent_pre_action_notify: bool,
    pre_action_notify_ms: u64,

    // Fired tracking (per idle cycle)
    fired_steps: Vec<bool>,
    last_fired_idx: Option<usize>,

    // Group last-fired (per idle cycle) so we can dedupe resume:
    last_dpms_fired_idx: Option<usize>,
    last_brightness_fired_idx: Option<usize>,

    // Track lock step separately so its resume-command can fire after unlock
    last_lock_fired_idx: Option<usize>,

    // Resume episode latch:
    resume_epoch: u64,
    resumed_epoch: u64,

    // If we resumed while locked (dpms/brightness), defer the remainder until unlock.
    resume_deferred_until_unlock: bool,

    // Low-power (hardware power-down) mode tracking.
    low_power_armed: bool,
    low_power_armed_ms: u64,
    low_power_active: bool,

    // Lifetime one-shots (instant steps with timeout=0)
    one_shots_fired: HashSet<OneShotKey>,
}

impl State {
    pub fn new(now_ms: u64) -> Self {
        Self {
            app_inhibitor_count: 0,
            media_inhibitor_count: 0,
            manually_paused: false,
            system_paused: false,
            paused: false,
            pause_started_ms: None,

            debounce_seconds: 0,
            debounce_pending: true, // boot behaves like "fresh idle cycle"

            is_locked: false,
            system_lock_confirmed: false,
            // Default profile is represented by None (profile selection is IPC-only).
            active_profile: None,

            power_state: None,
            plan_source: PlanSource::Desktop,

            last_activity_ms: now_ms,
            last_action_ms: now_ms,
            browser_activity_until_ms: 0,

            step_index: 0,
            step_base_ms: now_ms,

            sent_pre_action_notify: false,
            pre_action_notify_ms: 0,

            fired_steps: Vec::new(),
            last_fired_idx: None,
            last_dpms_fired_idx: None,
            last_brightness_fired_idx: None,
            last_lock_fired_idx: None,

            resume_epoch: 0,
            resumed_epoch: 0,
            resume_deferred_until_unlock: false,

            low_power_armed: false,
            low_power_armed_ms: 0,
            low_power_active: false,

            one_shots_fired: HashSet::new(),
        }
    }

    // ---------------- sizing / tracking ----------------

    pub fn ensure_plan_len(&mut self, len: usize) {
        if self.fired_steps.len() != len {
            self.fired_steps = vec![false; len];
            self.last_fired_idx = None;
            self.last_dpms_fired_idx = None;
            self.last_brightness_fired_idx = None;
            self.last_lock_fired_idx = None;
        }
    }

    pub fn mark_step_fired(
        &mut self,
        idx: usize,
        is_dpms: bool,
        is_brightness: bool,
        is_lock: bool,
        arms_resume: bool,
    ) {
        if idx >= self.fired_steps.len() {
            self.ensure_plan_len(idx + 1);
        }

        self.fired_steps[idx] = true;
        self.last_fired_idx = Some(idx);

        if is_dpms {
            self.last_dpms_fired_idx = Some(idx);
        }
        if is_brightness {
            self.last_brightness_fired_idx = Some(idx);
        }
        if is_lock {
            self.last_lock_fired_idx = Some(idx);
        }

        // Anything that wants a resume episode should arm it.
        // Lock is included so lock resume-command can fire after unlock.
        if arms_resume || is_dpms || is_brightness || is_lock {
            self.arm_resume_episode();
        }
    }

    /// Public so manager can re-arm a resume episode on unlock if it deferred.
    pub fn arm_resume_episode(&mut self) {
        self.resume_epoch = self.resume_epoch.wrapping_add(1);
    }

    pub fn resume_due(&self) -> bool {
        self.resumed_epoch != self.resume_epoch
    }

    pub fn mark_resumed(&mut self) {
        self.resumed_epoch = self.resume_epoch;
    }

    pub fn set_resume_deferred_until_unlock(&mut self, v: bool) {
        self.resume_deferred_until_unlock = v;
    }

    /// Take-and-clear helper.
    pub fn take_resume_deferred_until_unlock(&mut self) -> bool {
        let v = self.resume_deferred_until_unlock;
        self.resume_deferred_until_unlock = false;
        v
    }

    pub fn last_fired_idx(&self) -> Option<usize> {
        self.last_fired_idx
    }

    pub fn last_dpms_fired_idx(&self) -> Option<usize> {
        self.last_dpms_fired_idx
    }

    pub fn last_brightness_fired_idx(&self) -> Option<usize> {
        self.last_brightness_fired_idx
    }

    pub fn last_lock_fired_idx(&self) -> Option<usize> {
        self.last_lock_fired_idx
    }

    pub fn clear_fired_steps(&mut self) {
        for b in &mut self.fired_steps {
            *b = false;
        }
        self.last_fired_idx = None;
        self.last_dpms_fired_idx = None;
        self.last_brightness_fired_idx = None;
        self.last_lock_fired_idx = None;
    }

    /// Clear fired flags from a point onward (post-lock "segment restart").
    pub fn clear_fired_steps_from(&mut self, start_idx: usize) {
        if start_idx >= self.fired_steps.len() {
            return;
        }
        for i in start_idx..self.fired_steps.len() {
            self.fired_steps[i] = false;
        }

        // If our last_* pointers point into the cleared region, drop them.
        if self.last_fired_idx.is_some_and(|i| i >= start_idx) {
            self.last_fired_idx = None;
        }
        if self.last_dpms_fired_idx.is_some_and(|i| i >= start_idx) {
            self.last_dpms_fired_idx = None;
        }
        if self
            .last_brightness_fired_idx
            .is_some_and(|i| i >= start_idx)
        {
            self.last_brightness_fired_idx = None;
        }
        if self.last_lock_fired_idx.is_some_and(|i| i >= start_idx) {
            self.last_lock_fired_idx = None;
        }
    }

    // ---------------- lifetime one-shots ----------------

    pub fn one_shot_has_fired_step(&self, step: &PlanStep) -> bool {
        match OneShotKey::from_step(step) {
            Some(k) => self.one_shots_fired.contains(&k),
            None => false,
        }
    }

    pub fn mark_one_shot_fired_step(&mut self, step: &PlanStep) {
        if let Some(k) = OneShotKey::from_step(step) {
            self.one_shots_fired.insert(k);
        }
    }

    pub fn clear_one_shots(&mut self) {
        self.one_shots_fired.clear();
        // Return the backing allocation to the OS. clear() keeps capacity, so
        // without this a startup burst of instant steps would hold the allocation
        // indefinitely across profile/power transitions that call clear_one_shots.
        self.one_shots_fired.shrink_to(0);
    }

    // ---------------- pause timestamp helpers ----------------

    /// Set/clear the pause-start timestamp.
    pub fn set_pause_started_ms(&mut self, v: Option<u64>) {
        self.pause_started_ms = v;
    }

    /// Take-and-clear the pause-start timestamp.
    pub fn take_pause_started_ms(&mut self) -> Option<u64> {
        self.pause_started_ms.take()
    }

    // ---------------- getters ----------------

    pub fn app_inhibitor_count(&self) -> u64 {
        self.app_inhibitor_count
    }

    pub fn media_inhibitor_count(&self) -> u64 {
        self.media_inhibitor_count
    }

    pub fn manually_paused(&self) -> bool {
        self.manually_paused
    }

    pub fn system_paused(&self) -> bool {
        self.system_paused
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn is_locked(&self) -> bool {
        self.is_locked
    }

    pub fn system_lock_confirmed(&self) -> bool {
        self.system_lock_confirmed
    }

    pub fn active_profile(&self) -> Option<&str> {
        self.active_profile.as_deref()
    }

    pub fn plan_source(&self) -> PlanSource {
        self.plan_source
    }

    pub fn inhibitors_active(&self) -> bool {
        self.app_inhibitor_count > 0 || self.media_inhibitor_count > 0
    }

    pub fn step_index(&self) -> usize {
        self.step_index
    }

    pub fn step_base_ms(&self) -> u64 {
        self.step_base_ms
    }

    pub fn pre_action_notify_sent(&self) -> bool {
        self.sent_pre_action_notify
    }

    pub fn pre_action_notify_ms(&self) -> u64 {
        self.pre_action_notify_ms
    }

    pub fn debounce_pending(&self) -> bool {
        self.debounce_pending
    }

    pub fn pause_started_ms(&self) -> Option<u64> {
        self.pause_started_ms
    }

    // ---------------- setters ----------------

    pub fn set_app_inhibitor_count(&mut self, count: u64) {
        self.app_inhibitor_count = count;
    }

    pub fn set_media_inhibitor_count(&mut self, count: u64) {
        self.media_inhibitor_count = count;
    }

    pub fn set_manually_paused(&mut self, v: bool) {
        self.manually_paused = v;
    }

    pub fn set_system_paused(&mut self, v: bool) {
        self.system_paused = v;
    }

    pub fn set_paused(&mut self, v: bool) {
        self.paused = v;
        if !v {
            // If we are not paused, we should not keep a pause-start timestamp around.
            self.pause_started_ms = None;
        }
    }

    pub fn set_locked(&mut self, v: bool) {
        self.is_locked = v;
    }

    pub fn set_system_lock_confirmed(&mut self, v: bool) {
        self.system_lock_confirmed = v;
    }

    pub fn set_active_profile(&mut self, name: Option<String>) {
        self.active_profile = name;
    }

    pub fn set_power_state(&mut self, ps: PowerState) {
        self.power_state = Some(ps);
    }

    pub fn set_plan_source(&mut self, src: PlanSource) {
        self.plan_source = src;
    }

    pub fn set_debounce_seconds(&mut self, secs: u64) {
        self.debounce_seconds = secs;
    }

    pub fn mark_action_fired(&mut self, now_ms: u64) {
        self.last_action_ms = now_ms;
    }

    pub fn note_browser_activity(&mut self, now_ms: u64, hold_ms: u64) {
        self.browser_activity_until_ms = now_ms.saturating_add(hold_ms);
    }

    pub fn clear_browser_activity(&mut self) {
        self.browser_activity_until_ms = 0;
    }

    pub fn browser_activity_active(&self, now_ms: u64) -> bool {
        now_ms < self.browser_activity_until_ms
    }

    pub fn set_step_index(&mut self, v: usize) {
        self.step_index = v;
    }

    pub fn set_step_base_ms(&mut self, v: u64) {
        self.step_base_ms = v;
    }

    pub fn set_pre_action_notify_sent(&mut self, v: bool) {
        self.sent_pre_action_notify = v;
        if !v {
            self.pre_action_notify_ms = 0;
        }
    }

    pub fn set_pre_action_notify_ms(&mut self, t: u64) {
        self.pre_action_notify_ms = t;
    }

    pub fn set_debounce_pending(&mut self, v: bool) {
        self.debounce_pending = v;
    }

    // ---------------- low-power mode ----------------

    pub fn low_power_armed(&self) -> bool {
        self.low_power_armed
    }

    pub fn low_power_armed_ms(&self) -> u64 {
        self.low_power_armed_ms
    }

    pub fn low_power_active(&self) -> bool {
        self.low_power_active
    }

    pub fn set_low_power_armed(&mut self, v: bool) {
        self.low_power_armed = v;
    }

    pub fn set_low_power_armed_ms(&mut self, t: u64) {
        self.low_power_armed_ms = t;
    }

    pub fn set_low_power_active(&mut self, v: bool) {
        self.low_power_active = v;
    }

    /// Disarm the low-power timer (does not deactivate active hardware mode;
    /// that is restored via the ExitLowPower action).
    pub fn disarm_low_power(&mut self) {
        self.low_power_armed = false;
    }

    // ---------------- cycle control ----------------

    /// Full idle cycle reset (unlocked activity, profile/power transitions, etc.)
    pub fn reset_idle_cycle(&mut self, now_ms: u64) {
        self.last_activity_ms = now_ms;

        self.step_index = 0;
        self.step_base_ms = now_ms;

        self.sent_pre_action_notify = false;
        self.pre_action_notify_ms = 0;

        self.debounce_pending = true;

        self.clear_fired_steps();

        // Reset pause timestamp for a fresh cycle.
        self.pause_started_ms = None;

        // Disarm the low-power timer for a fresh cycle. Hardware restore (if
        // active) is handled by the manager emitting ExitLowPower before reset.
        self.low_power_armed = false;
    }

    /// Restart timers AND rewind to the post-lock start step so the post-lock
    /// segment (dpms/suspend/etc.) can run again while still locked.
    pub fn restart_post_lock_segment(&mut self, now_ms: u64, post_lock_start_idx: usize) {
        self.last_activity_ms = now_ms;

        self.step_index = post_lock_start_idx;
        self.step_base_ms = now_ms;

        self.sent_pre_action_notify = false;
        self.pre_action_notify_ms = 0;

        self.debounce_pending = true;

        self.clear_fired_steps_from(post_lock_start_idx);

        // Reset pause timestamp for a restarted segment.
        self.pause_started_ms = None;

        // Disarm the low-power timer when restarting the post-lock segment.
        self.low_power_armed = false;
    }
}

impl Default for State {
    fn default() -> Self {
        State::new(0)
    }
}
