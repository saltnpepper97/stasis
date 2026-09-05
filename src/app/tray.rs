// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::sync::LazyLock;
use std::time::Duration;

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
}

impl TraySnapshot {
    fn not_running(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            text: "not running".to_string(),
            alt: "not_running".to_string(),
            class: "not_running".to_string(),
            tooltip: format!("Stasis not running\n{message}"),
        }
    }

    fn state_title(&self) -> String {
        if self.alt == "manually_inhibited" {
            "Stasis paused (manually)".to_string()
        } else {
            format!("Stasis: {}", self.text)
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
    use super::TraySnapshot;

    fn manual_snapshot() -> TraySnapshot {
        TraySnapshot {
            text: "manual".to_string(),
            alt: "manually_inhibited".to_string(),
            class: "manually_inhibited".to_string(),
            tooltip: "Profile: default\nState: manual\nPaused: yes".to_string(),
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
        // Shells such as Dank Material Shell render this as the menu header.
        // Keep the state here, and do not repeat it as a menu item below.
        self.snapshot.state_title()
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

        vec![
            StandardItem {
                label: "Toggle Inhibit".to_string(),
                enabled: daemon_running,
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::ToggleInhibit)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Pause".to_string(),
                enabled: daemon_running,
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::Pause)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Resume".to_string(),
                enabled: daemon_running,
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::Resume)),
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
