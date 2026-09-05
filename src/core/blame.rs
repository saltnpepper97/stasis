// Author: Dustin Pilgrim
// License: GPL-3.0-only

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DbusHold {
    pub status: String,
    pub protocol: String,
    pub source: String,
    pub sender: String,
    pub application: Option<String>,
    pub process: Option<String>,
    pub pid: Option<u32>,
    pub reason: Option<String>,
    pub flags: Option<u32>,
    pub started_at_ms: u64,
    pub age_ms: u64,
    pub cookie: Option<u32>,
    pub handle: Option<String>,
}

impl DbusHold {
    pub fn with_age(mut self, now_ms: u64) -> Self {
        self.age_ms = now_ms.saturating_sub(self.started_at_ms);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Login1IdleHold {
    pub status: String,
    pub what: String,
    pub who: String,
    pub why: String,
    pub mode: String,
    pub uid: u32,
    pub pid: u32,
    pub process: Option<String>,
    pub started_at_ms: u64,
    pub age_ms: u64,
}

impl Login1IdleHold {
    pub fn with_age(mut self, now_ms: u64) -> Self {
        self.age_ms = now_ms.saturating_sub(self.started_at_ms);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlameCategory {
    pub active: bool,
    pub count: u64,
    pub sources: Vec<String>,
}

impl BlameCategory {
    pub fn new(count: u64, sources: &[String]) -> Self {
        Self {
            active: count > 0,
            count,
            sources: sources.to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlameSnapshot {
    pub schema_version: u32,
    pub generated_at_ms: u64,
    pub progression_blocked: bool,
    pub manual_pause: bool,
    pub system_pause: bool,
    pub browser_source_capture: bool,
    pub app_inhibitors: BlameCategory,
    pub media_inhibitors: BlameCategory,
    pub suspend_app_inhibitors: BlameCategory,
    pub suspend_media_inhibitors: BlameCategory,
    pub dbus_holds: Vec<DbusHold>,
    pub login1_idle_holds: Vec<Login1IdleHold>,
}

impl BlameSnapshot {
    pub fn render_human(&self) -> String {
        let mut out = String::from("Stasis progression blockers\n");
        push_toggle(&mut out, "Manual pause", self.manual_pause);
        push_toggle(&mut out, "System pause", self.system_pause);
        push_toggle(
            &mut out,
            "Browser source capture",
            self.browser_source_capture,
        );
        push_category(&mut out, "Applications", &self.app_inhibitors);
        push_category(&mut out, "Media", &self.media_inhibitors);
        push_category(
            &mut out,
            "Suspend-only applications",
            &self.suspend_app_inhibitors,
        );
        push_category(
            &mut out,
            "Suspend-only media",
            &self.suspend_media_inhibitors,
        );

        if self.dbus_holds.is_empty() {
            out.push_str("D-Bus holds: none\n");
        } else {
            out.push_str(&format!("D-Bus holds: {} live\n", self.dbus_holds.len()));
            for hold in &self.dbus_holds {
                let identity = hold
                    .application
                    .as_deref()
                    .or(hold.process.as_deref())
                    .unwrap_or("unresolved application");
                out.push_str(&format!(
                    "  - {identity} via {} [{}], age {}",
                    hold.protocol,
                    hold.status,
                    human_duration(hold.age_ms)
                ));
                if let Some(reason) = hold.reason.as_deref().filter(|s| !s.is_empty()) {
                    out.push_str(&format!(", reason: {reason}"));
                }
                if let Some(flags) = hold.flags {
                    out.push_str(&format!(", flags: {flags}"));
                }
                if let Some(cookie) = hold.cookie {
                    out.push_str(&format!(", cookie: {cookie}"));
                }
                if let Some(handle) = hold.handle.as_deref() {
                    out.push_str(&format!(", handle: {handle}"));
                }
                if hold.application.is_none() && hold.process.is_none() {
                    out.push_str(&format!(", sender: {}", hold.sender));
                }
                out.push('\n');
            }
        }

        if self.login1_idle_holds.is_empty() {
            out.push_str("login1 idle holds: none\n");
        } else {
            out.push_str(&format!(
                "login1 idle holds: {} blocking suspend\n",
                self.login1_idle_holds.len()
            ));
            for hold in &self.login1_idle_holds {
                let identity = if hold.who.trim().is_empty() {
                    hold.process.as_deref().unwrap_or("unresolved application")
                } else {
                    &hold.who
                };
                out.push_str(&format!(
                    "  - {identity} [{}], age {}",
                    hold.status,
                    human_duration(hold.age_ms)
                ));
                if !hold.why.trim().is_empty() {
                    out.push_str(&format!(", reason: {}", hold.why));
                }
                out.push_str(&format!(", pid: {}\n", hold.pid));
            }
        }

        if !self.progression_blocked {
            out.push_str("Result: no current blocker\n");
        } else {
            out.push_str("Result: an automatic idle action is currently blocked\n");
        }

        out.trim_end().to_string()
    }
}

fn push_toggle(out: &mut String, label: &str, active: bool) {
    out.push_str(&format!(
        "{label}: {}\n",
        if active { "active" } else { "inactive" }
    ));
}

fn push_category(out: &mut String, label: &str, category: &BlameCategory) {
    if category.count == 0 {
        out.push_str(&format!("{label}: none\n"));
        return;
    }

    if category.sources.is_empty() {
        out.push_str(&format!(
            "{label}: {} (identity unavailable)\n",
            category.count
        ));
    } else {
        out.push_str(&format!(
            "{label}: {} ({})\n",
            category.count,
            category.sources.join(", ")
        ));
    }
}

fn human_duration(age_ms: u64) -> String {
    let seconds = age_ms / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_output_distinguishes_live_unresolved_dbus_hold() {
        let snapshot = BlameSnapshot {
            schema_version: 1,
            generated_at_ms: 2_000,
            progression_blocked: false,
            manual_pause: false,
            system_pause: false,
            browser_source_capture: false,
            app_inhibitors: BlameCategory::new(0, &[]),
            media_inhibitors: BlameCategory::new(0, &[]),
            suspend_app_inhibitors: BlameCategory::new(0, &[]),
            suspend_media_inhibitors: BlameCategory::new(0, &[]),
            dbus_holds: vec![DbusHold {
                status: "live-unresolved".to_string(),
                protocol: "org.freedesktop.ScreenSaver".to_string(),
                source: "legacy-cookie".to_string(),
                sender: ":1.4".to_string(),
                application: None,
                process: None,
                pid: None,
                reason: Some("sharing".to_string()),
                flags: None,
                started_at_ms: 1_000,
                age_ms: 1_000,
                cookie: Some(7),
                handle: None,
            }],
            login1_idle_holds: Vec::new(),
        };

        let rendered = snapshot.render_human();
        assert!(rendered.contains("unresolved application"));
        assert!(rendered.contains("[live-unresolved]"));
        assert!(rendered.contains("cookie: 7"));
    }
}
