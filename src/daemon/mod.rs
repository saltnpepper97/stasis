// Author: Dustin Pilgrim
// License: GPL-3.0-only

mod actions;
mod run;

use crate::core::{
    action::Action,
    config::{ConfigFile, Pattern, PlanSource},
    events::{Event, PowerState},
    manager::Manager,
    manager_msg::ManagerMsg,
    state::State,
};

use std::path::PathBuf;

use tokio::sync::{mpsc, watch};

use crate::core::info::WatchEvent;
use crate::core::report::{EpisodeKind, ReportRecorder};
use crate::services::dbus::EventSink;
use crate::services::low_power::LowPowerController;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

struct MpscEventSink {
    tx: mpsc::Sender<ManagerMsg>,
}

impl EventSink for MpscEventSink {
    fn push(&self, ev: Event) {
        let _ = self.tx.try_send(ManagerMsg::Event(ev));
    }
}

pub struct Daemon {
    manager: Manager,
    state: State,

    config_path: PathBuf,

    inhibit_apps: Vec<Pattern>,
    suspend_inhibit_apps: Vec<Pattern>,

    monitor_media: bool,
    ignore_remote_media: bool,
    media_blacklist: Vec<Pattern>,

    inhibit_epoch: u64,
    enable_loginctl: bool,
    enable_dbus_inhibit: bool,

    /// Conservative hardware power-down controller for the low-power idle phase.
    /// Snapshot/restore based; restores exactly what it changed on any resume.
    low_power: LowPowerController,

    /// Power-saving episode recorder for `stasis report`.
    report: ReportRecorder,

    chassis: crate::core::utils::ChassisKind,
    bad_profile_logged: bool,

    verbose: bool,

    /// Latest shell-facing state for persistent IPC watchers.
    watch_tx: watch::Sender<WatchEvent>,
}

impl Daemon {
    pub fn new(mut cfg_file: ConfigFile, config_path: PathBuf, verbose: bool) -> Self {
        let now_ms = crate::core::utils::now_ms();
        let chassis = crate::core::utils::detect_chassis();

        let plan_src = match chassis {
            crate::core::utils::ChassisKind::Desktop => PlanSource::Desktop,
            crate::core::utils::ChassisKind::Laptop => {
                if crate::core::utils::is_on_ac_power() {
                    PlanSource::Ac
                } else {
                    PlanSource::Battery
                }
            }
        };

        let normalized_active_profile = cfg_file
            .active_profile
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .and_then(|s| {
                if s.eq_ignore_ascii_case("default") || s.eq_ignore_ascii_case("none") {
                    None
                } else {
                    Some(s.to_string())
                }
            })
            .filter(|name| cfg_file.profiles.iter().any(|p| p.name == *name));

        cfg_file.active_profile = normalized_active_profile;

        let effective = cfg_file
            .effective_for(cfg_file.active_profile.as_deref(), plan_src)
            .unwrap_or_else(|| {
                let mut c = cfg_file.default.clone();
                c.select_plan_source(PlanSource::Desktop);
                c
            });

        let inhibit_apps = effective.inhibit_apps.clone();
        let suspend_inhibit_apps = effective.suspend_inhibit_apps.clone();
        let monitor_media = effective.monitor_media;
        let ignore_remote_media = effective.ignore_remote_media;
        let media_blacklist = effective.media_blacklist.clone();

        let enable_loginctl_integration = effective.enable_loginctl_integration;
        let enable_dbus_inhibit = effective.enable_dbus_inhibit;

        eventline::debug!(
            "daemon: chassis={:?}, plan_src={:?}, active_profile={:?}, monitor_media={}, ignore_remote_media={}, media_blacklist_len={}, inhibit_apps_len={}, suspend_inhibit_apps_len={}, enable_loginctl_integration={}, enable_dbus_inhibit={}, config_path={}",
            chassis,
            plan_src,
            cfg_file.active_profile,
            monitor_media,
            ignore_remote_media,
            media_blacklist.len(),
            inhibit_apps.len(),
            suspend_inhibit_apps.len(),
            enable_loginctl_integration,
            enable_dbus_inhibit,
            config_path.display(),
        );

        let mut state = State::new(now_ms);
        state.set_plan_source(plan_src);

        match plan_src {
            PlanSource::Ac => state.set_power_state(PowerState::OnAC),
            PlanSource::Battery => state.set_power_state(PowerState::OnBattery),
            PlanSource::Desktop => {}
        }

        state.set_active_profile(cfg_file.active_profile.clone());

        let manager = Manager::new(cfg_file);
        let (watch_tx, _) = watch::channel(manager.watch_event(&state));

        Self {
            manager,
            state,
            config_path,
            inhibit_apps,
            suspend_inhibit_apps,
            monitor_media,
            ignore_remote_media,
            media_blacklist,
            inhibit_epoch: 0,
            enable_loginctl: enable_loginctl_integration,
            enable_dbus_inhibit,
            low_power: LowPowerController::new(),
            report: ReportRecorder::new(),
            chassis,
            bad_profile_logged: false,
            verbose,
            watch_tx,
        }
    }

