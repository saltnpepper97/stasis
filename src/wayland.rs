use eyre::Result;
use std::sync::Arc;
use std::time::Duration;

use crate::idle_timer::IdleTimer;
use crate::log::{log_error_message, log_message};

use tokio::sync::Notify;
use tokio::time::sleep;

use wayland_client::{
    protocol::{wl_registry, wl_seat::WlSeat},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notifier_v1::ExtIdleNotifierV1,
    ext_idle_notification_v1::{ExtIdleNotificationV1, Event as IdleEvent},
};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::{ZwpIdleInhibitManagerV1, Event as InhibitMgrEvent},
    zwp_idle_inhibitor_v1::{ZwpIdleInhibitorV1, Event as InhibitorEvent},
};

/// Holds Wayland idle state and handles integration with IdleTimer
pub struct WaylandIdleData {
    pub idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>,
    pub idle_notifier: Option<ExtIdleNotifierV1>,
    pub seat: Option<WlSeat>,
    pub notification: Option<ExtIdleNotificationV1>,
    pub inhibit_manager: Option<ZwpIdleInhibitManagerV1>,
    pub active_inhibitors: u32,
    pub respect_inhibitors: bool,
    pub shutdown: Arc<Notify>,
}

impl WaylandIdleData {
    pub fn new(idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>, respect_inhibitors: bool) -> Self {
        Self {
            idle_timer,
            idle_notifier: None,
            seat: None,
            notification: None,
            inhibit_manager: None,
            active_inhibitors: 0,
            respect_inhibitors,
            shutdown: Arc::new(Notify::new()),
        }
    }

    pub fn is_inhibited(&self) -> bool {
        self.respect_inhibitors && self.active_inhibitors > 0
    }
}

/// Bind registry globals
impl Dispatch<wl_registry::WlRegistry, ()> for WaylandIdleData {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "ext_idle_notifier_v1" => {
                    state.idle_notifier =
                        Some(registry.bind::<ExtIdleNotifierV1, _, _>(name, 1, qh, ()));
                    log_message("Binding ext_idle_notifier_v1");
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, 1, qh, ()));
                    log_message("Binding wl_seat");
                }
                "zwp_idle_inhibit_manager_v1" => {
                    state.inhibit_manager =
                        Some(registry.bind::<ZwpIdleInhibitManagerV1, _, _>(name, 1, qh, ()));
                    log_message("Binding zwp_idle_inhibit_manager_v1");
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for WaylandIdleData {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<ExtIdleNotificationV1, ()> for WaylandIdleData {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: IdleEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let idle_timer = Arc::clone(&state.idle_timer);
        let inhibited = state.is_inhibited();

        tokio::spawn(async move {
            if inhibited {
                log_message("Idle inhibited by an app; skipping idle trigger");
                return;
            }

            let mut timer = idle_timer.lock().await;
            if timer.is_compositor_managed() {
                return;
            }

            match event {
                IdleEvent::Idled => {
                    log_message("Compositor detected idle state");
                    timer.mark_all_idle();
                    timer.trigger_idle().await;
                }
                IdleEvent::Resumed => {
                    log_message("Compositor detected activity");
                    timer.reset();
                }
                _ => {}
            }
        });
    }
}

impl Dispatch<ZwpIdleInhibitorV1, ()> for WaylandIdleData {
    fn event(
        state: &mut Self,
        _proxy: &ZwpIdleInhibitorV1,
        _event: InhibitorEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.active_inhibitors += 1;
        log_message(&format!("Inhibitor created, count={}", state.active_inhibitors));
    }
}

impl Dispatch<ZwpIdleInhibitManagerV1, ()> for WaylandIdleData {
    fn event(
        state: &mut Self,
        _proxy: &ZwpIdleInhibitManagerV1,
        _event: InhibitMgrEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if state.active_inhibitors > 0 {
            state.active_inhibitors -= 1;
            log_message(&format!("Inhibitor removed, count={}", state.active_inhibitors));
        }
    }
}

impl Dispatch<WlSeat, ()> for WaylandIdleData {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

/// Setup Wayland idle detection
pub async fn setup(
    idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>,
    respect_inhibitors: bool,
) -> Result<Arc<tokio::sync::Mutex<WaylandIdleData>>> {
    log_message(&format!("Setting up Wayland idle detection (respect_inhibitors={})", respect_inhibitors));

    let conn = Connection::connect_to_env()
        .map_err(|e| eyre::eyre!("Failed to connect to Wayland: {}", e))?;
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let display = conn.display();

    let mut app_data = WaylandIdleData::new(idle_timer.clone(), respect_inhibitors);
    let _registry = display.get_registry(&qh, ());
    event_queue.roundtrip(&mut app_data)?;

    if let (Some(notifier), Some(seat)) = (&app_data.idle_notifier, &app_data.seat) {
        let timeout_ms = {
            let timer = idle_timer.lock().await;
            timer.shortest_timeout().as_millis() as u32
        };
        let notification = notifier.get_idle_notification(timeout_ms, seat, &qh, ());
        app_data.notification = Some(notification);

        let mut timer = idle_timer.lock().await;
        timer.set_compositor_managed(true);
        log_message("Wayland idle detection active");
    }

    let app_data = Arc::new(tokio::sync::Mutex::new(app_data));
    let shutdown = {
        let locked = app_data.lock().await;
        Arc::clone(&locked.shutdown)
    };

    // Event loop task
    tokio::spawn({
        let app_data = Arc::clone(&app_data);
        async move {
            loop {
                {
                    let mut locked_data = app_data.lock().await;
                    if let Err(e) = event_queue.dispatch_pending(&mut *locked_data) {
                        log_error_message(&format!("Wayland event error: {}", e));
                    }
                }

                tokio::select! {
                    _ = shutdown.notified() => {
                        log_message("Wayland event loop shutting down");
                        break;
                    }
                    _ = sleep(Duration::from_millis(50)) => {}
                }
            }
        }
    });

    Ok(app_data)
}

