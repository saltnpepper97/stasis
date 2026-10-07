// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::sync::LazyLock;
use std::time::Duration;

use crate::core::info::GamepadInfo;
use image::GenericImageView;
use ksni::{Tray, TrayMethods};
use serde::Deserialize;
use tokio::sync::mpsc;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

const TRAY_BUS_NAME: &str = "io.github.saltnpepper97.Stasis.Tray";

static TRAY_ICON: LazyLock<ksni::Icon> = LazyLock::new(|| {
    let img = image::load_from_memory_with_format(
        include_bytes!("../../assets/stasis-tray.png"),
        image::ImageFormat::Png,
    )
    .expect("embedded tray icon is a valid PNG")
    .resize(64, 64, image::imageops::FilterType::Lanczos3);

    let (width, height) = img.dimensions();
    let mut data = img.into_rgba8().into_vec();
    for pixel in data.chunks_exact_mut(4) {
        pixel.rotate_right(1); // RGBA -> ARGB, as required by StatusNotifierItem.
    }

    ksni::Icon {
        width: width as i32,
        height: height as i32,
        data,
    }
});

#[derive(Debug, Clone, Deserialize)]
struct TraySnapshot {
    text: String,
    alt: String,
    #[allow(dead_code)]
    class: String,
    tooltip: String,
    #[serde(default)]
    gamepad: Option<GamepadInfo>,
}

impl TraySnapshot {
    fn manually_paused(&self) -> bool {
        // The locked status can mask a manual pause in `alt`.
        self.alt == "manually_inhibited"
            || self
                .tooltip
                .lines()
                .any(|line| line.trim() == "Manual Pause: yes")
    }

