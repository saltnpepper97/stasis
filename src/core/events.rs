// Author: Dustin Pilgrim
// License: GPL-3.0-only

use crate::core::blame::DbusHold;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerState {
    OnAC,
    OnBattery,
}

/// Source of an observed session lock-state change.
///
/// Locker-process lifetime is the compatibility fallback. Once login1's
/// `LockedHint` confirms a lock episode, the hint becomes authoritative until
/// it reports the matching unlock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockSource {
    LockerProcess,
    LockedHint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Tick {
        now_ms: u64,
    },

    /// Input/activity observed (source-agnostic).
    UserActivity {
        kind: ActivityKind,
        now_ms: u64,
    },

    /// Browser-reported user activity pulse (e.g. extension/native host).
    /// This should behave like input activity for idle timing, but does not
    /// contribute to inhibitor counters.
    BrowserActivity {
        now_ms: u64,
    },

    /// Browser reports that playback/activity keepalive is no longer active.
    BrowserInactive {
        now_ms: u64,
    },

    /// Compositor-reported "seat has been idle for N ms" via ext-idle-notify-v1.
    ///
    /// NOTE: Some compositors may not send `CompositorResumed` reliably (niri currently),
    /// so this should not be the sole correctness mechanism for activity tracking.
    CompositorIdled {
        now_ms: u64,
    },

    /// Compositor-reported "seat resumed from idle" via ext-idle-notify-v1.
    CompositorResumed {
        now_ms: u64,
    },

    PowerChanged {
        state: PowerState,
        now_ms: u64,
    },

    LidClosed {
        now_ms: u64,
    },
    LidOpened {
        now_ms: u64,
    },

    SessionLocked {
        source: LockSource,
        now_ms: u64,
    },
    SessionUnlocked {
        source: LockSource,
        now_ms: u64,
    },

    ManualPause {
        now_ms: u64,
    },
    ManualResume {
        now_ms: u64,
    },

    /// Manually run a configured plan step by name/kind.
    /// (e.g. "startup", "dpms", "lock_screen", "suspend", "early-dpms")
    ManualTrigger {
        now_ms: u64,
        name: String,
    },

    /// A timer-driven pause ("pause for"/"pause until") ended.
    /// This is *not* the same as the user explicitly running `stasis resume`.
    PauseExpired {
        now_ms: u64,
        message: String,
    },

    ProfileChanged {
        name: String,
        now_ms: u64,
    },

    PrepareForSleep {
        now_ms: u64,
    },
    ResumedFromSleep {
        now_ms: u64,
    },

    AppInhibitorCount {
        count: u64,
        suspend_count: u64,
        now_ms: u64,
    },
    MediaInhibitorCount {
        count: u64,
        suspend_count: u64,
        now_ms: u64,
    },
    AppInhibitorSources {
        sources: Vec<String>,
        suspend_sources: Vec<String>,
        now_ms: u64,
    },
    MediaInhibitorSources {
        sources: Vec<String>,
        suspend_sources: Vec<String>,
        now_ms: u64,
    },
    DbusInhibitorsChanged {
        holds: Vec<DbusHold>,
        now_ms: u64,
    },
    BrowserSourceCaptureChanged {
        active: bool,
        now_ms: u64,
    },
}

impl Event {
    pub fn now_ms(&self) -> u64 {
        match self {
            Event::Tick { now_ms }
            | Event::UserActivity { now_ms, .. }
            | Event::BrowserActivity { now_ms }
            | Event::BrowserInactive { now_ms }
            | Event::CompositorIdled { now_ms }
            | Event::CompositorResumed { now_ms }
            | Event::PowerChanged { now_ms, .. }
            | Event::LidClosed { now_ms }
            | Event::LidOpened { now_ms }
            | Event::SessionLocked { now_ms, .. }
            | Event::SessionUnlocked { now_ms, .. }
            | Event::ManualPause { now_ms }
            | Event::ManualResume { now_ms }
            | Event::ManualTrigger { now_ms, .. }
            | Event::PauseExpired { now_ms, .. }
            | Event::ProfileChanged { now_ms, .. }
            | Event::PrepareForSleep { now_ms }
            | Event::ResumedFromSleep { now_ms }
            | Event::AppInhibitorCount { now_ms, .. }
            | Event::MediaInhibitorCount { now_ms, .. }
            | Event::AppInhibitorSources { now_ms, .. }
            | Event::MediaInhibitorSources { now_ms, .. }
            | Event::DbusInhibitorsChanged { now_ms, .. } => *now_ms,
            Event::BrowserSourceCaptureChanged { now_ms, .. } => *now_ms,
        }
    }
}
