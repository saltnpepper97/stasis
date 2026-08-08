// Author: Dustin Pilgrim
// License: GPL-3.0-only

use tokio::sync::oneshot;

use crate::core::{config::Pattern, events::Event, info::InfoSnapshot};

#[derive(Debug, Clone, Copy)]
pub enum ListKind {
    Actions,
    Profiles,
}

#[derive(Debug)]
pub enum ManagerMsg {
    Event(Event),

    GetInfo {
        reply: oneshot::Sender<InfoSnapshot>,
    },

    List {
        kind: ListKind,
        reply: oneshot::Sender<Result<String, String>>,
    },

    ReloadConfig {
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },

    SetProfile {
        name: Option<String>,
        reply: oneshot::Sender<Result<String, String>>,
    },

    StopDaemon {
        reply: oneshot::Sender<Result<String, String>>,
    },

    UpdateInhibitRules {
        epoch: u64,
        inhibit_apps: Vec<Pattern>,
        suspend_inhibit_apps: Vec<Pattern>,
        monitor_media: bool,
        ignore_remote_media: bool,
        media_blacklist: Vec<Pattern>,
    },
}
