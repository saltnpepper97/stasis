// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use tokio::sync::{Mutex, watch};
use zbus::{Connection, MatchRule, Proxy};

use crate::core::events::{Event, LockSource};

/// Sink for pushing events into the (sync) manager loop.
/// Implement this for whatever channel/queue you're using.
pub trait EventSink: Send + Sync + 'static {
    fn push(&self, ev: Event);
}

/// Spawn D-Bus listeners.
///
/// `enable_loginctl_integration` gates optional login1 sleep monitoring:
/// - PrepareForSleep (org.freedesktop.login1.Manager)
///
/// login1's Lock/Unlock signals are requests, not completed state changes, so
/// they are not used for lock tracking. LockedHint state is monitored
/// independently and automatically.
///
/// `enable_dbus_inhibit` gates session-bus inhibit monitoring:
/// - org.freedesktop.ScreenSaver Inhibit/UnInhibit
/// - org.gnome.SessionManager Inhibit/Uninhibit
/// - org.freedesktop.portal.Inhibit Inhibit + Request.Close
///
/// Lid events via UPower are always monitored when system bus is available.
///
/// Uses a `current_thread` runtime rather than the default multi-thread one.
/// D-Bus listening is purely I/O-bound with no CPU parallelism needed; the
/// full multi-thread runtime was spending ~1-2 MB on worker-thread stacks and
/// work-stealing queues that were never used.
pub fn spawn_dbus_listeners(
    sink: Arc<dyn EventSink>,
    enable_loginctl: bool,
    enable_dbus_inhibit: bool,
    shutdown: watch::Receiver<bool>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    Ok(std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio current_thread runtime");

        rt.block_on(async move {
            if let Err(e) = run_dbus(sink, enable_loginctl, enable_dbus_inhibit, shutdown).await {
                eventline::error!("D-Bus listener failed: {e:?}");
            }
        });
    }))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// Track inhibitors by unique D-Bus sender.
// Legacy APIs are cookie-based. Portal APIs are request-handle based.
#[derive(Debug, Default)]
struct DbusInhibitTracker {
    // Active inhibit state keyed by unique D-Bus sender name (e.g. ":1.29").
    active_senders: HashMap<String, SenderInhibitState>,

    // Pending legacy Inhibit method-call serials per sender, waiting for
    // method-return that carries the cookie.
    pending_legacy_calls: HashMap<String, HashSet<u32>>,

    // Pending portal Inhibit method-call serials per sender, waiting for
    // method-return that carries the request handle.
    pending_portal_calls: HashMap<String, HashSet<u32>>,
}

#[derive(Debug, Default)]
struct SenderInhibitState {
    // Legacy ScreenSaver / SessionManager inhibit cookies active for this sender.
    legacy_cookies: HashSet<u32>,

    // Portal Request handles currently active for this sender.
    portal_handles: HashSet<String>,
}

impl DbusInhibitTracker {
    fn total(&self) -> usize {
        self.active_senders.len()
    }

    fn mark_legacy_call(&mut self, sender: &str, serial: u32) {
        self.pending_legacy_calls
            .entry(sender.to_string())
            .or_default()
            .insert(serial);
    }

    fn clear_legacy_call(&mut self, sender: &str, serial: u32) {
        let remove_sender = if let Some(set) = self.pending_legacy_calls.get_mut(sender) {
            set.remove(&serial);
            set.is_empty()
        } else {
            false
        };

        if remove_sender {
            self.pending_legacy_calls.remove(sender);
        }
    }

    fn confirm_legacy_cookie(&mut self, sender: &str, reply_serial: u32, cookie: u32) -> bool {
        let had_pending = self
            .pending_legacy_calls
            .get(sender)
            .is_some_and(|set| set.contains(&reply_serial));
        if !had_pending {
            return false;
        }

        self.clear_legacy_call(sender, reply_serial);
        let state = self.active_senders.entry(sender.to_string()).or_default();
        state.legacy_cookies.insert(cookie)
    }

