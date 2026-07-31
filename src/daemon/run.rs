// Author: Dustin Pilgrim
// License: GPL-3.0-only

use crate::core::{
    events::Event,
    manager_msg::{ListKind, ManagerMsg},
};

use tokio::sync::{mpsc, watch};

use std::sync::Arc;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
use std::time::Duration;

use crate::services::dbus::EventSink;

use super::{AnyError, Daemon, MpscEventSink};

// `unsafe extern` is required as of Rust edition 2024.
// Declared at module level so the linker resolves it at compile time.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
unsafe extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}

/// Periodically calls malloc_trim(0) to return fragmented heap pages back to
/// the OS. Without this, RSS creeps upward as the allocator holds on to freed
/// pages across poll cycles. Runs once per minute — effectively free.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn spawn_heap_trimmer() {
    tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            // SAFETY: malloc_trim is a glibc extension that returns freed pages
            // to the OS. No preconditions beyond being on a glibc Linux system.
            unsafe { malloc_trim(0) };
        }
    });
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn spawn_heap_trimmer() {}

impl Daemon {
    pub async fn run(
        &mut self,
        mut shutdown: watch::Receiver<bool>,
        shutdown_tx: watch::Sender<bool>,
    ) -> Result<(), AnyError> {
        eventline::info!("daemon starting");

        // 32 is ample for an idle manager. Stasis events arrive slowly
        // (ticks, input edges, D-Bus signals) — 256 was wasted reservation.
        let (tx, mut rx) = mpsc::channel::<ManagerMsg>(32);

        if let Err(e) = crate::ipc::server::spawn_ipc_server(
            tx.clone(),
            self.watch_tx.subscribe(),
            self.verbose,
        )
        .await
        {
            eventline::warn!("ipc: failed to start: {}", e);
        }

        {
            let sink: Arc<dyn EventSink> = Arc::new(MpscEventSink { tx: tx.clone() });

            match crate::services::dbus::spawn_dbus_listeners(
                sink,
                self.enable_loginctl,
                self.enable_dbus_inhibit,
                shutdown.clone(),
            ) {
                Ok(_handle) => eventline::info!("dbus: listener started"),
                Err(e) => eventline::warn!("dbus: failed to start listener thread: {e}"),
            }
        }

        tokio::spawn(crate::services::ticker::run_ticker(tx.clone()));

        let (app_rules_tx, app_rules_rx) = watch::channel(crate::services::app_inhibit::AppRules {
            epoch: self.inhibit_epoch,
            apps: self.inhibit_apps.clone(),
        });
        tokio::spawn(crate::services::app_inhibit::run_app_inhibit(
            tx.clone(),
            app_rules_rx,
        ));

        let (media_rules_tx, media_rules_rx) = watch::channel(crate::services::media::MediaRules {
            epoch: self.inhibit_epoch,
            monitor_media: self.monitor_media,
            ignore_remote_media: self.ignore_remote_media,
            media_blacklist: self.media_blacklist.clone(),
        });
        tokio::spawn(crate::services::media::run_media(
            tx.clone(),
            media_rules_rx,
        ));

        if matches!(self.chassis, crate::core::utils::ChassisKind::Laptop) {
            tokio::spawn(crate::services::power::run_power(tx.clone()));
        } else {
            eventline::info!("power: skipped (desktop chassis)");
        }

        tokio::spawn({
            let tx = tx.clone();
            let shutdown = shutdown.clone();
            let shutdown_tx = shutdown_tx.clone();
            async move {
                let _ = crate::services::wayland::run_wayland(tx, shutdown, shutdown_tx).await;
            }
        });

        // Return fragmented heap pages to the OS once a minute.
        spawn_heap_trimmer();

        self.push_inhibit_rules_from_effective(&tx);

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        eventline::info!("daemon stopping (shutdown requested)");
                        break;
                    }
                }

                maybe = rx.recv() => {
                    let Some(msg) = maybe else {
                        eventline::info!("daemon stopping (event channel closed)");
                        break;
                    };

                    match msg {
                        ManagerMsg::Event(event) => {
                            let refresh_after = matches!(event, Event::ProfileChanged{..} | Event::PowerChanged{..});

                            // Telemetry: observe suspend/resume events.
                            self.report_observe_event(&event);

                            // Track display-off state before the manager handles the event.
                            let dpms_was = self.state.last_dpms_fired_idx().is_some();

                            let actions = self.handle_one_event_scoped(event);

                            self.publish_watch_state();

                            // Track display-off state after handling.
                            self.report_track_display_off(
                                dpms_was,
                                self.state.last_dpms_fired_idx().is_some(),
                            );

                            // Telemetry: observe low-power enter/exit actions.
                            for action in &actions {
                                self.report_observe_action(action);
                            }

                            if refresh_after {
                                self.push_inhibit_rules_from_effective(&tx);
                            }

                            for action in actions {
                                if let Err(e) = self.exec_action_with_tx(action, tx.clone()).await {
                                    eventline::error!("action failed: {}", e);
                                }
                            }
                        }

                        ManagerMsg::UpdateInhibitRules { epoch, inhibit_apps, monitor_media, ignore_remote_media, media_blacklist } => {
                            self.inhibit_epoch = epoch;
                            self.inhibit_apps = inhibit_apps.clone();
                            self.monitor_media = monitor_media;
                            self.ignore_remote_media = ignore_remote_media;
                            self.media_blacklist = media_blacklist.clone();

                            let _ = app_rules_tx.send(crate::services::app_inhibit::AppRules {
                                epoch,
                                apps: inhibit_apps,
                            });

                            let _ = media_rules_tx.send(crate::services::media::MediaRules {
                                epoch,
                                monitor_media,
                                ignore_remote_media,
                                media_blacklist,
                            });
                        }

                        ManagerMsg::List { kind, reply } => {
                            let out = match kind {
                                ListKind::Actions => Ok(self.manager.list_actions(&self.state)),
                                ListKind::Profiles => Ok(self.manager.list_profiles()),
                            };
                            let _ = reply.send(out);
                        }

                        ManagerMsg::GetInfo { reply } => {
                            let now_ms = crate::core::utils::now_ms();
                            let snap = self.manager.snapshot(&self.state, now_ms);
                            let _ = reply.send(snap);
                        }

                        ManagerMsg::SetProfile { name, reply } => {
                            let now_ms = crate::core::utils::now_ms();
                            let raw = name.clone().unwrap_or_else(|| "none".to_string());

                            let ev = Event::ProfileChanged { name: raw, now_ms };
                            let res = self.manager.handle_event(&mut self.state, ev);

                            let out = match res {
                                Ok(_actions) => {
                                    self.bad_profile_logged = false;
                                    let shown = name.unwrap_or_else(|| "none".to_string());

                                    self.push_inhibit_rules_from_effective(&tx);
                                    self.publish_watch_state();

                                    Ok(format!("Profile set: {shown}"))
                                }
                                Err(e) => Err(format!("{e:?}")),
                            };

                            let _ = reply.send(out);
                        }

                        ManagerMsg::ReloadConfig { reply } => {
                            let now_ms = crate::core::utils::now_ms();
                            let loaded = crate::config::load_from_path(&self.config_path);

                            let out = match loaded {
                                Ok(loaded) => {
                                    if loaded.path != self.config_path {
                                        eventline::warn!(
                                            "reload: primary config failed; fell back to {}",
                                            loaded.path.display()
                                        );
                                        self.config_path = loaded.path.clone();
                                    }

                                    let new_cfg_file = loaded.cfg;
                                    self.manager.set_config(new_cfg_file.clone());

                                    let desired = match self.state.active_profile() {
                                        Some(name) => {
                                            if new_cfg_file.effective_for(Some(name), self.state.plan_source()).is_some() {
                                                name.to_string()
                                            } else {
                                                "none".to_string()
                                            }
                                        }
                                        None => "none".to_string(),
                                    };

                                    let ev = Event::ProfileChanged { name: desired.clone(), now_ms };

                                    match self.manager.handle_event(&mut self.state, ev) {
                                        Ok(actions) => {
                                            self.bad_profile_logged = false;

                                            for action in actions {
                                                if let Err(e) = self.exec_action_with_tx(action, tx.clone()).await {
                                                    eventline::error!("action failed: {}", e);
                                                }
                                            }

                                            self.push_inhibit_rules_from_effective(&tx);
                                            self.publish_watch_state();

                                            if desired == "none" {
                                                Ok("Configuration reloaded; using the base configuration.".to_string())
                                            } else {
                                                Ok(format!(
                                                    "Configuration reloaded; profile '{desired}' remains active."
                                                ))
                                            }
                                        }
                                        Err(e) => Err(format!("{e:?}")),
                                    }
                                }
                                Err(e) => Err(e),
                            };

                            let _ = reply.send(out);
                        }

                        ManagerMsg::StopDaemon { reply } => {
                            eventline::info!("daemon stopping (stop requested via IPC)");
                            let _ = reply.send(Ok("Stopping Stasis daemon".to_string()));
                            let _ = shutdown_tx.send(true);
                            break;
                        }
                    }
                }
            }
        }

        // Explicit safety-net restore before the controller's Drop fires.
        // Ensures the user's hardware is never left in a reduced power state.
        if self.low_power.is_active() {
            eventline::info!("low_power: restoring hardware before shutdown");
            self.low_power.exit();
        }

        // Flush any open telemetry episodes so they appear in `stasis report`.
        self.report_flush();

        Ok(())
    }
}
