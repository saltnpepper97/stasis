// Author: Dustin Pilgrim
// License: GPL-3.0-only

use rustix::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{wl_registry, wl_seat::WlSeat},
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{Event as IdleEvent, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};

use crate::core::events::{ActivityKind, Event};
use crate::core::manager_msg::ManagerMsg;

const MAX_IDLE_NOTIFIER_VERSION: u32 = 2;

fn supported_idle_notifier_version(advertised: u32) -> u32 {
    advertised.min(MAX_IDLE_NOTIFIER_VERSION)
}

#[derive(Debug)]
pub enum WaylandError {
    Connect(String),
    Roundtrip(String),
}

impl std::fmt::Display for WaylandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaylandError::Connect(s) => write!(f, "wayland connect failed: {s}"),
            WaylandError::Roundtrip(s) => write!(f, "wayland roundtrip failed: {s}"),
        }
    }
}

impl std::error::Error for WaylandError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleNotificationKind {
    InhibitorAware,
    InputOnly,
}

struct WaylandState {
    tx: mpsc::Sender<ManagerMsg>,

    idle_notifier: Option<ExtIdleNotifierV1>,
    idle_notifier_version: u32,
    seat: Option<WlSeat>,
    idle_notification: Option<ExtIdleNotificationV1>,
    input_idle_notification: Option<ExtIdleNotificationV1>,

    idle_timeout_ms: u32,
}

impl WaylandState {
    fn new(tx: mpsc::Sender<ManagerMsg>, idle_timeout_ms: u32) -> Self {
        Self {
            tx,
            idle_notifier: None,
            idle_notifier_version: 0,
            seat: None,
            idle_notification: None,
            input_idle_notification: None,
            idle_timeout_ms,
        }
    }

    fn emit_activity(&self) {
        let now_ms = crate::core::utils::now_ms();
        let _ = self.tx.try_send(ManagerMsg::Event(Event::UserActivity {
            kind: ActivityKind::Any,
            now_ms,
        }));
    }

    fn emit_compositor_idled(&self) {
        let now_ms = crate::core::utils::now_ms();
        let _ = self
            .tx
            .try_send(ManagerMsg::Event(Event::CompositorIdled { now_ms }));
    }

    fn emit_compositor_resumed(&self) {
        let now_ms = crate::core::utils::now_ms();
        let _ = self
            .tx
            .try_send(ManagerMsg::Event(Event::CompositorResumed { now_ms }));
    }

    fn handle_idle_event(&self, kind: IdleNotificationKind, event: IdleEvent) {
        match (kind, event) {
            (IdleNotificationKind::InhibitorAware, IdleEvent::Idled) => {
                self.emit_compositor_idled();
            }
            (IdleNotificationKind::InhibitorAware, IdleEvent::Resumed) => {
                self.emit_compositor_resumed();
            }
            (IdleNotificationKind::InputOnly, IdleEvent::Resumed) => {
                self.emit_activity();
            }
            (IdleNotificationKind::InputOnly, IdleEvent::Idled) => {}
            _ => {}
        }
    }
}

// ---------------- Registry binding ----------------

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "ext_idle_notifier_v1" => {
                    state.idle_notifier_version = supported_idle_notifier_version(version);
                    state.idle_notifier = Some(registry.bind::<ExtIdleNotifierV1, _, _>(
                        name,
                        state.idle_notifier_version,
                        qh,
                        (),
                    ));
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // no-op: ExtIdleNotifierV1 is a factory/manager object.
        // Events are on ExtIdleNotificationV1.
    }
}

impl Dispatch<WlSeat, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // no-op: the seat is only needed by ext-idle-notify.
    }
}

// ---------------- Idle notifications ----------------

impl Dispatch<ExtIdleNotificationV1, IdleNotificationKind> for WaylandState {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: IdleEvent,
        kind: &IdleNotificationKind,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.handle_idle_event(*kind, event);
    }
}

/// Poll the Wayland connection fd with a timeout (milliseconds).
/// Returns true if data is available, false on timeout.
fn poll_wayland_fd(fd: std::os::unix::io::RawFd, timeout_ms: i32) -> Result<bool, String> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::io::Errno;

    // Match poll(2) semantics: negative timeout means "infinite".
    let timeout_ts: Option<Timespec> = if timeout_ms < 0 {
        None
    } else {
        Some(
            Timespec::try_from(Duration::from_millis(timeout_ms as u64))
                .map_err(|_| "poll failed: invalid timeout".to_string())?,
        )
    };

    // SAFETY: We only use this borrowed fd for the duration of this call.
    let bfd = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut fds = [PollFd::from_borrowed_fd(bfd, PollFlags::IN)];

    match poll(&mut fds, timeout_ts.as_ref()) {
        Ok(0) => Ok(false),
        Ok(_) => Ok(true),
        Err(Errno::INTR) => Ok(false),
        Err(e) => Err(format!("poll failed: {e}")),
    }
}