    fn clear_legacy_cookie(&mut self, sender: &str, cookie: u32) -> bool {
        let removed = if let Some(state) = self.active_senders.get_mut(sender) {
            state.legacy_cookies.remove(&cookie)
        } else {
            false
        };

        if removed {
            self.drop_sender_if_empty(sender);
        }

        removed
    }

    #[cfg(test)]
    fn mark_legacy_active(&mut self, sender: &str) {
        let state = self.active_senders.entry(sender.to_string()).or_default();
        state.legacy_cookies.insert(0);
    }

    #[cfg(test)]
    fn clear_legacy(&mut self, sender: &str) {
        let removed = if let Some(state) = self.active_senders.get_mut(sender) {
            state.legacy_cookies.remove(&0)
        } else {
            false
        };

        if removed {
            self.drop_sender_if_empty(sender);
        }
    }

    fn mark_portal_call(&mut self, sender: &str, serial: u32) {
        self.pending_portal_calls
            .entry(sender.to_string())
            .or_default()
            .insert(serial);
    }

    fn clear_portal_call(&mut self, sender: &str, serial: u32) {
        let remove_sender = if let Some(set) = self.pending_portal_calls.get_mut(sender) {
            set.remove(&serial);
            set.is_empty()
        } else {
            false
        };
        if remove_sender {
            self.pending_portal_calls.remove(sender);
        }
    }

    fn confirm_portal_handle(&mut self, sender: &str, reply_serial: u32, handle: &str) -> bool {
        let had_pending = self
            .pending_portal_calls
            .get(sender)
            .is_some_and(|set| set.contains(&reply_serial));
        if !had_pending {
            return false;
        }

        self.clear_portal_call(sender, reply_serial);
        let state = self.active_senders.entry(sender.to_string()).or_default();
        state.portal_handles.insert(handle.to_string())
    }

    fn clear_portal_handle(&mut self, sender: &str, handle: &str) -> bool {
        let removed = if let Some(state) = self.active_senders.get_mut(sender) {
            state.portal_handles.remove(handle)
        } else {
            false
        };

        if removed {
            self.drop_sender_if_empty(sender);
        }

        removed
    }

    #[cfg(test)]
    fn mark_portal_active(&mut self, sender: &str) {
        self.mark_portal_call(sender, 0);
    }

    #[cfg(test)]
    fn clear_portal(&mut self, sender: &str) {
        let _ = self.clear_any_portal_handle(sender);
    }

    #[cfg(test)]
    fn clear_any_portal_handle(&mut self, sender: &str) -> bool {
        let removed = if let Some(state) = self.active_senders.get_mut(sender) {
            if let Some(h) = state.portal_handles.iter().next().cloned() {
                state.portal_handles.remove(&h)
            } else {
                false
            }
        } else {
            false
        };

        if removed {
            self.drop_sender_if_empty(sender);
        }

        removed
    }

    fn drop_sender_if_empty(&mut self, sender: &str) {
        let should_remove = self
            .active_senders
            .get(sender)
            .is_some_and(|s| s.legacy_cookies.is_empty() && s.portal_handles.is_empty());

        if should_remove {
            self.active_senders.remove(sender);
        }
    }

    fn remove_sender(&mut self, sender: &str) {
        self.active_senders.remove(sender);
        self.pending_legacy_calls.remove(sender);
        self.pending_portal_calls.remove(sender);
    }
}

async fn tracker_register_legacy_call(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sender: &str,
    serial: u32,
) {
    let mut t = tracker.lock().await;
    t.mark_legacy_call(sender, serial);
}

