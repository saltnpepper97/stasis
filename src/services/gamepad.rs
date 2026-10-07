// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use evdev::{AbsInfo, AbsoluteAxisCode, Device, EventSummary, InputEvent, KeyCode};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{MissedTickBehavior, interval};

use crate::core::events::{ActivityKind, Event};
use crate::core::manager_msg::ManagerMsg;

const SCAN_INTERVAL: Duration = Duration::from_secs(2);
const ACTIVITY_INTERVAL: Duration = Duration::from_millis(250);

fn controller_button(code: KeyCode) -> bool {
    matches!(code.0, 0x120..=0x13f | 0x220..=0x223 | 0x2c0..=0x2e7)
}

// Linux sysfs capability words are native unsigned longs, highest word first.
// Inspect capabilities before opening a device so we never monitor keyboards,
// mice, or controller motion-sensor/touchpad companion devices.
fn controller_capabilities(bitmap: &str) -> bool {
    (0x120usize..=0x13f).any(|code| {
        bitmap
            .split_whitespace()
            .rev()
            .nth(code / usize::BITS as usize)
            .and_then(|word| usize::from_str_radix(word, 16).ok())
            .is_some_and(|word| word & (1usize << (code % usize::BITS as usize)) != 0)
    })
}

fn discover_controllers() -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir("/sys/class/input")? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("event") {
            continue;
        }
        let Ok(keys) = fs::read_to_string(entry.path().join("device/capabilities/key")) else {
            continue;
        };
        if controller_capabilities(&keys) {
            paths.push(Path::new("/dev/input").join(name));
        }
    }
    Ok(paths)
}

#[derive(Debug)]
struct Axis {
    value: i32,
    neutral: i64,
    deadzone: i64,
}

impl Axis {
    fn new(code: AbsoluteAxisCode, info: AbsInfo) -> Option<Self> {
        use AbsoluteAxisCode as A;
        let minimum = i64::from(info.minimum());
        let maximum = i64::from(info.maximum());
        let span = maximum - minimum;
        if span <= 0 {
            return None;
        }
        let (neutral, deadzone) = match code {
            A::ABS_X | A::ABS_Y | A::ABS_RX | A::ABS_RY | A::ABS_WHEEL => {
                // Fifteen percent of stick travel on either side of centre.
                ((minimum + maximum) / 2, span * 15 / 200)
            }
            A::ABS_Z | A::ABS_RZ | A::ABS_GAS | A::ABS_BRAKE | A::ABS_THROTTLE => {
                // Triggers rest at the minimum, including signed ranges.
                (minimum, span * 15 / 100)
            }
            A::ABS_HAT0X
            | A::ABS_HAT0Y
            | A::ABS_HAT1X
            | A::ABS_HAT1Y
            | A::ABS_HAT2X
            | A::ABS_HAT2Y
            | A::ABS_HAT3X
            | A::ABS_HAT3Y => (0, 0),
            _ => return None,
        };
        Some(Self {
            value: info.value(),
            neutral,
            deadzone: deadzone
                .max(i64::from(info.flat()))
                .max(i64::from(info.fuzz())),
        })
    }

    fn active(&self) -> bool {
        (i64::from(self.value) - self.neutral).abs() > self.deadzone
    }

    fn update(&mut self, value: i32) -> bool {
        if self.value == value {
            return false;
        }
        let was_active = self.active();
        self.value = value;
        // Centred drift does not count, but releasing a stick/trigger does.
        was_active || self.active()
    }
}

#[derive(Default)]
struct Controls {
    buttons: HashSet<KeyCode>,
    axes: HashMap<AbsoluteAxisCode, Axis>,
}

impl Controls {
    fn from_device(device: &Device) -> io::Result<Self> {
        let buttons = device
            .get_key_state()?
            .iter()
            .filter(|key| controller_button(*key))
            .collect();
        let mut axes = HashMap::new();
        for (code, info) in device.get_absinfo()? {
            if let Some(axis) = Axis::new(code, info) {
                axes.insert(code, axis);
            }
        }
        Ok(Self { buttons, axes })
    }