    fn push_inhibit_rules_from_effective(&mut self, tx: &mpsc::Sender<ManagerMsg>) {
        let cfg_file = self.manager.cfg_file_ref();

        let plan_src = self.state.plan_source();
        let prof = self.state.active_profile();

        let effective = cfg_file.effective_for(prof, plan_src).unwrap_or_else(|| {
            let mut c = cfg_file.default.clone();
            c.select_plan_source(PlanSource::Desktop);
            c
        });

        self.inhibit_epoch = self.inhibit_epoch.wrapping_add(1);

        let msg = ManagerMsg::UpdateInhibitRules {
            epoch: self.inhibit_epoch,
            inhibit_apps: effective.inhibit_apps.clone(),
            suspend_inhibit_apps: effective.suspend_inhibit_apps.clone(),
            monitor_media: effective.monitor_media,
            ignore_remote_media: effective.ignore_remote_media,
            media_blacklist: effective.media_blacklist.clone(),
        };

        let _ = tx.try_send(msg);
    }

    fn handle_one_event_scoped(&mut self, event: Event) -> Vec<Action> {
        if matches!(event, Event::Tick { .. }) {
            return self
                .manager
                .handle_event(&mut self.state, event)
                .unwrap_or_else(|e| {
                    self.log_handle_event_error_once(&e);
                    Vec::new()
                });
        }

        // If not verbose, avoid scope! entirely (prevents "done: event#..." spam)
        if !self.verbose {
            return self
                .manager
                .handle_event(&mut self.state, event)
                .unwrap_or_else(|e| {
                    self.log_handle_event_error_once(&e);
                    Vec::new()
                });
        }

        eventline::scope!("event", {
            eventline::debug!("incoming: {:?}", event);

            match self.manager.handle_event(&mut self.state, event.clone()) {
                Ok(actions) => {
                    if !actions.is_empty() {
                        eventline::debug!("actions: {:?}", actions);
                    }
                    actions
                }
                Err(e) => {
                    self.log_handle_event_error_once(&e);
                    Vec::new()
                }
            }
        })
    }

    fn log_handle_event_error_once(&mut self, e: &crate::core::error::Error) {
        let s = format!("{e:?}");
        if s.contains("ProfileNotFound") {
            if !self.bad_profile_logged {
                self.bad_profile_logged = true;
                eventline::error!(
                    "handle_event failed: {:?} (config selection failed; active_profile is invalid — try `stasis profile none` or a real profile name)",
                    e
                );
            }
        } else {
            eventline::error!("handle_event failed: {:?}", e);
        }
    }

    /// Publish only semantic changes, never timer/countdown churn.
    fn publish_watch_state(&self) {
        let next = self.manager.watch_event(&self.state);
        self.watch_tx.send_if_modified(|current| {
            if *current == next {
                false
            } else {
                *current = next;
                true
            }
        });
    }

    // ---------------- telemetry helpers ----------------

    /// Record suspend/resume episodes from events before the manager consumes them.
    fn report_observe_event(&mut self, event: &Event) {
        match event {
            Event::PrepareForSleep { now_ms } => {
                self.report.start(EpisodeKind::Suspend, *now_ms);
            }
            Event::ResumedFromSleep { now_ms } => {
                self.report.end(EpisodeKind::Suspend, *now_ms);
            }
            _ => {}
        }
    }

    /// Record low-power episodes from actions returned by the manager.
    fn report_observe_action(&mut self, action: &Action) {
        let now = crate::core::utils::now_ms();
        match action {
            Action::EnterLowPower => self.report.start(EpisodeKind::LowPower, now),
            Action::ExitLowPower => self.report.end(EpisodeKind::LowPower, now),
            _ => {}
        }
    }

    /// Track display-off (DPMS) episodes via state transitions.
    fn report_track_display_off(&mut self, was_off: bool, is_off: bool) {
        let now = crate::core::utils::now_ms();
        if !was_off && is_off {
            self.report.start(EpisodeKind::DisplayOff, now);
        } else if was_off && !is_off {
            self.report.end(EpisodeKind::DisplayOff, now);
        }
    }

    fn report_flush(&mut self) {
        self.report.flush(crate::core::utils::now_ms());
    }
}