async fn tracker_confirm_legacy_cookie(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sink: &Arc<dyn EventSink>,
    sender: &str,
    reply_serial: u32,
    cookie: u32,
) {
    let mut t = tracker.lock().await;
    let old_total = t.total();
    let newly_inserted = t.confirm_legacy_cookie(sender, reply_serial, cookie);
    let new_total = t.total();
    let sender_legacy = t
        .active_senders
        .get(sender)
        .map(|s| s.legacy_cookies.len())
        .unwrap_or(0);
    let sender_handles = t
        .active_senders
        .get(sender)
        .map(|s| s.portal_handles.len())
        .unwrap_or(0);
    drop(t);

    if newly_inserted && old_total == 0 && new_total > 0 {
        eventline::debug!(
            "dbus: inhibit active (legacy sender={}, total={}, sender_legacy={}, sender_handles={}, cookie={})",
            sender,
            new_total,
            sender_legacy,
            sender_handles,
            cookie
        );
        sink.push(Event::BrowserActivity { now_ms: now_ms() });
    } else if newly_inserted {
        eventline::debug!(
            "dbus: legacy cookie added (sender={}, total={}, sender_legacy={}, sender_handles={}, cookie={})",
            sender,
            new_total,
            sender_legacy,
            sender_handles,
            cookie
        );
    }
}

async fn tracker_clear_legacy_call(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sender: &str,
    serial: u32,
) {
    let mut t = tracker.lock().await;
    t.clear_legacy_call(sender, serial);
}

async fn tracker_clear_legacy_cookie(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sink: &Arc<dyn EventSink>,
    sender: &str,
    cookie: u32,
) {
    let mut t = tracker.lock().await;
    let old_total = t.total();
    let removed = t.clear_legacy_cookie(sender, cookie);
    let new_total = t.total();
    let sender_legacy = t
        .active_senders
        .get(sender)
        .map(|s| s.legacy_cookies.len())
        .unwrap_or(0);
    let sender_handles = t
        .active_senders
        .get(sender)
        .map(|s| s.portal_handles.len())
        .unwrap_or(0);
    drop(t);

    if removed && old_total > 0 && new_total == 0 {
        eventline::debug!(
            "dbus: inhibit cleared (legacy sender={}, cookie={})",
            sender,
            cookie
        );
        sink.push(Event::BrowserInactive { now_ms: now_ms() });
    } else if removed {
        eventline::debug!(
            "dbus: legacy cookie cleared (sender={}, total={}, sender_legacy={}, sender_handles={}, cookie={})",
            sender,
            new_total,
            sender_legacy,
            sender_handles,
            cookie
        );
    } else {
        eventline::debug!(
            "dbus: legacy cookie clear ignored (sender={}, cookie={})",
            sender,
            cookie
        );
    }
}

async fn tracker_register_portal_call(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sender: &str,
    serial: u32,
) {
    let mut t = tracker.lock().await;
    t.mark_portal_call(sender, serial);
}

async fn tracker_confirm_portal_handle(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sink: &Arc<dyn EventSink>,
    sender: &str,
    reply_serial: u32,
    handle: &str,
) {
    let mut t = tracker.lock().await;
    let old_total = t.total();
    let newly_inserted = t.confirm_portal_handle(sender, reply_serial, handle);
    let new_total = t.total();
    let sender_handles = t
        .active_senders
        .get(sender)
        .map(|s| s.portal_handles.len())
        .unwrap_or(0);
    let sender_legacy = t
        .active_senders
        .get(sender)
        .map(|s| s.legacy_cookies.len())
        .unwrap_or(0);
    drop(t);

    if newly_inserted && old_total == 0 && new_total > 0 {
        eventline::debug!(
            "dbus: inhibit active (portal sender={}, total={}, sender_handles={}, sender_legacy={}, handle={})",
            sender,
            new_total,
            sender_handles,
            sender_legacy,
            handle
        );
        sink.push(Event::BrowserActivity { now_ms: now_ms() });
    } else if newly_inserted {
        eventline::debug!(
            "dbus: portal handle added (sender={}, total={}, sender_handles={}, sender_legacy={}, handle={})",
            sender,
            new_total,
            sender_handles,
            sender_legacy,
            handle
        );
    }
}

