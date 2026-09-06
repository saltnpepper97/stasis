// Author: Dustin Pilgrim
// License: GPL-3.0-only

use crate::core::{
    action::Action,
    config::{Config, PlanSource, PlanStep, PlanStepKind},
    error::{ConfigError, Error, StateError},
    events::{Event, LockSource, PowerState},
    state::State,
};

use super::Manager;

impl Manager {
    // Browser-originated inhibit is authoritative and must remain active until
    // an explicit inactive edge (or sender disconnect) clears it.
    const BROWSER_ACTIVITY_HOLD_MS: u64 = u64::MAX;

    pub fn handle_event(&mut self, state: &mut State, event: Event) -> Result<Vec<Action>, Error> {
        let now_ms = event.now_ms();
        let cfg = self.effective_cfg(state)?;

        state.ensure_plan_len(cfg.plan.len());
        state.set_debounce_seconds(cfg.debounce_seconds);

        self.refresh_timing_holds(state, &cfg, now_ms);

        let mut out = Vec::new();

        out.extend(self.maybe_fire_startup_instants(state, &cfg, now_ms));
        self.sync_step_index_after_startup_instants(state, &cfg);

        match event {
            Event::Tick { .. } => {
                // If an inhibitor/system pause appeared while we were in
                // low-power mode, restore hardware immediately so the system
                // is ready for whatever is keeping it awake.
                if state.paused() {
                    if state.low_power_active() {
                        out.push(Action::ExitLowPower);
                        state.set_low_power_active(false);
                    }
                    state.disarm_low_power();
                    return Ok(out);
                }

                // Browser keepalive/activity pulses keep us in waiting-for-idle.
                if state.browser_activity_active(now_ms) {
                    return Ok(out);
                }

                // While waiting for a real idle edge, do not schedule notifications
                // or run plan steps.
                if state.debounce_pending() {
                    return Ok(out);
                }

                self.advance_past_lock_if_needed(state, &cfg);
                out.extend(self.maybe_fire_next_step(state, &cfg, now_ms));
                self.refresh_timing_holds(state, &cfg, now_ms);

                // Low-power: fire after the DPMS step has run and the configured
                // timeout has elapsed since DPMS fired.
                self.maybe_fire_low_power(state, &cfg, now_ms, &mut out);
            }

            Event::UserActivity { .. } => {
                self.handle_activity_like_event(state, &cfg, now_ms, &mut out);
            }

            Event::BrowserActivity { .. } => {
                // Browser policy is authoritative for gating, but it is not proof
                // of physical input. Preserve a current compositor observation so
                // BrowserInactive can safely resume an already-idle session.
                let compositor_idle = state.compositor_idle();
                state.note_browser_activity(now_ms, Self::BROWSER_ACTIVITY_HOLD_MS);
                self.handle_activity_like_event(state, &cfg, now_ms, &mut out);
                state.set_compositor_idle(compositor_idle);
            }

            Event::BrowserInactive { .. } => {
                state.clear_browser_activity();

                // Browser activity may have masked a real compositor-idled edge.
                // Resume promptly only when that verified idle state is still current.
                self.begin_idle_from_verified_observation(state, &cfg, now_ms);
            }

            Event::ManualPause { .. } => {
                if state.manually_paused() {
                    return Err(Error::InvalidState(StateError::AlreadyPaused));
                }
                state.set_manually_paused(true);
                self.refresh_timing_holds(state, &cfg, now_ms);
                eventline::info!("manual pause started");
            }

            Event::ManualResume { .. } => {
                if !state.manually_paused() {
                    return Err(Error::InvalidState(StateError::NotPaused));
                }
                self.release_manual_pause(state, &cfg, now_ms, &mut out);
            }

            // notify_on_unpause should ONLY fire for auto-resume from
            // `stasis pause for/until` when the pause expires internally.
            Event::PauseExpired { message, .. } => {
                if state.manually_paused() {
                    self.release_manual_pause(state, &cfg, now_ms, &mut out);
                    eventline::info!("timed pause expired; manual pause released");

                    if cfg.notify_on_unpause {
                        out.push(Action::Notify {
                            message,
                            icon: cfg.notification_icon.clone(),
                        });
                    }
                }
            }

            Event::ManualTrigger { name, .. } => {
                let n = Self::normalize_trigger_name(&name);

                if n == "all" {
                    eventline::info!("trigger: all");

                    let mut emitted_any = false;

                    for (idx, step) in cfg.plan.iter().enumerate() {
                        if !step.enabled() {
                            continue;
                        }
                        if step.is_instant() {
                            continue;
                        }
                        if Self::is_lock_step(step) && state.is_locked() {
                            continue;
                        }

                        let emitted = self.actions_for_plan_step(state, step, &cfg);
                        if !emitted.is_empty() {
                            let arms_resume = step.resume_command.is_some();
                            let is_dpms = Self::is_dpms_group(step);
                            let is_brightness = Self::is_brightness_group(step);
                            let is_lock = Self::is_lock_step(step);

                            state.mark_step_fired(
                                idx,
                                is_dpms,
                                is_brightness,
                                is_lock,
                                arms_resume,
                            );
                            emitted_any = true;
                        }

                        out.extend(emitted);
                    }

                    if emitted_any {
                        state.mark_action_fired(now_ms);
                        state.set_pre_action_notify_sent(false);
                        state.set_debounce_pending(false);
                        state.set_step_base_ms(now_ms);
                        state.set_step_index(cfg.plan.len());
                        state.set_suspend_hold_started_ms(None);
                    }

                    return Ok(out);
                }

                if let Some((idx, step)) = self.find_trigger_step(&cfg, &name) {
                    eventline::info!("trigger: {} -> step_idx={}", name, idx);

                    let emitted = self.actions_for_plan_step(state, step, &cfg);
                    if !emitted.is_empty() {
                        let arms_resume = step.resume_command.is_some();
                        let is_dpms = Self::is_dpms_group(step);
                        let is_brightness = Self::is_brightness_group(step);
                        let is_lock = Self::is_lock_step(step);

                        state.mark_step_fired(idx, is_dpms, is_brightness, is_lock, arms_resume);
                        state.mark_action_fired(now_ms);

                        state.set_step_index(idx + 1);
                        state.set_step_base_ms(now_ms);
                        state.set_debounce_pending(false);
                        state.set_pre_action_notify_sent(false);
                        state.set_suspend_hold_started_ms(None);
                    }

                    out.extend(emitted);
                } else {
                    eventline::warn!("trigger: '{}' not found/enabled in effective config", name);
                }
            }

            Event::SessionLocked { source, .. } => {
                if source == LockSource::LockedHint {
                    if !state.system_lock_confirmed() {
                        eventline::debug!(
                            "lock: login1 LockedHint confirmed; using system lock state"
                        );
                    }
                    state.set_system_lock_confirmed(true);
                }

                if !state.is_locked() {
                    state.set_locked(true);
                    self.advance_past_lock_if_needed(state, &cfg);
                }
            }

            Event::SessionUnlocked { source, .. } => {
                let should_unlock = match source {
                    LockSource::LockerProcess if state.system_lock_confirmed() => {
                        eventline::debug!(
                            "lock: ignoring locker process exit while LockedHint remains true"
                        );
                        false
                    }
                    LockSource::LockedHint if !state.system_lock_confirmed() => {
                        eventline::debug!(
                            "lock: ignoring LockedHint=false without a confirmed hint-tracked episode"
                        );
                        false
                    }
                    LockSource::LockedHint => {
                        state.set_system_lock_confirmed(false);
                        true
                    }
                    LockSource::LockerProcess => true,
                };

                if should_unlock && state.is_locked() {
                    state.set_locked(false);

                    if state.take_resume_deferred_until_unlock() {
                        state.arm_resume_episode();
                    }

                    self.handle_activity_like_event(state, &cfg, now_ms, &mut out);
                }
            }

            Event::PrepareForSleep { .. } => {
                state.set_system_paused(true);
                self.refresh_timing_holds(state, &cfg, now_ms);

                if let Some(cmd) = &cfg.prepare_sleep_command {
                    let c = cmd.trim().to_string();
                    if !c.is_empty() {
                        out.push(Action::RunCommand { command: c });
                    }
                }
            }

            Event::ResumedFromSleep { .. } => {
                state.set_system_paused(false);

                self.handle_activity_like_event(state, &cfg, now_ms, &mut out);
            }

            Event::LidClosed { .. } => {
                // Lid close pauses the plan timers.
                state.set_system_paused(true);
                self.refresh_timing_holds(state, &cfg, now_ms);

                // Run configured lid-close command (if any).
                if let Some(cmd) = &cfg.lid_close_action {
                    let c = cmd.trim().to_string();
                    if !c.is_empty() {
                        out.push(Action::RunCommand { command: c });
                    }
                }
            }

            Event::LidOpened { .. } => {
                // Lid open resumes timers.
                state.set_system_paused(false);

                // Run configured lid-open command (if any) before treating as activity.
                if let Some(cmd) = &cfg.lid_open_action {
                    let c = cmd.trim().to_string();
                    if !c.is_empty() {
                        out.push(Action::RunCommand { command: c });
                    }
                }

                self.handle_activity_like_event(state, &cfg, now_ms, &mut out);
            }

            Event::ProfileChanged { name, .. } => {
                let raw = name.trim();

                // IPC-only profile selection.
                // "default" and "none" both mean: no profile overlay (use default block only).
                let candidate: Option<String> = if raw.is_empty() {
                    return Err(Error::InvalidConfig(ConfigError::InvalidProfileName));
                } else if raw.eq_ignore_ascii_case("none") || raw.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(raw.to_string())
                };

                // Validate selection against loaded config.
                if self
                    .cfg_file
                    .effective_for(candidate.as_deref(), state.plan_source())
                    .is_none()
                {
                    return Err(Error::InvalidConfig(ConfigError::ProfileNotFound));
                }

                state.set_active_profile(candidate);

                state.set_app_inhibitor_count(0);
                state.set_media_inhibitor_count(0);
                state.set_suspend_app_inhibitor_count(0);
                state.set_suspend_media_inhibitor_count(0);
                state.set_app_inhibitor_sources(Vec::new());
                state.set_media_inhibitor_sources(Vec::new());
                state.set_suspend_app_inhibitor_sources(Vec::new());
                state.set_suspend_media_inhibitor_sources(Vec::new());
                self.refresh_timing_holds(state, &cfg, now_ms);

                self.restore_low_power_if_active(state, &mut out);
                state.reset_idle_cycle(now_ms);
                state.clear_one_shots();

                let cfg = self.effective_cfg(state)?;
                state.ensure_plan_len(cfg.plan.len());
                state.set_debounce_seconds(cfg.debounce_seconds);

                self.refresh_timing_holds(state, &cfg, now_ms);
                self.sync_step_index_after_startup_instants(state, &cfg);
                self.advance_past_lock_if_needed(state, &cfg);

                out.extend(self.maybe_fire_startup_instants(state, &cfg, now_ms));
                self.sync_step_index_after_startup_instants(state, &cfg);
            }

            Event::PowerChanged { state: ps, .. } => {
                state.set_power_state(ps);

                let src = match ps {
                    PowerState::OnAC => PlanSource::Ac,
                    PowerState::OnBattery => PlanSource::Battery,
                };
                state.set_plan_source(src);

                self.restore_low_power_if_active(state, &mut out);
                state.reset_idle_cycle(now_ms);
                state.clear_one_shots();

                let cfg = self.effective_cfg(state)?;
                state.ensure_plan_len(cfg.plan.len());
                state.set_debounce_seconds(cfg.debounce_seconds);

                self.refresh_timing_holds(state, &cfg, now_ms);
                self.sync_step_index_after_startup_instants(state, &cfg);
                self.advance_past_lock_if_needed(state, &cfg);

                out.extend(self.maybe_fire_startup_instants(state, &cfg, now_ms));
                self.sync_step_index_after_startup_instants(state, &cfg);
            }

            Event::AppInhibitorCount {
                count,
                suspend_count,
                ..
            } => {
                state.set_app_inhibitor_count(count);
                state.set_suspend_app_inhibitor_count(suspend_count);
                self.refresh_timing_holds(state, &cfg, now_ms);
            }

            Event::MediaInhibitorCount {
                count,
                suspend_count,
                ..
            } => {
                let was_paused = state.paused();
                let full_media_ended = state.media_inhibitor_count() > 0 && count == 0;

                state.set_media_inhibitor_count(count);
                state.set_suspend_media_inhibitor_count(suspend_count);
                self.refresh_timing_holds(state, &cfg, now_ms);

                if full_media_ended {
                    self.handle_activity_like_event(state, &cfg, now_ms, &mut out);

                    // Keep the log, but do not notify here.
                    // notify_on_unpause is reserved for PauseExpired auto-resume only.
                    if was_paused && !state.paused() {
                        eventline::info!("media ended");
                    }
                }
            }

            Event::AppInhibitorSources {
                sources,
                suspend_sources,
                ..
            } => {
                state.set_app_inhibitor_sources(sources);
                state.set_suspend_app_inhibitor_sources(suspend_sources);
            }

            Event::MediaInhibitorSources {
                sources,
                suspend_sources,
                ..
            } => {
                state.set_media_inhibitor_sources(sources);
                state.set_suspend_media_inhibitor_sources(suspend_sources);
            }

            Event::DbusInhibitorsChanged { holds, .. } => {
                state.set_dbus_holds(holds);
            }

            Event::Login1IdleInhibitorsChanged { holds, .. } => {
                state.set_login1_idle_holds(holds);
                self.refresh_timing_holds(state, &cfg, now_ms);
            }

            Event::BrowserSourceCaptureChanged { active, .. } => {
                state.set_browser_source_capture_active(active);
            }

            Event::CompositorResumed { .. } => {
                // Treat exactly like activity.
                self.handle_activity_like_event(state, &cfg, now_ms, &mut out);
            }

            Event::CompositorIdled { .. } => {
                // Record the real inhibitor-aware compositor state even while
                // another Stasis policy temporarily prevents plan timing.
                state.set_compositor_idle(true);

                // If browser reports active playback/usage, extension state wins.
                if state.browser_activity_active(now_ms) {
                    return Ok(out);
                }

                if !state.paused() {
                    // If debounce is already cleared, this Idled edge arrived without a
                    // prior CompositorResumed. Some compositors (e.g. niri) do not send
                    // CompositorResumed reliably. Treat the missing Resumed as implicit
                    // activity so resume commands fire and the cycle resets before timing
                    // the new idle window.
                    if !state.debounce_pending() {
                        self.handle_activity_like_event(state, &cfg, now_ms, &mut out);
                        // The current event is still authoritative after the
                        // implicit activity reset above.
                        state.set_compositor_idle(true);
                    }

                    Self::begin_idle_timing(state, now_ms);
                    self.refresh_timing_holds(state, &cfg, now_ms);
                }
            }
        }