    fn active(&self) -> bool {
        !self.buttons.is_empty() || self.axes.values().any(Axis::active)
    }

    fn update(&mut self, event: InputEvent) -> bool {
        match event.destructure() {
            EventSummary::Key(_, code, value) if controller_button(code) => match value {
                0 => self.buttons.remove(&code),
                1 => self.buttons.insert(code),
                _ => false, // Key repeats and driver noise are not new edges.
            },
            EventSummary::AbsoluteAxis(_, code, value) => self
                .axes
                .get_mut(&code)
                .is_some_and(|axis| axis.update(value)),
            _ => false,
        }
    }
}

async fn read_controller(device: Device, tx: mpsc::Sender<ManagerMsg>) -> io::Result<()> {
    let mut controls = Controls::from_device(&device)?;
    // No EVIOCGRAB: the game continues to receive every input event.
    let mut events = device.into_event_stream()?;
    let mut pulse = interval(ACTIVITY_INTERVAL);
    pulse.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut pending_activity = false;
    loop {
        tokio::select! {
            event = events.next_event() => {
                // Device's synchronization layer recovers state after SYN_DROPPED.
                pending_activity |= controls.update(event?);
            }
            _ = pulse.tick() => {
                // A held button/stick/trigger remains activity even if the driver
                // emits no further changes. Connection alone is never activity.
                if pending_activity || controls.active() {
                    pending_activity = false;
                    if tx.send(ManagerMsg::Event(Event::UserActivity {
                        kind: ActivityKind::Gamepad,
                        now_ms: crate::core::utils::now_ms(),
                    })).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Automatically attach to readable Linux controllers, including hotplug.
/// Disabling monitoring or shutting down drops every device handle and task.
pub async fn run_gamepad(
    tx: mpsc::Sender<ManagerMsg>,
    enabled: watch::Receiver<bool>,
    shutdown: watch::Receiver<bool>,
) {
    run_monitor(tx, enabled, shutdown, discover_controllers).await;
}

async fn run_monitor(
    tx: mpsc::Sender<ManagerMsg>,
    mut enabled: watch::Receiver<bool>,
    mut shutdown: watch::Receiver<bool>,
    discover: impl Fn() -> io::Result<Vec<PathBuf>> + Send,
) {
    let mut readers: JoinSet<(PathBuf, io::Result<()>)> = JoinSet::new();
    let mut attached = HashMap::new();
    let mut failures = HashMap::new();
    let mut scan_failure = None;
    let mut scan = interval(SCAN_INTERVAL);
    scan.set_missed_tick_behavior(MissedTickBehavior::Skip);
    eventline::info!("gamepad: started (monitor_gamepad={})", *enabled.borrow());

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() || shutdown.has_changed().is_err() {
                    break;
                }
            }
            changed = enabled.changed() => {
                if changed.is_err() {
                    break;
                }
                if !*enabled.borrow_and_update() {
                    readers.shutdown().await;
                    attached.clear();
                    failures.clear();
                    publish_devices(&tx, &attached).await;
                    eventline::info!("gamepad: monitoring disabled");
                } else {
                    scan.reset_immediately();
                }
            }
            Some(result) = readers.join_next(), if !readers.is_empty() => {
                match result {
                    Ok((path, result)) => {
                        attached.remove(&path);
                        publish_devices(&tx, &attached).await;
                        match result {
                            Ok(()) => break, // The manager channel closed.
                            Err(error) => eventline::info!(
                                "gamepad: detached {} ({error})", path.display()
                            ),
                        }
                    }
                    Err(error) => eventline::warn!("gamepad: reader failed: {error}"),
                }
            }
            _ = scan.tick(), if *enabled.borrow() => {
                let paths = match discover() {
                    Ok(paths) => { scan_failure = None; paths }
                    Err(error) => {
                        if scan_failure != Some(error.kind()) {
                            eventline::warn!("gamepad: cannot discover controllers: {error}");
                            scan_failure = Some(error.kind());
                        }
                        continue;
                    }
                };
                failures.retain(|path, _| paths.contains(path));
                for path in paths {
                    if attached.contains_key(&path) {
                        continue;
                    }
                    match Device::open(&path) {
                        Ok(device) => {
                            failures.remove(&path);
                            // Recheck after opening in case an event node was reused.
                            if !device.supported_keys().is_some_and(|keys|
                                keys.iter().any(|key| matches!(key.0, 0x120..=0x13f))) {
                                continue;
                            }
                            eventline::info!("gamepad: monitoring {} ({})", path.display(),
                                device.name().unwrap_or("unnamed controller"));
                            attached.insert(path.clone(), device.name().unwrap_or("unnamed controller").to_string());
                            publish_devices(&tx, &attached).await;
                            let tx = tx.clone();
                            readers.spawn(async move {
                                let result = read_controller(device, tx).await;
                                (path, result)
                            });
                        }
                        Err(error) => {
                            if failures.insert(path.clone(), error.kind()) != Some(error.kind()) {
                                eventline::warn!(
                                    "gamepad: cannot read {}: {error}; controller monitoring requires read access to this device",
                                    path.display()
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    readers.shutdown().await;
    eventline::info!("gamepad: stopped");
}

async fn publish_devices(tx: &mpsc::Sender<ManagerMsg>, attached: &HashMap<PathBuf, String>) {
    let mut devices: Vec<_> = attached.values().cloned().collect();
    devices.sort();
    let _ = tx
        .send(ManagerMsg::Event(Event::GamepadDevicesChanged {
            devices,
            now_ms: crate::core::utils::now_ms(),
        }))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::{AbsoluteAxisEvent, AttributeSet, KeyEvent, UinputAbsSetup, uinput::VirtualDevice};
    use tokio::time::timeout;

    fn virtual_controller() -> (VirtualDevice, PathBuf) {
        let mut buttons = AttributeSet::new();
        buttons.insert(KeyCode::BTN_SOUTH);
        let mut pad = VirtualDevice::builder()
            .unwrap()
            .name("Stasis gamepad regression test")
            .with_keys(&buttons)
            .unwrap()
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_X,
                AbsInfo::new(0, -32768, 32767, 16, 128, 0),
            ))
            .unwrap()
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_Z,
                AbsInfo::new(0, 0, 255, 0, 0, 0),
            ))
            .unwrap()
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_HAT0X,
                AbsInfo::new(0, -1, 1, 0, 0, 0),
            ))
            .unwrap()
            .build()
            .unwrap();
        let path = pad
            .enumerate_dev_nodes_blocking()
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        (pad, path)
    }

    async fn expect_activity(rx: &mut mpsc::Receiver<ManagerMsg>, wait: Duration) {
        let event = timeout(wait, async {
            loop {
                let event = rx.recv().await.expect("reader should remain connected");
                if !matches!(
                    event,
                    ManagerMsg::Event(Event::GamepadDevicesChanged { .. })
                ) {
                    break event;
                }
            }
        })
        .await
        .expect("controller activity should arrive");
        assert!(matches!(
            event,
            ManagerMsg::Event(Event::UserActivity {
                kind: ActivityKind::Gamepad,
                ..
            })
        ));
    }

    async fn expect_quiet(rx: &mut mpsc::Receiver<ManagerMsg>) {
        assert!(
            timeout(Duration::from_millis(400), async {
                loop {
                    let event = rx.recv().await.expect("service should remain connected");
                    if !matches!(
                        event,
                        ManagerMsg::Event(Event::GamepadDevicesChanged { .. })
                    ) {
                        break event;
                    }
                }
            })
            .await
            .is_err(),
            "controller at rest should not generate activity"
        );
    }

    async fn open_when_ready(path: &Path) -> Device {
        // uinput creates the node before udev assigns its input group/ACL.
        timeout(Duration::from_secs(3), async {
            loop {
                match Device::open(path) {
                    Ok(device) => break device,
                    Err(error)
                        if error.kind() == io::ErrorKind::PermissionDenied
                            || error.kind() == io::ErrorKind::NotFound =>
                    {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    Err(error) => panic!("cannot open virtual controller: {error}"),
                }
            }
        })
        .await
        .expect("udev should make the test controller accessible")
    }

    #[tokio::test]
    #[ignore = "requires read/write access to /dev/uinput and generated input devices"]
    async fn linux_input_reader_filters_drift_tracks_holds_and_disconnects() {
        let (mut pad, path) = virtual_controller();
        assert!(discover_controllers().unwrap().contains(&path));
        let (tx, mut rx) = mpsc::channel(32);
        let reader = tokio::spawn(read_controller(open_when_ready(&path).await, tx));
        expect_quiet(&mut rx).await;

        for value in [200, -200, 500, -500, 0] {
            pad.emit(&[*AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_X, value)])
                .unwrap();
        }
        expect_quiet(&mut rx).await;
        let controls = [
            (
                *KeyEvent::new(KeyCode::BTN_SOUTH, 1),
                *KeyEvent::new(KeyCode::BTN_SOUTH, 0),
            ),
            (
                *AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_X, 20000),
                *AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_X, 0),
            ),
            (
                *AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_Z, 200),
                *AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_Z, 0),
            ),
            (
                *AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_HAT0X, -1),
                *AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_HAT0X, 0),
            ),
        ];
        for (press, release) in controls {
            pad.emit(&[press]).unwrap();
            for _ in 0..4 {
                expect_activity(&mut rx, Duration::from_secs(1)).await;
            }
            pad.emit(&[release]).unwrap();
            expect_activity(&mut rx, Duration::from_secs(1)).await;
            expect_quiet(&mut rx).await;
        }
        drop(pad);
        assert!(
            timeout(Duration::from_secs(2), reader)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    #[ignore = "requires read/write access to /dev/uinput and generated input devices"]
    async fn linux_gamepad_service_hotplugs_reconnects_disables_and_stops() {
        let (tx, mut rx) = mpsc::channel(32);
        let (enabled_tx, enabled_rx) = watch::channel(true);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        // Isolate test devices so a user can keep using physical controllers.
        let discover = || {
            Ok(discover_controllers()?
                .into_iter()
                .filter(|path| {
                    let Some(event) = path.file_name() else {
                        return false;
                    };
                    fs::read_to_string(
                        Path::new("/sys/class/input")
                            .join(event)
                            .join("device/name"),
                    )
                    .is_ok_and(|name| name.trim() == "Stasis gamepad regression test")
                })
                .collect())
        };
        let service = tokio::spawn(run_monitor(tx, enabled_rx, shutdown_rx, discover));
        // Create after starting the service to exercise hotplug discovery.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (mut pad, _) = virtual_controller();
        pad.emit(&[*KeyEvent::new(KeyCode::BTN_SOUTH, 1)]).unwrap();
        expect_activity(&mut rx, Duration::from_secs(4)).await;
        drop(pad);
        tokio::time::sleep(Duration::from_millis(350)).await;
        while rx.try_recv().is_ok() {}
        expect_quiet(&mut rx).await;

        let (mut pad, _) = virtual_controller();
        pad.emit(&[*KeyEvent::new(KeyCode::BTN_SOUTH, 1)]).unwrap();
        expect_activity(&mut rx, Duration::from_secs(4)).await;
        enabled_tx.send(false).unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        while rx.try_recv().is_ok() {}
        expect_quiet(&mut rx).await;

        // Re-enabling detects a control still held, without another input edge.
        enabled_tx.send(true).unwrap();
        expect_activity(&mut rx, Duration::from_secs(2)).await;
        shutdown_tx.send(true).unwrap();
        timeout(Duration::from_secs(1), service)
            .await
            .unwrap()
            .unwrap();
        while rx.try_recv().is_ok() {}
        assert!(rx.recv().await.is_none());
        drop(pad);
    }

    #[test]
    fn stick_drift_is_ignored_but_deflection_and_release_count() {
        let mut axis = Axis::new(
            AbsoluteAxisCode::ABS_X,
            AbsInfo::new(0, -32768, 32767, 16, 128, 0),
        )
        .unwrap();
        for value in [1, -1, 200, -200, 4000, -4000, 0] {
            assert!(!axis.update(value));
            assert!(!axis.active());
        }
        assert!(axis.update(20000));
        assert!(axis.active());
        assert!(!axis.update(20000));
        assert!(axis.update(0));
        assert!(!axis.active());
    }

    #[test]
    fn unsigned_sticks_and_signed_triggers_use_the_correct_neutral() {
        let stick =
            Axis::new(AbsoluteAxisCode::ABS_RX, AbsInfo::new(128, 0, 255, 0, 0, 0)).unwrap();
        assert!(!stick.active());
        let mut trigger = Axis::new(
            AbsoluteAxisCode::ABS_Z,
            AbsInfo::new(-32768, -32768, 32767, 0, 0, 0),
        )
        .unwrap();
        assert!(!trigger.active());
        assert!(!trigger.update(-30000));
        assert!(trigger.update(0));
        assert!(trigger.active());
        assert!(trigger.update(-32768));
        assert!(!trigger.active());
    }

    #[test]
    fn driver_deadzone_and_sensors_are_respected() {
        let mut axis = Axis::new(
            AbsoluteAxisCode::ABS_X,
            AbsInfo::new(0, -100, 100, 0, 40, 0),
        )
        .unwrap();
        assert!(!axis.update(39));
        assert!(axis.update(41));
        assert!(
            Axis::new(
                AbsoluteAxisCode::ABS_MISC,
                AbsInfo::new(50, 0, 100, 0, 0, 0)
            )
            .is_none()
        );
    }

    #[test]
    fn buttons_and_dpad_track_holds_without_keyboard_or_repeat_noise() {
        let mut controls = Controls::default();
        controls.axes.insert(
            AbsoluteAxisCode::ABS_HAT0X,
            Axis::new(AbsoluteAxisCode::ABS_HAT0X, AbsInfo::new(0, -1, 1, 0, 0, 0)).unwrap(),
        );
        assert!(!controls.active());
        assert!(!controls.update(*KeyEvent::new(KeyCode::KEY_A, 1)));
        assert!(controls.update(*KeyEvent::new(KeyCode::BTN_SOUTH, 1)));
        assert!(controls.active());
        assert!(!controls.update(*KeyEvent::new(KeyCode::BTN_SOUTH, 2)));
        assert!(controls.update(*KeyEvent::new(KeyCode::BTN_SOUTH, 0)));
        assert!(!controls.active());
        assert!(controls.update(*AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_HAT0X, -1)));
        assert!(controls.active());
        assert!(controls.update(*AbsoluteAxisEvent::new(AbsoluteAxisCode::ABS_HAT0X, 0)));
        assert!(!controls.active());
    }

    #[test]
    fn discovery_distinguishes_controller_buttons_from_other_devices() {
        let bitmap = |code: usize| {
            let word = code / usize::BITS as usize;
            let mut words = vec![0usize; word + 1];
            words[word] = 1 << (code % usize::BITS as usize);
            words
                .iter()
                .rev()
                .map(|word| format!("{word:x}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(controller_capabilities(&bitmap(
            KeyCode::BTN_SOUTH.0 as usize
        )));
        assert!(controller_capabilities(&bitmap(
            KeyCode::BTN_TRIGGER.0 as usize
        )));
        assert!(!controller_capabilities(&bitmap(
            KeyCode::BTN_LEFT.0 as usize
        )));
        assert!(!controller_capabilities(&bitmap(KeyCode::KEY_A.0 as usize)));
        assert!(!controller_capabilities("invalid"));
    }
}