async fn tracker_clear_portal_call(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sender: &str,
    serial: u32,
) {
    let mut t = tracker.lock().await;
    t.clear_portal_call(sender, serial);
}

async fn tracker_clear_portal_handle(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sink: &Arc<dyn EventSink>,
    sender: &str,
    handle: &str,
) {
    // Keep the source-capture safeguard.
    // Portal close is deferred while browser source capture is still active.
    let source_capture_active = tokio::task::spawn_blocking(browser_source_capture_active_now)
        .await
        .unwrap_or(false);

    if source_capture_active {
        let t = tracker.lock().await;
        let total = t.total();
        let sender_handles = t
            .active_senders
            .get(sender)
            .map(|s| s.portal_handles.len())
            .unwrap_or(0);
        let sender_legacy = t
            .active_senders
            .get(sender)
            .map(|s| s.legacy_cookies.len())
            .unwrap_or(0);
        drop(t);

        eventline::debug!(
            "dbus: portal close deferred (browser source capture still active, sender={}, total={}, sender_handles={}, sender_legacy={}, handle={})",
            sender,
            total,
            sender_handles,
            sender_legacy,
            handle
        );
        return;
    }

    let mut t = tracker.lock().await;
    let old_total = t.total();
    let removed = t.clear_portal_handle(sender, handle);
    let new_total = t.total();
    let sender_handles = t
        .active_senders
        .get(sender)
        .map(|s| s.portal_handles.len())
        .unwrap_or(0);
    let sender_legacy = t
        .active_senders
        .get(sender)
        .map(|s| s.legacy_cookies.len())
        .unwrap_or(0);
    drop(t);

    if removed && old_total > 0 && new_total == 0 {
        eventline::debug!(
            "dbus: inhibit cleared (portal sender={}, handle={})",
            sender,
            handle
        );
        sink.push(Event::BrowserInactive { now_ms: now_ms() });
    } else if removed {
        eventline::debug!(
            "dbus: portal handle closed (sender={}, remaining_total={}, sender_handles={}, sender_legacy={}, handle={})",
            sender,
            new_total,
            sender_handles,
            sender_legacy,
            handle
        );
    } else {
        eventline::debug!(
            "dbus: portal handle close ignored (sender={}, handle={})",
            sender,
            handle
        );
    }
}