        Ok(out)
    }

    fn begin_idle_timing(state: &mut State, now_ms: u64) {
        state.set_debounce_pending(false);
        state.set_step_base_ms(now_ms);
        state.set_pre_action_notify_sent(false);
        state.set_pre_action_notify_ms(0);
    }

    fn release_manual_pause(
        &mut self,
        state: &mut State,
        cfg: &Config,
        now_ms: u64,
        out: &mut Vec<Action>,
    ) {
        // A pause is not physical input. Preserve the compositor's current
        // verified observation across the activity-like reset used for resume
        // commands, then reuse it only if no other blocker remains.
        let compositor_idle = state.compositor_idle();
        state.set_manually_paused(false);
        self.handle_activity_like_event(state, cfg, now_ms, out);
        state.set_compositor_idle(compositor_idle);
        self.begin_idle_from_verified_observation(state, cfg, now_ms);
    }

    fn begin_idle_from_verified_observation(&self, state: &mut State, cfg: &Config, now_ms: u64) {
        if state.compositor_idle()
            && state.debounce_pending()
            && !state.paused()
            && !state.browser_activity_active(now_ms)
        {
            Self::begin_idle_timing(state, now_ms);
            self.refresh_timing_holds(state, cfg, now_ms);
        }
    }

    fn handle_activity_like_event(
        &mut self,
        state: &mut State,
        cfg: &Config,
        now_ms: u64,
        out: &mut Vec<Action>,
    ) {
        // Restore hardware power state BEFORE running resume commands so the
        // GPU/boards are back up by the time the display comes on.
        self.restore_low_power_if_active(state, out);

        out.extend(self.resume_commands_for_activity(state, cfg));

        if state.is_locked() {
            self.advance_past_lock_if_needed(state, cfg);

            let post_lock_start = self.first_enabled_step_after_lock(cfg);
            state.restart_post_lock_segment(now_ms, post_lock_start);

            self.refresh_timing_holds(state, cfg, now_ms);

            return;
        }

        state.reset_idle_cycle(now_ms);
        self.refresh_timing_holds(state, cfg, now_ms);

        self.sync_step_index_after_startup_instants(state, cfg);

        self.advance_past_lock_if_needed(state, cfg);
    }

    fn effective_cfg(&self, state: &State) -> Result<Config, Error> {
        self.cfg_file
            .effective_for(state.active_profile(), state.plan_source())
            .ok_or(Error::InvalidConfig(ConfigError::ProfileNotFound))
    }

    /// Emit ExitLowPower (and clear tracking) if hardware low-power mode is
    /// currently active. Called on every activity/resume path so hardware is
    /// always restored from its snapshot before anything else happens.
    fn restore_low_power_if_active(&self, state: &mut State, out: &mut Vec<Action>) {
        if state.low_power_active() {
            out.push(Action::ExitLowPower);
            state.set_low_power_active(false);
        }
        state.disarm_low_power();
    }

    /// Arm the low-power timer right after the DPMS step has fired.
    fn arm_low_power_after_dpms(&self, state: &mut State, cfg: &Config, now_ms: u64) {
        if cfg.low_power_when_idle && cfg.low_power_when_idle_timeout > 0 {
            state.set_low_power_armed(true);
            state.set_low_power_armed_ms(now_ms);
        }
    }

    /// On Tick, check whether the armed low-power timer has elapsed and emit
    /// EnterLowPower if so.
    fn maybe_fire_low_power(
        &self,
        state: &mut State,
        cfg: &Config,
        now_ms: u64,
        out: &mut Vec<Action>,
    ) {
        if !state.low_power_armed() || state.low_power_active() {
            return;
        }

        let due = state
            .low_power_armed_ms()
            .saturating_add(cfg.low_power_when_idle_timeout.saturating_mul(1000));

        if now_ms >= due {
            out.push(Action::EnterLowPower);
            state.set_low_power_active(true);
            state.set_low_power_armed(false);
        }
    }

    fn refresh_timing_holds(&self, state: &mut State, cfg: &Config, now_ms: u64) {
        let new_paused =
            state.manually_paused() || state.inhibitors_active() || state.system_paused();
        let was_paused = state.paused();

        if !was_paused && new_paused {
            self.finish_suspend_hold(state, now_ms);
            state.set_pause_started_ms(Some(now_ms));
        } else if was_paused && !new_paused {
            let started_ms = state.take_pause_started_ms();
            Self::shift_step_timing_from(state, started_ms, now_ms);
        }

        state.set_paused(new_paused);

        if new_paused {
            state.set_suspend_hold_started_ms(None);
            return;
        }

        let should_hold = !state.debounce_pending()
            && state.suspend_inhibitors_active()
            && Self::next_runnable_step_is_suspend(state, cfg);

        if should_hold {
            if state.suspend_hold_started_ms().is_none() {
                state.set_suspend_hold_started_ms(Some(now_ms));
            }
        } else {
            self.finish_suspend_hold(state, now_ms);
        }
    }

    fn finish_suspend_hold(&self, state: &mut State, now_ms: u64) {
        let started_ms = state.take_suspend_hold_started_ms();
        Self::shift_step_timing_from(state, started_ms, now_ms);
    }

    fn shift_step_timing_from(state: &mut State, started_ms: Option<u64>, now_ms: u64) {
        let Some(started_ms) = started_ms else {
            return;
        };

        let elapsed_ms = now_ms.saturating_sub(started_ms);
        state.set_step_base_ms(state.step_base_ms().saturating_add(elapsed_ms));

        if state.pre_action_notify_sent() {
            state.set_pre_action_notify_ms(state.pre_action_notify_ms().saturating_add(elapsed_ms));
        }
    }

    fn next_runnable_step_is_suspend(state: &State, cfg: &Config) -> bool {
        let mut idx = state.step_index();
        while idx < cfg.plan.len() && !cfg.plan[idx].enabled() {
            idx += 1;
        }

        if idx < cfg.plan.len() && Self::is_lock_step(&cfg.plan[idx]) && state.is_locked() {
            idx += 1;
            while idx < cfg.plan.len() && !cfg.plan[idx].enabled() {
                idx += 1;
            }
        }

        idx < cfg.plan.len() && matches!(cfg.plan[idx].kind, PlanStepKind::Suspend)
    }

    pub(super) fn normalize_trigger_name(s: &str) -> String {
        let mut t = s.trim().to_ascii_lowercase();
        t = t.replace([' ', '\t'], "");
        t = t.replace('_', "-");
        t
    }

    fn trigger_matches_step(name: &str, step: &PlanStep) -> bool {
        let n = Self::normalize_trigger_name(name);

        let n = match n.as_str() {
            "lockscreen" => "lock-screen".to_string(),
            "lock" => "lock-screen".to_string(),
            _ => n,
        };

        match &step.kind {
            PlanStepKind::Startup => n == "startup",
            PlanStepKind::Dpms => n == "dpms",
            PlanStepKind::Brightness => n == "brightness",
            PlanStepKind::LockScreen => n == "lock-screen",
            PlanStepKind::Suspend => n == "suspend",
            PlanStepKind::Custom(k) => {
                let k_norm = Self::normalize_trigger_name(k);
                n == k_norm || n == format!("custom:{k_norm}") || n == format!("custom-{k_norm}")
            }
        }
    }

    fn find_trigger_step<'a>(&self, cfg: &'a Config, name: &str) -> Option<(usize, &'a PlanStep)> {
        for (idx, step) in cfg.plan.iter().enumerate() {
            if !step.enabled() {
                continue;
            }
            if Self::trigger_matches_step(name, step) {
                return Some((idx, step));
            }
        }
        None
    }

    fn is_lock_step(step: &PlanStep) -> bool {
        matches!(step.kind, PlanStepKind::LockScreen)
    }

    fn is_dpms_group(step: &PlanStep) -> bool {
        match &step.kind {
            PlanStepKind::Dpms => true,
            PlanStepKind::Custom(name) => Self::normalize_trigger_name(name) == "early-dpms",
            _ => false,
        }
    }

    fn is_brightness_group(step: &PlanStep) -> bool {
        matches!(step.kind, PlanStepKind::Brightness)
    }

    fn first_enabled_step_after_lock(&self, cfg: &Config) -> usize {
        let mut seen_lock = false;
        for (i, s) in cfg.plan.iter().enumerate() {
            if !s.enabled() {
                continue;
            }
            if !seen_lock && Self::is_lock_step(s) {
                seen_lock = true;
                continue;
            }
            if seen_lock {
                return i;
            }
        }
        cfg.plan.len()
    }

    fn maybe_fire_startup_instants(
        &self,
        state: &mut State,
        cfg: &Config,
        now_ms: u64,
    ) -> Vec<Action> {
        let mut idx = 0usize;
        let mut out = Vec::new();

        loop {
            while idx < cfg.plan.len() && !cfg.plan[idx].enabled() {
                idx += 1;
            }
            if idx >= cfg.plan.len() {
                break;
            }

            let step = &cfg.plan[idx];
            let is_startup = matches!(step.kind, PlanStepKind::Startup);
            if !(is_startup && step.is_instant()) {
                break;
            }

            if state.one_shot_has_fired_step(step) {
                idx += 1;
                continue;
            }

            let emitted = self.actions_for_plan_step(state, step, cfg);
            if !emitted.is_empty() {
                let arms_resume = step.resume_command.is_some();
                let is_dpms = Self::is_dpms_group(step);
                let is_brightness = Self::is_brightness_group(step);
                let is_lock = Self::is_lock_step(step);

                state.mark_step_fired(idx, is_dpms, is_brightness, is_lock, arms_resume);

                state.mark_action_fired(now_ms);
                state.set_pre_action_notify_sent(false);
            }

            out.extend(emitted);
            state.mark_one_shot_fired_step(step);

            idx += 1;
        }

        out
    }

    fn sync_step_index_after_startup_instants(&self, state: &mut State, cfg: &Config) {
        if state.step_index() != 0 {
            return;
        }

        let mut idx = 0usize;

        loop {
            while idx < cfg.plan.len() && !cfg.plan[idx].enabled() {
                idx += 1;
            }
            if idx >= cfg.plan.len() {
                state.set_step_index(cfg.plan.len());
                return;
            }

            let step = &cfg.plan[idx];
            let is_startup_instant =
                matches!(step.kind, PlanStepKind::Startup) && step.is_instant();

            if is_startup_instant {
                idx += 1;
                continue;
            }

            state.set_step_index(idx);
            return;
        }
    }

    fn advance_past_lock_if_needed(&self, state: &mut State, cfg: &Config) {
        if !state.is_locked() {
            return;
        }

        let mut idx = state.step_index();
        while idx < cfg.plan.len() && !cfg.plan[idx].enabled() {
            idx += 1;
        }

        if idx < cfg.plan.len() && Self::is_lock_step(&cfg.plan[idx]) {
            idx += 1;
            while idx < cfg.plan.len() && !cfg.plan[idx].enabled() {
                idx += 1;
            }
            state.set_step_index(idx);
            state.set_pre_action_notify_sent(false);
        }
    }

    fn maybe_fire_next_step(&self, state: &mut State, cfg: &Config, now_ms: u64) -> Vec<Action> {
        let mut out = Vec::new();
        let mut idx = state.step_index();

        loop {
            while idx < cfg.plan.len() && !cfg.plan[idx].enabled() {
                idx += 1;
            }
            if idx >= cfg.plan.len() {
                state.set_step_index(cfg.plan.len());
                return out;
            }

            if Self::is_lock_step(&cfg.plan[idx]) && state.is_locked() {
                idx += 1;
                state.set_step_index(idx);
                state.set_pre_action_notify_sent(false);
                continue;
            }

            let step = &cfg.plan[idx];
            if matches!(step.kind, PlanStepKind::Suspend) && state.suspend_inhibitors_active() {
                self.refresh_timing_holds(state, cfg, now_ms);
                state.set_step_index(idx);
                return out;
            }

            if step.is_instant() {
                if state.one_shot_has_fired_step(step) {
                    idx += 1;
                    state.set_step_index(idx);
                    continue;
                }

                let emitted = self.actions_for_plan_step(state, step, cfg);
                if !emitted.is_empty() {
                    let arms_resume = step.resume_command.is_some();
                    let is_dpms = Self::is_dpms_group(step);
                    let is_brightness = Self::is_brightness_group(step);
                    let is_lock = Self::is_lock_step(step);

                    state.mark_step_fired(idx, is_dpms, is_brightness, is_lock, arms_resume);
                }
                out.extend(emitted);
                state.mark_one_shot_fired_step(step);

                idx += 1;
                state.set_step_index(idx);

                state.set_step_base_ms(now_ms);
                state.mark_action_fired(now_ms);
                state.set_pre_action_notify_sent(false);
                continue;
            }

            break;
        }

        while idx < cfg.plan.len() && !cfg.plan[idx].enabled() {
            idx += 1;
        }
        if idx >= cfg.plan.len() {
            state.set_step_index(cfg.plan.len());
            return out;
        }

        let step = &cfg.plan[idx];

        let debounce_ms = if state.debounce_pending() {
            cfg.debounce_seconds.saturating_mul(1000)
        } else {
            0
        };
        let timeout_ms = step.timeout_seconds.saturating_mul(1000);

        let base_due_ms = state
            .step_base_ms()
            .saturating_add(debounce_ms)
            .saturating_add(timeout_ms);

        let has_notification = cfg.notify_before_action && step.notification.is_some();
        let notify_wait_ms = step.notify_seconds_before.unwrap_or(0).saturating_mul(1000);

        if has_notification {
            if now_ms < base_due_ms && !state.pre_action_notify_sent() {
                return out;
            }

            if !state.pre_action_notify_sent() {
                let msg = step.notification.clone().unwrap();
                out.push(Action::Notify {
                    message: msg,
                    icon: step
                        .notification_icon
                        .clone()
                        .or_else(|| cfg.notification_icon.clone()),
                });

                state.set_pre_action_notify_sent(true);
                state.set_pre_action_notify_ms(now_ms);
                return out;
            }

            let due_after_notify_ms = state.pre_action_notify_ms().saturating_add(notify_wait_ms);
            if now_ms < due_after_notify_ms {
                return out;
            }
        } else if now_ms < base_due_ms {
            return out;
        }

        let emitted = self.actions_for_plan_step(state, step, cfg);
        if !emitted.is_empty() {
            let arms_resume = step.resume_command.is_some();
            let is_dpms = Self::is_dpms_group(step);
            let is_brightness = Self::is_brightness_group(step);
            let is_lock = Self::is_lock_step(step);

            state.mark_step_fired(idx, is_dpms, is_brightness, is_lock, arms_resume);

            // Arm the low-power timer as soon as the DPMS (or early-dpms) step
            // has fired — the timer counts from this moment.
            if is_dpms {
                self.arm_low_power_after_dpms(state, cfg, now_ms);
            }
        }
        out.extend(emitted);

        state.set_step_index(idx + 1);
        state.set_step_base_ms(now_ms);
        state.mark_action_fired(now_ms);

        state.set_pre_action_notify_sent(false);
        state.set_debounce_pending(false);

        out
    }

    fn actions_for_plan_step(&self, state: &State, step: &PlanStep, cfg: &Config) -> Vec<Action> {
        match &step.kind {
            PlanStepKind::LockScreen => {
                if state.is_locked() {
                    return Vec::new();
                }

                step.command
                    .clone()
                    .map(|cmd| vec![Action::RunLockScreen { command: cmd }])
                    .unwrap_or_default()
            }

            PlanStepKind::Suspend => {
                let mut out = Vec::new();

                // pre_suspend_command always fires first, before the actual suspend command.
                if let Some(cmd) = cfg.pre_suspend_command.clone() {
                    out.push(Action::RunCommand { command: cmd });
                }

                if let Some(cmd) = step.command.clone() {
                    out.push(Action::RunCommand { command: cmd });
                } else {
                    out.push(Action::Suspend);
                }

                out
            }

            _ => step
                .command
                .clone()
                .map(|c| vec![Action::RunCommand { command: c }])
                .unwrap_or_default(),
        }
    }

    fn resume_commands_for_activity(&self, state: &mut State, cfg: &Config) -> Vec<Action> {
        if !state.resume_due() {
            return Vec::new();
        }

        let mut out = Vec::new();

        if let Some(idx) = state.last_dpms_fired_idx() {
            if idx < cfg.plan.len() {
                if let Some(cmd) = cfg.plan[idx].resume_command.clone() {
                    out.push(Action::RunResumeCommand { command: cmd });
                }
            }
        }

        if let Some(idx) = state.last_brightness_fired_idx() {
            if idx < cfg.plan.len() {
                if let Some(cmd) = cfg.plan[idx].resume_command.clone() {
                    out.push(Action::RunResumeCommand { command: cmd });
                }
            }
        }

        let mut needs_defer_until_unlock = false;

        if state.is_locked() {
            if let Some(idx) = state.last_lock_fired_idx() {
                if idx < cfg.plan.len() && cfg.plan[idx].resume_command.is_some() {
                    needs_defer_until_unlock = true;
                }
            }

            if let Some(last) = state.last_fired_idx() {
                let skip = state.last_dpms_fired_idx() == Some(last)
                    || state.last_brightness_fired_idx() == Some(last)
                    || state.last_lock_fired_idx() == Some(last);

                if !skip && last < cfg.plan.len() && cfg.plan[last].resume_command.is_some() {
                    needs_defer_until_unlock = true;
                }
            }

            if needs_defer_until_unlock {
                state.set_resume_deferred_until_unlock(true);
            }

            if !out.is_empty() || needs_defer_until_unlock {
                state.mark_resumed();
            }

            return out;
        }

        if let Some(idx) = state.last_lock_fired_idx() {
            if idx < cfg.plan.len() {
                if let Some(cmd) = cfg.plan[idx].resume_command.clone() {
                    out.push(Action::RunResumeCommand { command: cmd });
                }
            }
        }

        if let Some(last) = state.last_fired_idx() {
            let skip = state.last_dpms_fired_idx() == Some(last)
                || state.last_brightness_fired_idx() == Some(last)
                || state.last_lock_fired_idx() == Some(last);

            if !skip && last < cfg.plan.len() {
                if let Some(cmd) = cfg.plan[last].resume_command.clone() {
                    out.push(Action::RunResumeCommand { command: cmd });
                }
            }
        }

        state.mark_resumed();
        out
    }
}
