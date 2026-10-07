// Author: Dustin Pilgrim
// License: GPL-3.0-only

use serde::{Deserialize, Serialize};

use crate::core::blame::Login1IdleHold;
use crate::core::config::Pattern;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedGame {
    pub id: String,
    pub title: String,
    pub source: String,
    pub path_game_dir: Option<String>,
    pub pids: Vec<i32>,
}

impl DetectedGame {
    pub fn is_blacklisted(&self, patterns: &[Pattern]) -> bool {
        let steam_app_id = self
            .id
            .strip_prefix("steam:")
            .map(|id| format!("steam_app_{id}"));
        [
            Some(self.id.as_str()),
            Some(self.title.as_str()),
            Some(self.source.as_str()),
            self.path_game_dir.as_deref(),
            steam_app_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|field| {
            let field = field.to_lowercase();
            patterns.iter().any(|pattern| pattern.matches_lc(&field))
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GamesInfo {
    pub monitoring: bool,
    pub catalogue_entries: usize,
    pub running: Vec<DetectedGame>,
    pub ignored: Vec<DetectedGame>,
    pub errors: Vec<String>,
}

impl GamesInfo {
    /// Reclassify observations using the current effective config, including
    /// queued observations from before a profile switch or config reload.
    pub fn apply_rules(&mut self, monitoring: bool, blacklist: &[Pattern]) {
        self.monitoring = monitoring;
        if !monitoring {
            self.running.clear();
            self.ignored.clear();
            return;
        }
        let games = std::mem::take(&mut self.running)
            .into_iter()
            .chain(std::mem::take(&mut self.ignored));
        for game in games {
            if game.is_blacklisted(blacklist) {
                self.ignored.push(game);
            } else {
                self.running.push(game);
            }
        }
        self.running.sort_by(|a, b| a.id.cmp(&b.id));
        self.ignored.sort_by(|a, b| a.id.cmp(&b.id));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GamepadInfo {
    pub monitoring: bool,
    pub devices: Vec<String>,
    pub input_recent: bool,
    pub last_activity_ms: Option<u64>,
}

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
    pub gamepad: GamepadInfo,
    pub games: GamesInfo,
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