    fn not_running(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            text: "not running".to_string(),
            alt: "not_running".to_string(),
            class: "not_running".to_string(),
            tooltip: format!("Stasis not running\n{message}"),
            gamepad: None,
        }
    }

    fn state_title(&self) -> String {
        if self.alt == "manually_inhibited" {
            "Stasis paused (manually)".to_string()
        } else {
            format!("Stasis: {}", self.text)
        }
    }

    fn gamepad_label(&self) -> String {
        let Some(info) = &self.gamepad else {
            return "Gamepad: status unavailable".to_string();
        };
        if !info.monitoring {
            "Gamepad: monitoring disabled".to_string()
        } else if info.devices.is_empty() {
            "Gamepad: none connected".to_string()
        } else {
            let input = if info.input_recent {
                "input detected"
            } else {
                "waiting for input"
            };
            format!("Gamepad: {} ({input})", info.devices.join(", "))
        }
    }

    fn tooltip_description(&self) -> String {
        self.tooltip
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                !line.starts_with("State:")
                    && !line.starts_with("Manual Pause:")
                    && !line.starts_with("Paused:")
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{StasisTray, TrayCommand, TraySnapshot};
    use ksni::Tray;
    use tokio::sync::mpsc;

    fn manual_snapshot() -> TraySnapshot {
        TraySnapshot {
            text: "manual".to_string(),
            alt: "manually_inhibited".to_string(),
            class: "manually_inhibited".to_string(),
            tooltip: "Profile: default\nState: manual\nPaused: yes".to_string(),
            gamepad: None,
        }
    }

    #[test]
    fn manual_pause_has_requested_title() {
        assert_eq!(manual_snapshot().state_title(), "Stasis paused (manually)");
    }

    #[test]
    fn tooltip_does_not_repeat_state_from_title() {
        let description = manual_snapshot().tooltip_description();
        assert!(!description.contains("State:"));
        assert!(!description.contains("Manual Pause:"));
        assert!(!description.contains("Paused:"));
        assert_eq!(description.matches("Profile: default").count(), 1);
    }

    #[test]
    fn controller_row_distinguishes_detection_input_and_disabled_monitoring() {
        use crate::core::info::GamepadInfo;
        let mut snapshot = manual_snapshot();
        assert_eq!(snapshot.gamepad_label(), "Gamepad: status unavailable");
        snapshot.gamepad = Some(GamepadInfo {
            monitoring: true,
            devices: vec!["Xbox controller".into()],
            input_recent: false,
            last_activity_ms: None,
        });
        assert_eq!(
            snapshot.gamepad_label(),
            "Gamepad: Xbox controller (waiting for input)"
        );
        snapshot.gamepad.as_mut().unwrap().input_recent = true;
        assert_eq!(
            snapshot.gamepad_label(),
            "Gamepad: Xbox controller (input detected)"
        );
        snapshot.gamepad.as_mut().unwrap().monitoring = false;
        assert_eq!(snapshot.gamepad_label(), "Gamepad: monitoring disabled");
        snapshot.gamepad.as_mut().unwrap().monitoring = true;
        snapshot.gamepad.as_mut().unwrap().devices.clear();
        assert_eq!(snapshot.gamepad_label(), "Gamepad: none connected");
    }

    #[test]
    fn tray_accepts_older_daemon_status_without_gamepad_metadata() {
        let snapshot: TraySnapshot = serde_json::from_str(
            r#"{"text":"active","alt":"idle_active","class":"idle_active","tooltip":"State: active"}"#,
        ).unwrap();
        assert!(snapshot.gamepad.is_none());
    }

    #[test]
    fn combined_pause_resume_action_tracks_manual_pause() {
        for (alt, manual_pause) in [
            ("idle_active", false),
            ("idle_inhibited", false),
            ("manually_inhibited", true),
            ("locked", true),
            ("locked", false),
            ("not_running", false),
        ] {
            let (commands, mut received) = mpsc::unbounded_channel();
            let mut tray = StasisTray {
                snapshot: TraySnapshot {
                    alt: alt.to_string(),
                    tooltip: format!("Manual Pause: {}", if manual_pause { "yes" } else { "no" }),
                    ..manual_snapshot()
                },
                commands,
            };
            let mut actions = tray.menu().into_iter().filter_map(|item| match item {
                ksni::MenuItem::Standard(item)
                    if item.label == "Pause" || item.label == "Resume" =>
                {
                    Some(item)
                }
                _ => None,
            });
            let action = actions.next().expect("pause/resume action exists");
            assert!(actions.next().is_none(), "only one pause/resume action");
            assert_eq!(action.label, if manual_pause { "Resume" } else { "Pause" });
            assert_eq!(action.enabled, alt != "not_running");
            if action.enabled {
                (action.activate)(&mut tray);
                match received.try_recv().expect("action sends a command") {
                    TrayCommand::Resume => assert!(manual_pause),
                    TrayCommand::Pause => assert!(!manual_pause),
                    command => panic!("unexpected command: {command:?}"),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TrayCommand {
    ToggleInhibit,
    Pause,
    Resume,
    Reload,
    Quit,
}

#[derive(Debug)]
struct StasisTray {
    snapshot: TraySnapshot,
    commands: mpsc::UnboundedSender<TrayCommand>,
}

impl StasisTray {
    fn send(&self, cmd: TrayCommand) {
        let _ = self.commands.send(cmd);
    }
}

impl Tray for StasisTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "stasis".to_string()
    }

    fn title(&self) -> String {
        // Some shells render this as a header; others show only the menu rows.
        "Stasis".to_string()
    }

    fn status(&self) -> ksni::Status {
        if self.snapshot.alt == "not_running" {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![TRAY_ICON.clone()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.snapshot.state_title(),
            description: self.snapshot.tooltip_description(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;

        let daemon_running = self.snapshot.alt != "not_running";
        let manually_paused = self.snapshot.manually_paused();

        vec![
            StandardItem {
                label: self.snapshot.state_title(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.snapshot.gamepad_label(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Toggle Inhibit".to_string(),
                enabled: daemon_running,
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::ToggleInhibit)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if manually_paused { "Resume" } else { "Pause" }.to_string(),
                enabled: daemon_running,
                activate: Box::new(move |this: &mut Self| {
                    this.send(if manually_paused {
                        TrayCommand::Resume
                    } else {
                        TrayCommand::Pause
                    });
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Reload Config".to_string(),
                enabled: daemon_running,
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::Reload)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Tray".to_string(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub async fn run() -> Result<(), AnyError> {
    let Some(_instance) = claim_single_instance().await? else {
        eprintln!("stasis tray: another instance is already running");
        return Ok(());
    };

    let (commands_tx, mut commands_rx) = mpsc::unbounded_channel();
    let tray = StasisTray {
        snapshot: fetch_snapshot().await,
        commands: commands_tx,
    };

    let handle = tray.spawn().await.map_err(|err| {
        format!(
            "tray unavailable: {err}. Start a StatusNotifier tray host first, such as Waybar's tray module, KDE Plasma, or another panel."
        )
    })?;

    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = refresh.tick() => {
                update_snapshot(&handle).await;
            }

            Some(cmd) = commands_rx.recv() => {
                if matches!(cmd, TrayCommand::Quit) {
                    handle.shutdown().await;
                    break;
                }

                run_command(cmd).await;
                update_snapshot(&handle).await;
            }
        }
    }

    Ok(())
}

async fn claim_single_instance() -> Result<Option<zbus::Connection>, zbus::Error> {
    let builder = zbus::connection::Builder::session()?
        .name(TRAY_BUS_NAME)?
        .allow_name_replacements(false)
        .replace_existing_names(false);
    match builder.build().await {
        Ok(connection) => Ok(Some(connection)),
        Err(zbus::Error::NameTaken) => Ok(None),
        Err(err) => Err(err),
    }
}

async fn update_snapshot(handle: &ksni::Handle<StasisTray>) {
    let snapshot = fetch_snapshot().await;
    let _ = handle
        .update(|tray: &mut StasisTray| {
            tray.snapshot = snapshot;
        })
        .await;
}

async fn fetch_snapshot() -> TraySnapshot {
    match crate::ipc::client::send_raw("info --json").await {
        Ok(resp) => serde_json::from_str(resp.trim()).unwrap_or_else(|err| {
            TraySnapshot::not_running(format!("invalid daemon status JSON: {err}"))
        }),
        Err(err) => TraySnapshot::not_running(err),
    }
}

async fn run_command(cmd: TrayCommand) {
    let raw = match cmd {
        TrayCommand::ToggleInhibit => "toggle-inhibit",
        TrayCommand::Pause => "pause",
        TrayCommand::Resume => "resume",
        TrayCommand::Reload => "reload",
        TrayCommand::Quit => return,
    };

    if let Err(err) = crate::ipc::client::send_raw(raw).await {
        eprintln!("stasis tray: {raw} failed: {err}");
    }
}
