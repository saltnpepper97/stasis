// Author: Dustin Pilgrim
// License: GPL-3.0-only

use serde::Serialize;

use crate::core::blame::Login1IdleHold;

/// Stable state published by `stasis watch`.
///
/// This intentionally excludes timer-derived display details so listeners only
/// receive a line when a shell-relevant state value actually changes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WatchEvent {
    /// One of: waiting, active, inhibited, locked, or manual.
    pub state: String,
    pub paused: bool,
    pub manually_paused: bool,
    pub profile: String,
}

/// Snapshot returned from the daemon/manager for `stasis info`.
///
/// - `waybar` is the stable JSON contract.
/// - `pretty_text` is CLI-facing output for `stasis info`.
#[derive(Debug, Clone, Serialize)]
pub struct InfoSnapshot {
    pub waybar: WaybarInfo,

    #[serde(skip_serializing)]
    pub pretty_text: String,

    pub manually_paused: bool,
}

/// Waybar JSON contract.
#[derive(Debug, Clone, Serialize)]
pub struct WaybarInfo {
    pub text: String,
    pub alt: String,
    pub class: String,
    pub tooltip: String,
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub login1_idle_inhibitors: Vec<Login1IdleHold>,
}

impl InfoSnapshot {
    pub fn new(waybar: WaybarInfo, pretty_text: impl Into<String>, manually_paused: bool) -> Self {
        Self {
            waybar,
            pretty_text: pretty_text.into(),
            manually_paused,
        }
    }
}