/// Spawnable Wayland service.
pub async fn run_wayland(
    tx: mpsc::Sender<ManagerMsg>,
    mut shutdown: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<(), WaylandError> {
    let idle_timeout_ms: u32 = 250;

    eventline::info!("wayland: starting (idle_timeout_ms={})", idle_timeout_ms);

    let conn = Connection::connect_to_env().map_err(|e| WaylandError::Connect(e.to_string()))?;
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let display = conn.display();

    let mut state = WaylandState::new(tx, idle_timeout_ms);

    let _registry = display.get_registry(&qh, ());
    event_queue
        .roundtrip(&mut state)
        .map_err(|e| WaylandError::Roundtrip(e.to_string()))?;

    if let (Some(notifier), Some(seat)) = (state.idle_notifier.clone(), state.seat.clone()) {
        state.idle_notification = Some(notifier.get_idle_notification(
            state.idle_timeout_ms,
            &seat,
            &qh,
            IdleNotificationKind::InhibitorAware,
        ));

        if state.idle_notifier_version >= 2 {
            state.input_idle_notification = Some(notifier.get_input_idle_notification(
                state.idle_timeout_ms,
                &seat,
                &qh,
                IdleNotificationKind::InputOnly,
            ));
            eventline::info!("wayland: ext_idle_notifier_v1 v2 active with input tracking");
        } else {
            eventline::info!("wayland: ext_idle_notifier_v1 v1 active (safe fallback)");
        }
    } else {
        eventline::warn!(
            "wayland: ext_idle_notifier_v1 or wl_seat missing; idle notifier disabled"
        );
    }

    let stop = Arc::new(AtomicBool::new(false));
    let stop_dispatch = Arc::clone(&stop);

    tokio::spawn(async move {
        loop {
            if *shutdown.borrow() {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            if shutdown.changed().await.is_err() {
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }
    });

    let wayland_fd = {
        use std::os::unix::io::AsRawFd;
        conn.as_fd().as_raw_fd()
    };

    // Returns true if the compositor went away, false if we stopped cleanly.
    let join = tokio::task::spawn_blocking(move || {
        const POLL_TIMEOUT_MS: i32 = 200;
        let mut compositor_gone = false;

        loop {
            if stop_dispatch.load(Ordering::Relaxed) {
                break;
            }

            if let Err(e) = event_queue.flush() {
                eventline::error!("wayland: flush error: {}", e);
                compositor_gone = true;
                break;
            }

            match event_queue.prepare_read() {
                Some(read_guard) => match poll_wayland_fd(wayland_fd, POLL_TIMEOUT_MS) {
                    Ok(true) => {
                        if let Err(e) = read_guard.read() {
                            eventline::error!("wayland: read error: {}", e);
                            compositor_gone = true;
                            break;
                        }
                        if let Err(e) = event_queue.dispatch_pending(&mut state) {
                            eventline::error!("wayland: dispatch error: {}", e);
                            compositor_gone = true;
                            break;
                        }
                    }
                    Ok(false) => {
                        drop(read_guard); // timeout, check stop flag
                    }
                    Err(e) => {
                        eventline::error!("wayland: poll error: {}", e);
                        compositor_gone = true;
                        break;
                    }
                },
                None => {
                    // Events already queued; dispatch without reading.
                    if let Err(e) = event_queue.dispatch_pending(&mut state) {
                        eventline::error!("wayland: dispatch error: {}", e);
                        compositor_gone = true;
                        break;
                    }
                }
            }
        }

        compositor_gone
    });

    let compositor_gone = join.await.unwrap_or_else(|e| {
        eventline::error!("wayland: blocking task panicked: {:?}", e);
        true
    });

    if compositor_gone {
        eventline::info!("wayland: compositor disconnected; shutting down");
        let _ = shutdown_tx.send(true);
    } else {
        eventline::info!("wayland: stopping");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::error::TryRecvError;

    fn test_state() -> (WaylandState, mpsc::Receiver<ManagerMsg>) {
        let (tx, rx) = mpsc::channel(8);
        (WaylandState::new(tx, 250), rx)
    }

    fn recv_event(rx: &mut mpsc::Receiver<ManagerMsg>) -> Event {
        match rx.try_recv().expect("expected routed Wayland event") {
            ManagerMsg::Event(event) => event,
            other => panic!("expected manager event, got {other:?}"),
        }
    }

    #[test]
    fn idle_notifier_version_uses_v1_fallback_and_caps_at_v2() {
        assert_eq!(supported_idle_notifier_version(1), 1);
        assert_eq!(supported_idle_notifier_version(2), 2);
        assert_eq!(supported_idle_notifier_version(3), 2);
    }

    #[test]
    fn input_idle_notification_only_emits_activity_on_resume() {
        let (state, mut rx) = test_state();

        state.handle_idle_event(IdleNotificationKind::InputOnly, IdleEvent::Idled);
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

        state.handle_idle_event(IdleNotificationKind::InputOnly, IdleEvent::Resumed);
        assert!(matches!(recv_event(&mut rx), Event::UserActivity { .. }));
    }

    #[test]
    fn inhibitor_aware_notification_emits_compositor_edges() {
        let (state, mut rx) = test_state();

        state.handle_idle_event(IdleNotificationKind::InhibitorAware, IdleEvent::Idled);
        assert!(matches!(recv_event(&mut rx), Event::CompositorIdled { .. }));

        state.handle_idle_event(IdleNotificationKind::InhibitorAware, IdleEvent::Resumed);
        assert!(matches!(
            recv_event(&mut rx),
            Event::CompositorResumed { .. }
        ));
    }
}