fn browser_source_capture_active_now() -> bool {
    let out = match Command::new("pactl")
        .args(["list", "source-outputs"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };
    parse_browser_stream_blocks(
        &String::from_utf8_lossy(&out.stdout),
        &["Source Output #", "SourceOutput #"],
        stream_block_is_browser,
    )
}

fn parse_browser_stream_blocks(
    text: &str,
    headers: &[&str],
    block_predicate: fn(&str) -> bool,
) -> bool {
    if text.trim().is_empty() {
        return false;
    }

    let mut block = String::new();
    let mut saw_header = false;

    for line in text.lines() {
        let trimmed = line.trim_start();
        let is_header = headers.iter().any(|h| trimmed.starts_with(h));

        if is_header {
            if saw_header && block_predicate(&block) {
                return true;
            }
            block.clear();
            saw_header = true;
        }

        if saw_header {
            block.push_str(line);
            block.push('\n');
        }
    }

    saw_header && block_predicate(&block)
}

fn browser_token_in_block(block_lc: &str) -> bool {
    [
        "firefox",
        "vivaldi",
        "chromium",
        "google-chrome",
        "google chrome",
        "brave",
        "librewolf",
        "waterfox",
        "zen browser",
        "zen-browser",
        "msedge",
        "microsoft-edge",
        "opera",
    ]
    .iter()
    .any(|token| block_lc.contains(token))
}

fn stream_block_is_browser(block: &str) -> bool {
    let b = block.to_ascii_lowercase();
    browser_token_in_block(&b)
}

async fn tracker_remove_sender(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sink: &Arc<dyn EventSink>,
    sender: &str,
) {
    let mut t = tracker.lock().await;
    let old_total = t.total();
    t.remove_sender(sender);
    let new_total = t.total();
    drop(t);

    if old_total > 0 && new_total == 0 {
        eventline::debug!(
            "dbus: inhibit cleared by sender disconnect (sender={})",
            sender
        );
        sink.push(Event::BrowserInactive { now_ms: now_ms() });
    }
}

async fn spawn_dbus_inhibit_monitor(sink: Arc<dyn EventSink>) -> zbus::Result<()> {
    let monitor = Connection::session().await?;
    monitor
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus.Monitoring"),
            "BecomeMonitor",
            &(&[] as &[&str], 0u32),
        )
        .await?;

    let tracker = Arc::new(Mutex::new(DbusInhibitTracker::default()));
    eventline::debug!("dbus: inhibit monitor started (session bus)");

    let mut stream = zbus::MessageStream::from(monitor);
    tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            let Ok(msg) = msg else { continue };

            let header = msg.header();
            let iface = header
                .interface()
                .map(|i| i.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let member = header
                .member()
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();

            match msg.message_type() {
                zbus::message::Type::MethodCall => {
                    let Some(sender) = header.sender() else {
                        continue;
                    };
                    let sender = sender.to_string();

                    let legacy_inhibit_call = (iface == "org.freedesktop.screensaver"
                        && member == "inhibit")
                        || (iface == "org.gnome.sessionmanager" && member == "inhibit");

                    if legacy_inhibit_call {
                        let serial = header.primary().serial_num().get();
                        tracker_register_legacy_call(&tracker, &sender, serial).await;
                        continue;
                    }

                    let legacy_uninhibit_call = (iface == "org.freedesktop.screensaver"
                        && member == "uninhibit")
                        || (iface == "org.gnome.sessionmanager" && member == "uninhibit");

                    if legacy_uninhibit_call {
                        let cookie: u32 = match msg.body().deserialize() {
                            Ok(v) => v,
                            Err(_) => {
                                eventline::debug!(
                                    "dbus: legacy uninhibit body parse failed (sender={}, iface={}, member={})",
                                    sender,
                                    iface,
                                    member
                                );
                                continue;
                            }
                        };

                        tracker_clear_legacy_cookie(&tracker, &sink, &sender, cookie).await;
                        continue;
                    }

                    let portal_inhibit_call =
                        iface == "org.freedesktop.portal.inhibit" && member == "inhibit";

                    if portal_inhibit_call {
                        let serial = header.primary().serial_num().get();
                        tracker_register_portal_call(&tracker, &sender, serial).await;
                        continue;
                    }

                    if iface == "org.freedesktop.portal.request" && member == "close" {
                        let Some(path) = header.path() else {
                            continue;
                        };
                        tracker_clear_portal_handle(&tracker, &sink, &sender, path.as_str()).await;
                    }
                }

                zbus::message::Type::MethodReturn => {
                    let Some(reply_serial) = header.reply_serial() else {
                        continue;
                    };
                    let Some(dest) = header.destination() else {
                        continue;
                    };

                    let sender = dest.as_str().to_string();
                    let reply_serial = reply_serial.get();

                    // Legacy APIs return a cookie (u32).
                    if let Ok(cookie) = msg.body().deserialize::<u32>() {
                        tracker_confirm_legacy_cookie(
                            &tracker,
                            &sink,
                            &sender,
                            reply_serial,
                            cookie,
                        )
                        .await;
                        continue;
                    }

                    // Portal Inhibit returns a request-handle object path.
                    if let Ok(handle) = msg.body().deserialize::<zbus::zvariant::OwnedObjectPath>()
                    {
                        tracker_confirm_portal_handle(
                            &tracker,
                            &sink,
                            &sender,
                            reply_serial,
                            handle.as_str(),
                        )
                        .await;
                        continue;
                    }
                }

                zbus::message::Type::Error => {
                    let Some(reply_serial) = header.reply_serial() else {
                        continue;
                    };
                    let Some(dest) = header.destination() else {
                        continue;
                    };
                    let sender = dest.as_str().to_string();
                    let serial = reply_serial.get();

                    tracker_clear_legacy_call(&tracker, &sender, serial).await;
                    tracker_clear_portal_call(&tracker, &sender, serial).await;
                }

                zbus::message::Type::Signal => {
                    if iface == "org.freedesktop.dbus" && member == "nameownerchanged" {
                        let parsed: (String, String, String) = match msg.body().deserialize() {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let (name, _old_owner, new_owner) = parsed;

                        if name.starts_with(':') && new_owner.is_empty() {
                            tracker_remove_sender(&tracker, &sink, &name).await;
                        }
                    }
                }
            }
        }
    });

    eventline::debug!("dbus: inhibit monitor subscriptions active");
    Ok(())
}

