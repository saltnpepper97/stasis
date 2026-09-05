// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use tokio::sync::{Mutex, watch};
use zbus::{Connection, MatchRule, Proxy};

use crate::core::{
    blame::{DbusHold, Login1IdleHold},
    events::{Event, LockSource},
};

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
/// `enable_dbus_inhibit` gates inhibit monitoring:
/// - org.freedesktop.ScreenSaver Inhibit/UnInhibit
/// - org.gnome.SessionManager Inhibit/Uninhibit
/// - org.freedesktop.portal.Inhibit Inhibit + Request.Close
///
/// It also gates system-bus login1 blocking `idle` inhibitors. Those holds
/// block only Stasis's automatic suspend step; earlier lock and DPMS actions
/// continue to follow the configured plan.
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
    pending_legacy_calls: HashMap<String, HashMap<u32, PendingHold>>,

    // Pending portal Inhibit method-call serials per sender, waiting for
    // method-return that carries the request handle.
    pending_portal_calls: HashMap<String, HashMap<u32, PendingHold>>,

    // Independent browser source-capture observation. This participates in
    // browser gating but never changes the lifecycle of a D-Bus request.
    browser_source_capture_active: bool,
}

#[derive(Debug, Default)]
struct SenderInhibitState {
    // Legacy ScreenSaver / SessionManager inhibit cookies active for this sender.
    legacy_cookies: HashMap<u32, DbusHold>,

    // Portal Request handles currently active for this sender.
    portal_handles: HashMap<String, DbusHold>,
}

#[derive(Debug, Clone)]
struct PendingHold {
    protocol: String,
    source: String,
    application: Option<String>,
    process: Option<String>,
    pid: Option<u32>,
    reason: Option<String>,
    flags: Option<u32>,
    started_at_ms: u64,
}