async fn run_dbus(
    sink: Arc<dyn EventSink>,
    enable_loginctl: bool,
    enable_dbus_inhibit: bool,
    mut shutdown: watch::Receiver<bool>,
) -> zbus::Result<()> {
    eventline::info!("dbus: monitor logic rev=legacy-cookie-v1");

    let sys = match Connection::system().await {
        Ok(c) => Some(c),
        Err(e) => {
            eventline::warn!("D-Bus: could not connect to system bus: {e:?}");
            None
        }
    };

    let session = match Connection::session().await {
        Ok(c) => Some(c),
        Err(e) => {
            eventline::warn!("D-Bus: could not connect to session bus: {e:?}");
            None
        }
    };

    if enable_dbus_inhibit {
        if session.is_some() {
            if let Err(e) = spawn_dbus_inhibit_monitor(sink.clone()).await {
                eventline::warn!("D-Bus: inhibit monitor unavailable on session bus: {e:?}");
            }
        } else {
            eventline::warn!("D-Bus: inhibit monitoring requested, but session bus is unavailable");
        }
    } else {
        eventline::info!("D-Bus: session inhibit monitoring disabled by config");
    }

    if let Some(sys) = sys.as_ref() {
        if enable_loginctl {
            match Proxy::new(
                sys,
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
            )
            .await
            {
                Ok(proxy) => match proxy.receive_signal("PrepareForSleep").await {
                    Ok(mut stream) => {
                        let sink = sink.clone();
                        tokio::spawn(async move {
                            while let Some(sig) = stream.next().await {
                                let going_down: bool = match sig.body().deserialize() {
                                    Ok(v) => v,
                                    Err(_) => continue,
                                };
                                let t = now_ms();
                                sink.push(if going_down {
                                    Event::PrepareForSleep { now_ms: t }
                                } else {
                                    Event::ResumedFromSleep { now_ms: t }
                                });
                            }
                        });
                    }
                    Err(e) => {
                        eventline::warn!("D-Bus: could not subscribe to PrepareForSleep: {e:?}");
                    }
                },
                Err(e) => {
                    eventline::warn!(
                        "D-Bus: login1 Manager proxy unavailable: {e:?}; sleep/wake monitoring disabled"
                    );
                }
            }
        } else {
            eventline::info!("D-Bus: login1 integration disabled; skipping sleep/wake monitoring");
        }

        // Always-on LockedHint watcher. Independent of `enable_loginctl_integration`.
        // Compositors that set logind's LockedHint property (e.g. a Quickshell fork
        // with the lockhint feature) are tracked here via PropertiesChanged.
        // Non-fatal: errors are logged and fall through to other monitoring.
        if let Ok(session_path) = get_current_session_path(sys).await {
            eventline::info!(
                "D-Bus: LockedHint watcher monitoring session {}",
                session_path.as_str()
            );

            if let Ok(proxy) = Proxy::new(
                sys,
                "org.freedesktop.login1",
                session_path.clone(),
                "org.freedesktop.login1.Session",
            )
            .await
            {
                match proxy.get_property::<bool>("LockedHint").await {
                    Ok(locked) => sink.push(if locked {
                        Event::SessionLocked {
                            source: LockSource::LockedHint,
                            now_ms: now_ms(),
                        }
                    } else {
                        Event::SessionUnlocked {
                            source: LockSource::LockedHint,
                            now_ms: now_ms(),
                        }
                    }),
                    Err(e) => {
                        eventline::warn!("D-Bus: could not read initial LockedHint: {e:?}");
                    }
                }
            }

            let rule = MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.DBus.Properties")
                .and_then(|b| b.member("PropertiesChanged"))
                .and_then(|b| b.path(session_path.clone()))
                .map(|b| b.build());

            if let Ok(rule) = rule {
                match zbus::MessageStream::for_match_rule(rule, sys, None).await {
                    Ok(mut stream) => {
                        let sink_lockedhint = sink.clone();
                        tokio::spawn(async move {
                            use zbus::zvariant::Value;

                            while let Some(msg) = stream.next().await {
                                let Ok(msg) = msg else { continue };

                                let body = msg.body();
                                let parsed: (String, HashMap<String, Value>, Vec<String>) =
                                    match body.deserialize() {
                                        Ok(v) => v,
                                        Err(_) => continue,
                                    };

                                let (iface, changed, _invalidated) = parsed;

                                if iface != "org.freedesktop.login1.Session" {
                                    continue;
                                }

                                if let Some(v) = changed.get("LockedHint") {
                                    if let Ok(locked) = v.clone().downcast::<bool>() {
                                        let t = now_ms();
                                        sink_lockedhint.push(if locked {
                                            Event::SessionLocked {
                                                source: LockSource::LockedHint,
                                                now_ms: t,
                                            }
                                        } else {
                                            Event::SessionUnlocked {
                                                source: LockSource::LockedHint,
                                                now_ms: t,
                                            }
                                        });
                                    }
                                }
                            }
                        });
                    }
                    Err(e) => {
                        eventline::warn!(
                            "D-Bus: could not subscribe to LockedHint PropertiesChanged: {e:?}"
                        );
                    }
                }
            }
        }

        {
            match Proxy::new(
                sys,
                "org.freedesktop.UPower",
                "/org/freedesktop/UPower",
                "org.freedesktop.UPower",
            )
            .await
            {
                Ok(proxy) => {
                    // Force service activation up front; a raw signal match rule alone
                    // does not guarantee UPower is started yet.
                    if let Err(e) = proxy.get_property::<bool>("LidIsPresent").await {
                        eventline::warn!("D-Bus: could not read initial UPower lid state: {e:?}");
                    }
                }
                Err(e) => {
                    eventline::warn!("D-Bus: could not create UPower proxy: {e:?}");
                }
            }

            let rule = MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.DBus.Properties")?
                .member("PropertiesChanged")?
                .path("/org/freedesktop/UPower")?
                .build();

            let mut stream = zbus::MessageStream::for_match_rule(rule, sys, None).await?;
            let sink = sink.clone();

            tokio::spawn(async move {
                use zbus::zvariant::Value;

                while let Some(msg) = stream.next().await {
                    let Ok(msg) = msg else { continue };

                    let body = msg.body();
                    let parsed: (String, HashMap<String, Value>, Vec<String>) =
                        match body.deserialize() {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                    let (iface, changed, _invalidated) = parsed;

                    if iface != "org.freedesktop.UPower" {
                        continue;
                    }

                    if let Some(v) = changed.get("LidIsClosed") {
                        if let Ok(closed) = v.clone().downcast::<bool>() {
                            let t = now_ms();
                            sink.push(if closed {
                                Event::LidClosed { now_ms: t }
                            } else {
                                Event::LidOpened { now_ms: t }
                            });
                        }
                    }
                }
            });
        }
    } else {
        eventline::warn!("D-Bus: system bus unavailable; login1/lid monitoring disabled");
    }

    loop {
        if *shutdown.borrow() {
            break;
        }
        let _ = shutdown.changed().await;
        if *shutdown.borrow() {
            break;
        }
    }

    Ok(())
}

// ---- Session path resolution ----

async fn get_current_session_path(
    connection: &Connection,
) -> zbus::Result<zbus::zvariant::OwnedObjectPath> {
    let proxy = Proxy::new(
        connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;

    if let Ok(session_id) = std::env::var("XDG_SESSION_ID") {
        let result: zbus::Result<zbus::zvariant::OwnedObjectPath> =
            proxy.call("GetSession", &(session_id.as_str(),)).await;

        if let Ok(path) = result {
            eventline::debug!("D-Bus: using session from XDG_SESSION_ID");
            return Ok(path);
        }
    }

    let uid: u32 = rustix::process::getuid().as_raw();

    let sessions: Vec<(String, u32, String, String, zbus::zvariant::OwnedObjectPath)> =
        proxy.call("ListSessions", &()).await?;

    for (session_id, session_uid, _username, seat, path) in sessions.clone() {
        if session_uid != uid {
            continue;
        }

        if let Ok(sproxy) = Proxy::new(
            connection,
            "org.freedesktop.login1",
            path.clone(),
            "org.freedesktop.login1.Session",
        )
        .await
        {
            if let Ok(session_type) = sproxy.get_property::<String>("Type").await {
                if (session_type == "wayland" || session_type == "x11") && seat == "seat0" {
                    eventline::info!(
                        "D-Bus: selected graphical session '{}' (type: {}, seat: {})",
                        session_id,
                        session_type,
                        seat
                    );
                    return Ok(path);
                }
            }
        }
    }

    for (_session_id, session_uid, _username, _seat, path) in sessions {
        if session_uid == uid {
            eventline::warn!("D-Bus: using first session for UID {}", uid);
            return Ok(path);
        }
    }

    let pid = std::process::id();
    let path: zbus::zvariant::OwnedObjectPath = proxy.call("GetSessionByPID", &(pid,)).await?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{DbusInhibitTracker, stream_block_is_browser};

    #[test]
    fn portal_inhibit_stays_active_until_explicit_clear() {
        let mut tracker = DbusInhibitTracker::default();
        tracker.mark_portal_call(":1.26", 100);
        tracker.confirm_portal_handle(
            ":1.26",
            100,
            "/org/freedesktop/portal/desktop/request/1_26/t/abc",
        );
        assert_eq!(tracker.total(), 1);

        tracker.mark_portal_call(":1.26", 101);
        tracker.confirm_portal_handle(
            ":1.26",
            101,
            "/org/freedesktop/portal/desktop/request/1_26/t/def",
        );
        assert_eq!(tracker.total(), 1);

        tracker.clear_portal_handle(
            ":1.26",
            "/org/freedesktop/portal/desktop/request/1_26/t/abc",
        );
        assert_eq!(tracker.total(), 1);

        tracker.clear_portal_handle(
            ":1.26",
            "/org/freedesktop/portal/desktop/request/1_26/t/def",
        );
        assert_eq!(tracker.total(), 0);
    }

    #[test]
    fn sender_removed_when_both_legacy_and_portal_clear() {
        let mut tracker = DbusInhibitTracker::default();

        tracker.mark_legacy_active(":1.99");
        tracker.mark_portal_active(":1.99");
        assert_eq!(tracker.total(), 1);

        tracker.clear_portal(":1.99");
        assert_eq!(tracker.total(), 1);

        tracker.clear_legacy(":1.99");
        assert_eq!(tracker.total(), 0);
    }

    #[test]
    fn browser_block_matches_firefox_properties() {
        let block = r#"
Properties:
    application.name = "Firefox"
"#;
        assert!(stream_block_is_browser(block));
    }

    #[test]
    fn browser_block_ignores_non_browser_properties() {
        let block = r#"
Properties:
    application.name = "zoom"
"#;
        assert!(!stream_block_is_browser(block));
    }
}