type Login1InhibitorRow = (String, String, String, String, u32, u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Login1InhibitorKey {
    what: String,
    who: String,
    why: String,
    mode: String,
    uid: u32,
    pid: u32,
}

#[derive(Debug, Default)]
struct Login1IdleTracker {
    first_seen_ms: HashMap<Login1InhibitorKey, u64>,
}

impl Login1IdleTracker {
    fn reconcile(&mut self, rows: Vec<Login1InhibitorRow>, now_ms: u64) -> Vec<Login1IdleHold> {
        let mut active = rows
            .into_iter()
            .filter_map(|(what, who, why, mode, uid, pid)| {
                let blocks_idle = what.split(':').any(|scope| scope == "idle");
                if !blocks_idle || !mode.eq_ignore_ascii_case("block") {
                    return None;
                }
                Some(Login1InhibitorKey {
                    what,
                    who,
                    why,
                    mode,
                    uid,
                    pid,
                })
            })
            .collect::<Vec<_>>();

        active.sort_by(|a, b| {
            a.who
                .cmp(&b.who)
                .then_with(|| a.pid.cmp(&b.pid))
                .then_with(|| a.why.cmp(&b.why))
                .then_with(|| a.what.cmp(&b.what))
                .then_with(|| a.mode.cmp(&b.mode))
                .then_with(|| a.uid.cmp(&b.uid))
        });
        let active_set = active.iter().cloned().collect::<HashSet<_>>();
        self.first_seen_ms.retain(|key, _| active_set.contains(key));

        active
            .into_iter()
            .map(|key| {
                let started_at_ms = *self.first_seen_ms.entry(key.clone()).or_insert(now_ms);
                Login1IdleHold {
                    status: "live".to_string(),
                    what: key.what,
                    who: key.who,
                    why: key.why,
                    mode: key.mode,
                    uid: key.uid,
                    pid: key.pid,
                    process: process_name_for_pid(key.pid),
                    started_at_ms,
                    age_ms: 0,
                }
            })
            .collect()
    }
}

impl PendingHold {
    fn into_hold(self, sender: &str, cookie: Option<u32>, handle: Option<String>) -> DbusHold {
        let resolved = self.application.is_some() || self.process.is_some();
        DbusHold {
            status: if resolved { "live" } else { "live-unresolved" }.to_string(),
            protocol: self.protocol,
            source: self.source,
            sender: sender.to_string(),
            application: self.application,
            process: self.process,
            pid: self.pid,
            reason: self.reason,
            flags: self.flags,
            started_at_ms: self.started_at_ms,
            age_ms: 0,
            cookie,
            handle,
        }
    }
}

#[cfg(test)]
fn test_hold(sender: &str, cookie: Option<u32>, handle: Option<String>) -> DbusHold {
    DbusHold {
        status: "live".to_string(),
        protocol: "test".to_string(),
        source: "test".to_string(),
        sender: sender.to_string(),
        application: Some("test app".to_string()),
        process: Some("test".to_string()),
        pid: Some(1),
        reason: Some("test".to_string()),
        flags: None,
        started_at_ms: 1,
        age_ms: 0,
        cookie,
        handle,
    }
}

impl DbusInhibitTracker {
    fn total(&self) -> usize {
        self.active_senders.len()
    }

    fn blocks_idle(&self) -> bool {
        !self.active_senders.is_empty() || self.browser_source_capture_active
    }

    fn holds(&self) -> Vec<DbusHold> {
        let mut holds: Vec<_> = self
            .active_senders
            .values()
            .flat_map(|state| {
                state
                    .legacy_cookies
                    .values()
                    .chain(state.portal_handles.values())
            })
            .cloned()
            .collect();
        holds.sort_by(|a, b| {
            a.started_at_ms
                .cmp(&b.started_at_ms)
                .then_with(|| a.sender.cmp(&b.sender))
                .then_with(|| a.cookie.cmp(&b.cookie))
                .then_with(|| a.handle.cmp(&b.handle))
        });
        holds
    }

    fn mark_legacy_call(&mut self, sender: &str, serial: u32, pending: PendingHold) {
        self.pending_legacy_calls
            .entry(sender.to_string())
            .or_default()
            .insert(serial, pending);
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
        let pending = self
            .pending_legacy_calls
            .get_mut(sender)
            .and_then(|calls| calls.remove(&reply_serial));
        let Some(pending) = pending else { return false };
        if self
            .pending_legacy_calls
            .get(sender)
            .is_some_and(HashMap::is_empty)
        {
            self.pending_legacy_calls.remove(sender);
        }
        let state = self.active_senders.entry(sender.to_string()).or_default();
        state
            .legacy_cookies
            .insert(cookie, pending.into_hold(sender, Some(cookie), None))
            .is_none()
    }

    fn clear_legacy_cookie(&mut self, sender: &str, cookie: u32) -> bool {
        let removed = if let Some(state) = self.active_senders.get_mut(sender) {
            state.legacy_cookies.remove(&cookie).is_some()
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
        state
            .legacy_cookies
            .insert(0, test_hold(sender, Some(0), None));
    }

    #[cfg(test)]
    fn clear_legacy(&mut self, sender: &str) {
        let removed = if let Some(state) = self.active_senders.get_mut(sender) {
            state.legacy_cookies.remove(&0).is_some()
        } else {
            false
        };

        if removed {
            self.drop_sender_if_empty(sender);
        }
    }

    fn mark_portal_call(&mut self, sender: &str, serial: u32, pending: PendingHold) {
        self.pending_portal_calls
            .entry(sender.to_string())
            .or_default()
            .insert(serial, pending);
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
        let pending = self
            .pending_portal_calls
            .get_mut(sender)
            .and_then(|calls| calls.remove(&reply_serial));
        let Some(pending) = pending else { return false };
        if self
            .pending_portal_calls
            .get(sender)
            .is_some_and(HashMap::is_empty)
        {
            self.pending_portal_calls.remove(sender);
        }
        let state = self.active_senders.entry(sender.to_string()).or_default();
        state
            .portal_handles
            .insert(
                handle.to_string(),
                pending.into_hold(sender, None, Some(handle.to_string())),
            )
            .is_none()
    }

    fn clear_portal_handle(&mut self, sender: &str, handle: &str) -> bool {
        let removed = if let Some(state) = self.active_senders.get_mut(sender) {
            state.portal_handles.remove(handle).is_some()
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
        let state = self.active_senders.entry(sender.to_string()).or_default();
        let handle = format!("/test/{}", sender.trim_start_matches(':'));
        state
            .portal_handles
            .insert(handle.clone(), test_hold(sender, None, Some(handle)));
    }

    #[cfg(test)]
    fn clear_portal(&mut self, sender: &str) {
        let _ = self.clear_any_portal_handle(sender);
    }

    #[cfg(test)]
    fn clear_any_portal_handle(&mut self, sender: &str) -> bool {
        let removed = if let Some(state) = self.active_senders.get_mut(sender) {
            if let Some(h) = state.portal_handles.keys().next().cloned() {
                state.portal_handles.remove(&h).is_some()
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
    pending: PendingHold,
) {
    let mut t = tracker.lock().await;
    t.mark_legacy_call(sender, serial, pending);
}

fn push_tracker_snapshot(t: &DbusInhibitTracker, sink: &Arc<dyn EventSink>) {
    sink.push(Event::DbusInhibitorsChanged {
        holds: t.holds(),
        now_ms: now_ms(),
    });
}

async fn tracker_confirm_legacy_cookie(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sink: &Arc<dyn EventSink>,
    sender: &str,
    reply_serial: u32,
    cookie: u32,
) {
    let mut t = tracker.lock().await;
    let was_blocking = t.blocks_idle();
    let newly_inserted = t.confirm_legacy_cookie(sender, reply_serial, cookie);
    let new_total = t.total();
    let is_blocking = t.blocks_idle();
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
    let snapshot_changed = newly_inserted;
    if snapshot_changed {
        push_tracker_snapshot(&t, sink);
    }
    drop(t);

    if newly_inserted && !was_blocking && is_blocking {
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
    let was_blocking = t.blocks_idle();
    let removed = t.clear_legacy_cookie(sender, cookie);
    let new_total = t.total();
    let is_blocking = t.blocks_idle();
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
    if removed {
        push_tracker_snapshot(&t, sink);
    }
    drop(t);

    if removed && was_blocking && !is_blocking {
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
    pending: PendingHold,
) {
    let mut t = tracker.lock().await;
    t.mark_portal_call(sender, serial, pending);
}

async fn tracker_confirm_portal_handle(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sink: &Arc<dyn EventSink>,
    sender: &str,
    reply_serial: u32,
    handle: &str,
) {
    let mut t = tracker.lock().await;
    let was_blocking = t.blocks_idle();
    let newly_inserted = t.confirm_portal_handle(sender, reply_serial, handle);
    let new_total = t.total();
    let is_blocking = t.blocks_idle();
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
    if newly_inserted {
        push_tracker_snapshot(&t, sink);
    }
    drop(t);

    if newly_inserted && !was_blocking && is_blocking {
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
    let mut t = tracker.lock().await;
    let was_blocking = t.blocks_idle();
    let removed = t.clear_portal_handle(sender, handle);
    let new_total = t.total();
    let is_blocking = t.blocks_idle();
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
    if removed {
        push_tracker_snapshot(&t, sink);
    }
    drop(t);

    if removed && was_blocking && !is_blocking {
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

async fn tracker_set_source_capture(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sink: &Arc<dyn EventSink>,
    active: bool,
) {
    let mut t = tracker.lock().await;
    if t.browser_source_capture_active == active {
        return;
    }

    let was_blocking = t.blocks_idle();
    t.browser_source_capture_active = active;
    let is_blocking = t.blocks_idle();
    drop(t);

    eventline::debug!("dbus: browser source capture active={active}");
    sink.push(Event::BrowserSourceCaptureChanged {
        active,
        now_ms: now_ms(),
    });
    if !was_blocking && is_blocking {
        sink.push(Event::BrowserActivity { now_ms: now_ms() });
    } else if was_blocking && !is_blocking {
        sink.push(Event::BrowserInactive { now_ms: now_ms() });
    }
}

fn browser_source_capture_active_now() -> Option<bool> {
    let out = match Command::new("pactl")
        .args(["list", "source-outputs"])
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return None,
    };
    Some(parse_browser_stream_blocks(
        &String::from_utf8_lossy(&out.stdout),
        &["Source Output #", "SourceOutput #"],
        stream_block_is_browser,
    ))
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
        let is_header = headers.iter().any(|header| trimmed.starts_with(header));

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

fn stream_block_is_browser(block: &str) -> bool {
    let block = block.to_ascii_lowercase();
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
    .any(|token| block.contains(token))
}

fn spawn_source_capture_monitor(tracker: Arc<Mutex<DbusInhibitTracker>>, sink: Arc<dyn EventSink>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Ok(Some(active)) =
                tokio::task::spawn_blocking(browser_source_capture_active_now).await
            {
                tracker_set_source_capture(&tracker, &sink, active).await;
            }
        }
    });
}

async fn list_login1_inhibitors(proxy: &Proxy<'_>) -> zbus::Result<Vec<Login1InhibitorRow>> {
    let reply = proxy.call_method("ListInhibitors", &()).await?;
    reply.body().deserialize()
}

fn spawn_login1_idle_inhibit_monitor(connection: Connection, sink: Arc<dyn EventSink>) {
    tokio::spawn(async move {
        let proxy = match Proxy::new(
            &connection,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )
        .await
        {
            Ok(proxy) => proxy,
            Err(error) => {
                eventline::warn!("D-Bus: login1 idle inhibit monitor unavailable: {error:?}");
                return;
            }
        };

        let mut tracker = Login1IdleTracker::default();
        let mut published = Vec::new();
        let mut unavailable_logged = false;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;
            match list_login1_inhibitors(&proxy).await {
                Ok(rows) => {
                    unavailable_logged = false;
                    let holds = tracker.reconcile(rows, now_ms());
                    if holds == published {
                        continue;
                    }

                    eventline::info!(
                        "D-Bus: login1 blocking idle inhibitors changed: {} -> {}",
                        published.len(),
                        holds.len()
                    );
                    published = holds.clone();
                    sink.push(Event::Login1IdleInhibitorsChanged {
                        holds,
                        now_ms: now_ms(),
                    });
                }
                Err(error) if !unavailable_logged => {
                    unavailable_logged = true;
                    eventline::warn!(
                        "D-Bus: could not query login1 idle inhibitors; preserving last known state: {error:?}"
                    );
                }
                Err(_) => {}
            }
        }
    });
}

async fn tracker_remove_sender(
    tracker: &Arc<Mutex<DbusInhibitTracker>>,
    sink: &Arc<dyn EventSink>,
    sender: &str,
) {
    let mut t = tracker.lock().await;
    let old_total = t.total();
    let was_blocking = t.blocks_idle();
    t.remove_sender(sender);
    let new_total = t.total();
    let is_blocking = t.blocks_idle();
    if old_total != new_total {
        push_tracker_snapshot(&t, sink);
    }
    drop(t);

    if was_blocking && !is_blocking {
        eventline::debug!(
            "dbus: inhibit cleared by sender disconnect (sender={})",
            sender
        );
        sink.push(Event::BrowserInactive { now_ms: now_ms() });
    }
}

async fn resolve_sender_identity(
    connection: &Connection,
    sender: &str,
) -> (Option<u32>, Option<String>) {
    let reply = connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixProcessID",
            &(sender,),
        )
        .await;

    let Ok(reply) = reply else {
        return (None, None);
    };
    let Ok(pid) = reply.body().deserialize::<u32>() else {
        return (None, None);
    };

    (Some(pid), process_name_for_pid(pid))
}

fn process_name_for_pid(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
        })
}

fn pending_hold(
    protocol: &str,
    application: Option<String>,
    reason: Option<String>,
    flags: Option<u32>,
    identity: (Option<u32>, Option<String>),
) -> PendingHold {
    PendingHold {
        protocol: protocol.to_string(),
        source: if protocol == "org.freedesktop.portal.Inhibit" {
            "portal-request"
        } else {
            "legacy-cookie"
        }
        .to_string(),
        application: clean_metadata(application, 80),
        process: identity.1,
        pid: identity.0,
        reason: clean_metadata(reason, 160),
        flags,
        started_at_ms: now_ms(),
    }
}

fn clean_metadata(value: Option<String>, max_chars: usize) -> Option<String> {
    let value = value?;
    let cleaned: String = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn portal_option_string(
    options: &HashMap<String, zbus::zvariant::OwnedValue>,
    key: &str,
) -> Option<String> {
    options
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| String::try_from(value).ok())
        .filter(|value| !value.trim().is_empty())
}

async fn spawn_dbus_inhibit_monitor(sink: Arc<dyn EventSink>) -> zbus::Result<()> {
    eventline::debug!("dbus: connecting inhibit monitor");
    let monitor = Connection::session().await?;
    eventline::debug!("dbus: connecting sender resolver");
    let resolver = Connection::session().await?;
    eventline::debug!("dbus: requesting monitor mode");
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
    spawn_source_capture_monitor(tracker.clone(), sink.clone());

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
                        let identity = resolve_sender_identity(&resolver, &sender).await;
                        let pending = if iface == "org.freedesktop.screensaver" {
                            let parsed = msg.body().deserialize::<(String, String)>();
                            let (application, reason) = parsed.unwrap_or_default();
                            pending_hold(
                                "org.freedesktop.ScreenSaver",
                                Some(application),
                                Some(reason),
                                None,
                                identity,
                            )
                        } else {
                            let parsed = msg.body().deserialize::<(String, u32, String, u32)>();
                            let (application, _window, reason, flags) = parsed.unwrap_or_default();
                            pending_hold(
                                "org.gnome.SessionManager",
                                Some(application),
                                Some(reason),
                                Some(flags),
                                identity,
                            )
                        };
                        tracker_register_legacy_call(&tracker, &sender, serial, pending).await;
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
                        let identity = resolve_sender_identity(&resolver, &sender).await;
                        let parsed = msg.body().deserialize::<(
                            String,
                            u32,
                            HashMap<String, zbus::zvariant::OwnedValue>,
                        )>();
                        let (_window, flags, options) = parsed.unwrap_or_default();
                        let application = portal_option_string(&options, "app_id")
                            .or_else(|| portal_option_string(&options, "application"));
                        let reason = portal_option_string(&options, "reason");
                        let pending = pending_hold(
                            "org.freedesktop.portal.Inhibit",
                            application,
                            reason,
                            Some(flags),
                            identity,
                        );
                        tracker_register_portal_call(&tracker, &sender, serial, pending).await;
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
    eventline::info!("dbus: monitor logic rev=hold-metadata-v2");

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
        eventline::info!("D-Bus: inhibit monitoring disabled by config");
    }

    if let Some(sys) = sys.as_ref() {
        if enable_dbus_inhibit {
            spawn_login1_idle_inhibit_monitor(sys.clone(), sink.clone());
            eventline::info!("D-Bus: login1 blocking idle inhibitors map to suspend-only holds");
        }

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
    use super::{DbusInhibitTracker, Login1IdleTracker, PendingHold, stream_block_is_browser};

    fn pending(protocol: &str) -> PendingHold {
        PendingHold {
            protocol: protocol.to_string(),
            source: "test".to_string(),
            application: Some("Firefox".to_string()),
            process: Some("firefox".to_string()),
            pid: Some(42),
            reason: Some("video playback".to_string()),
            flags: Some(8),
            started_at_ms: 100,
        }
    }

    #[test]
    fn portal_inhibit_stays_active_until_explicit_clear() {
        let mut tracker = DbusInhibitTracker::default();
        tracker.mark_portal_call(":1.26", 100, pending("org.freedesktop.portal.Inhibit"));
        tracker.confirm_portal_handle(
            ":1.26",
            100,
            "/org/freedesktop/portal/desktop/request/1_26/t/abc",
        );
        assert_eq!(tracker.total(), 1);

        tracker.mark_portal_call(":1.26", 101, pending("org.freedesktop.portal.Inhibit"));
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
        assert!(tracker.holds().is_empty());
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
    fn closing_portal_hold_does_not_clear_live_legacy_hold() {
        let mut tracker = DbusInhibitTracker::default();
        tracker.mark_portal_call(":1.26", 100, pending("org.freedesktop.portal.Inhibit"));
        tracker.confirm_portal_handle(":1.26", 100, "/request/firefox");
        tracker.mark_legacy_call(":1.108", 101, pending("org.freedesktop.ScreenSaver"));
        tracker.confirm_legacy_cookie(":1.108", 101, 77);

        assert_eq!(tracker.holds().len(), 2);
        assert!(tracker.clear_portal_handle(":1.26", "/request/firefox"));

        let holds = tracker.holds();
        assert_eq!(holds.len(), 1);
        assert_eq!(holds[0].cookie, Some(77));
        assert_eq!(holds[0].protocol, "org.freedesktop.ScreenSaver");
    }

    #[test]
    fn closed_portal_hold_is_removed_while_source_capture_remains_independent() {
        let mut tracker = DbusInhibitTracker {
            browser_source_capture_active: true,
            ..Default::default()
        };
        tracker.mark_portal_call(":1.26", 100, pending("org.freedesktop.portal.Inhibit"));
        tracker.confirm_portal_handle(":1.26", 100, "/request/firefox");

        assert!(tracker.clear_portal_handle(":1.26", "/request/firefox"));
        assert!(tracker.holds().is_empty());
        assert_eq!(tracker.total(), 0);
        assert!(tracker.blocks_idle());
    }

    #[test]
    fn browser_source_capture_detection_uses_stream_identity() {
        let block = r#"
Properties:
    application.name = "Firefox"
"#;
        assert!(stream_block_is_browser(block));
        assert!(!stream_block_is_browser(
            "Properties:\n    application.name = \"Zoom\""
        ));
    }

    #[test]
    fn login1_tracker_keeps_only_blocking_idle_holds_and_cleans_up() {
        let codex = (
            "idle".to_string(),
            "codex".to_string(),
            "active turn".to_string(),
            "block".to_string(),
            1000,
            u32::MAX,
        );
        let rows = vec![
            codex.clone(),
            (
                "sleep".to_string(),
                "upower".to_string(),
                "polling".to_string(),
                "delay".to_string(),
                0,
                1,
            ),
            (
                "idle:sleep".to_string(),
                "backup".to_string(),
                "working".to_string(),
                "block".to_string(),
                1000,
                u32::MAX - 1,
            ),
        ];
        let mut tracker = Login1IdleTracker::default();

        let first = tracker.reconcile(rows.clone(), 1_000);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].who, "backup");
        assert_eq!(first[1].who, "codex");
        assert_eq!(first[1].started_at_ms, 1_000);

        let unchanged = tracker.reconcile(rows, 5_000);
        assert_eq!(unchanged[1].started_at_ms, 1_000);

        assert!(tracker.reconcile(Vec::new(), 6_000).is_empty());
        let restarted = tracker.reconcile(vec![codex], 7_000);
        assert_eq!(restarted[0].started_at_ms, 7_000);
    }
}
