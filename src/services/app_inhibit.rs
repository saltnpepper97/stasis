// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::collections::HashSet;
use std::env;
use std::mem;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use crate::core::config::Pattern;
use crate::core::events::Event;
use crate::core::manager_msg::ManagerMsg;

#[derive(Debug, Clone)]
pub struct AppRules {
    pub epoch: u64,
    pub apps: Vec<Pattern>,
    pub suspend_apps: Vec<Pattern>,
}

/// Spawnable task: periodically polls app inhibitors and emits events on change.
///
/// Logging policy (INFO):
/// - If count changes: log "X -> Y"
/// - If we are forcing a refresh (profile/reload/rules changed) OR first poll:
///     log "0 -> N" ONLY when N != 0
pub async fn run_app_inhibit(
    tx: mpsc::Sender<ManagerMsg>,
    mut rules_rx: watch::Receiver<AppRules>,
) {
    let initial = rules_rx.borrow().clone();
    let mut last_epoch = initial.epoch;

    let mut svc =
        AppInhibitService::new(&initial.apps, &initial.suspend_apps).with_poll_interval_ms(1000);

    eventline::info!("app_inhibit: started (backend={})", svc.backend_name());

    // Ensure we emit once immediately (and log 0->N if N!=0).
    svc.force_emit_next();

    let sleep_ms = 250u64;

    loop {
        tokio::select! {
            changed = rules_rx.changed() => {
                if changed.is_err() {
                    return;
                }

                let rules = rules_rx.borrow().clone();

                let epoch_bumped = rules.epoch != last_epoch;
                if epoch_bumped {
                    last_epoch = rules.epoch;
                    svc.force_emit_next();
                }

                svc.reconfigure(&rules.apps, &rules.suspend_apps);

                let now_ms = crate::core::utils::now_ms();
                if let Some(events) = svc.poll_async(now_ms).await {
                    for ev in events {
                        if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                            return;
                        }
                    }
                }
            }

            _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {
                let now_ms = crate::core::utils::now_ms();
                if let Some(events) = svc.poll_async(now_ms).await {
                    for ev in events {
                        if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct AppInhibitService {
    apps: Vec<Pattern>,
    suspend_apps: Vec<Pattern>,
    backend: Backend,

    poll_interval_ms: u64,
    last_poll_ms: u64,

    last_counts: Option<(u64, u64)>, // None => never polled yet
    last_sources: (Vec<String>, Vec<String>),
    force_emit: bool, // next poll must emit and do baseline logging

    /// Reused scratch buffer — `clear()`ed before every poll, never dropped.
    /// We `mem::take` it into `spawn_blocking` and restore it on return so we
    /// never allocate a new HashSet on the hot path. If the task panics, the
    /// field is left as an empty default and a fresh allocation occurs next poll.
    seen: HashSet<String>,
    suspend_seen: HashSet<String>,
}

#[derive(Debug)]
enum Backend {
    Halley(HalleyBackend),
    Hyprland(HyprlandBackend),
    Niri(NiriBackend),
    Proc(ProcBackend),
}

#[derive(Debug, Default)]
struct HalleyBackend {}

#[derive(Debug, Default)]
struct HyprlandBackend {}

#[derive(Debug, Default)]
struct NiriBackend {}

#[derive(Debug, Default)]
struct ProcBackend {}

impl AppInhibitService {
    pub fn new(inhibit_apps: &[Pattern], suspend_inhibit_apps: &[Pattern]) -> Self {
        let apps = normalize_patterns(inhibit_apps);
        let suspend_apps = normalize_patterns(suspend_inhibit_apps);
        let backend = detect_backend().unwrap_or_else(|| Backend::Proc(ProcBackend::default()));

        Self {
            apps,
            suspend_apps,
            backend,
            poll_interval_ms: 1000,
            last_poll_ms: 0,
            last_counts: None,
            last_sources: (Vec::new(), Vec::new()),
            force_emit: false,
            // Start small; the shrink logic below keeps it tight at steady state.
            seen: HashSet::with_capacity(8),
            suspend_seen: HashSet::with_capacity(8),
        }
    }

    pub fn with_poll_interval_ms(mut self, ms: u64) -> Self {
        self.poll_interval_ms = ms.max(100);
        self
    }

    pub fn reconfigure(&mut self, inhibit_apps: &[Pattern], suspend_inhibit_apps: &[Pattern]) {
        let new_apps = normalize_patterns(inhibit_apps);
        let new_suspend_apps = normalize_patterns(suspend_inhibit_apps);

        if patterns_same(&self.apps, &new_apps)
            && patterns_same(&self.suspend_apps, &new_suspend_apps)
        {
            return;
        }

        self.apps = new_apps;
        self.suspend_apps = new_suspend_apps;
        self.force_emit_next();

        eventline::info!(
            "app_inhibit: reconfigured (apps_len={}, suspend_apps_len={}, backend={})",
            self.apps.len(),
            self.suspend_apps.len(),
            self.backend_name(),
        );
    }

    pub fn force_emit_next(&mut self) {
        self.force_emit = true;
        self.last_poll_ms = 0;
    }

    /// Async-aware poll. Subprocess/fs queries run via `spawn_blocking` so the
    /// tokio executor thread is never blocked.
    ///
    /// The `seen` scratch buffer is moved into the blocking task via `mem::take`
    /// and returned alongside the count so its backing allocation survives across
    /// polls — no HashSet is allocated on the steady-state hot path.
    pub async fn poll_async(&mut self, now_ms: u64) -> Option<Vec<Event>> {
        if now_ms < self.last_poll_ms.saturating_add(self.poll_interval_ms) {
            return None;
        }
        self.last_poll_ms = now_ms;

        let previous = self.last_counts.unwrap_or((0, 0));

        let counts = if self.apps.is_empty() && self.suspend_apps.is_empty() {
            // Nothing to match — rinse the buffer so stale entries can't linger.
            self.seen.clear();
            self.suspend_seen.clear();
            (0, 0)
        } else {
            // Take both scratch buffers out so one backend query can classify
            // every observed app against both rule sets.
            let mut scratch = mem::take(&mut self.seen);
            let mut suspend_scratch = mem::take(&mut self.suspend_seen);
            scratch.clear();
            suspend_scratch.clear();

            match &self.backend {
                Backend::Halley(_) => {
                    let apps = self.apps.clone();
                    let suspend_apps = self.suspend_apps.clone();
                    match tokio::task::spawn_blocking(move || {
                        HalleyBackend::count_into(
                            &apps,
                            &suspend_apps,
                            &mut scratch,
                            &mut suspend_scratch,
                        )?;
                        Ok::<_, String>((scratch, suspend_scratch))
                    })
                    .await
                    {
                        Ok(Ok((returned, returned_suspend))) => {
                            self.seen = returned;
                            self.suspend_seen = returned_suspend;
                            (self.seen.len() as u64, self.suspend_seen.len() as u64)
                        }
                        Ok(Err(e)) => {
                            eventline::warn!(
                                "app_inhibit: halley query failed (keeping previous counts={:?}): {}",
                                previous,
                                e
                            );
                            previous
                        }
                        Err(e) => {
                            eventline::warn!(
                                "app_inhibit: halley task panicked (keeping previous counts={:?}): {}",
                                previous,
                                e
                            );
                            previous
                        }
                    }
                }

                Backend::Hyprland(_) => {
                    let apps = self.apps.clone();
                    let suspend_apps = self.suspend_apps.clone();
                    match tokio::task::spawn_blocking(move || {
                        HyprlandBackend::count_into(
                            &apps,
                            &suspend_apps,
                            &mut scratch,
                            &mut suspend_scratch,
                        )?;
                        Ok::<_, String>((scratch, suspend_scratch))
                    })
                    .await
                    {
                        Ok(Ok((returned, returned_suspend))) => {
                            self.seen = returned;
                            self.suspend_seen = returned_suspend;
                            (self.seen.len() as u64, self.suspend_seen.len() as u64)
                        }
                        Ok(Err(e)) => {
                            eventline::warn!(
                                "app_inhibit: hyprland query failed (keeping previous counts={:?}): {}",
                                previous,
                                e
                            );
                            previous
                        }
                        Err(e) => {
                            eventline::warn!(
                                "app_inhibit: hyprland task panicked (keeping previous counts={:?}): {}",
                                previous,
                                e
                            );
                            previous
                        }
                    }
                }

                Backend::Niri(_) => {
                    let apps = self.apps.clone();
                    let suspend_apps = self.suspend_apps.clone();
                    match tokio::task::spawn_blocking(move || {
                        NiriBackend::count_into(
                            &apps,
                            &suspend_apps,
                            &mut scratch,
                            &mut suspend_scratch,
                        )?;
                        Ok::<_, String>((scratch, suspend_scratch))
                    })
                    .await
                    {
                        Ok(Ok((returned, returned_suspend))) => {
                            self.seen = returned;
                            self.suspend_seen = returned_suspend;
                            (self.seen.len() as u64, self.suspend_seen.len() as u64)
                        }
                        Ok(Err(e)) => {
                            eventline::warn!(
                                "app_inhibit: niri query failed (keeping previous counts={:?}): {}",
                                previous,
                                e
                            );
                            previous
                        }
                        Err(e) => {
                            eventline::warn!(
                                "app_inhibit: niri task panicked (keeping previous counts={:?}): {}",
                                previous,
                                e
                            );
                            previous
                        }
                    }
                }

                Backend::Proc(_) => {
                    let apps = self.apps.clone();
                    let suspend_apps = self.suspend_apps.clone();
                    match tokio::task::spawn_blocking(move || {
                        ProcBackend::count_into(
                            &apps,
                            &suspend_apps,
                            &mut scratch,
                            &mut suspend_scratch,
                        );
                        (scratch, suspend_scratch)
                    })
                    .await
                    {
                        Ok((returned, returned_suspend)) => {
                            self.seen = returned;
                            self.suspend_seen = returned_suspend;
                            (self.seen.len() as u64, self.suspend_seen.len() as u64)
                        }
                        Err(e) => {
                            eventline::warn!(
                                "app_inhibit: proc task panicked (keeping previous counts={:?}): {}",
                                previous,
                                e
                            );
                            previous
                        }
                    }
                }
            }
        };

        // Aggressively shrink the scratch buffer if it ballooned beyond what we
        // realistically need. An idle manager typically matches 0–5 apps at once;
        // keeping 32+ empty slots alive wastes RSS indefinitely.
        if self.seen.capacity() > 32 && self.seen.len() < 8 {
            self.seen.shrink_to(8);
        }
        if self.suspend_seen.capacity() > 32 && self.suspend_seen.len() < 8 {
            self.suspend_seen.shrink_to(8);
        }

        let mut sources: Vec<_> = self.seen.iter().cloned().collect();
        let mut suspend_sources: Vec<_> = self.suspend_seen.iter().cloned().collect();
        sources.sort();
        suspend_sources.sort();

        let first_poll = self.last_counts.is_none();
        let counts_changed = !first_poll && previous != counts;
        let sources_changed = !first_poll
            && (self.last_sources.0 != sources || self.last_sources.1 != suspend_sources);
        let changed = counts_changed || sources_changed;

        if counts_changed {
            eventline::info!(
                "app_inhibit: counts {:?} -> {:?} (backend={}, apps_len={}, suspend_apps_len={})",
                previous,
                counts,
                self.backend_name(),
                self.apps.len(),
                self.suspend_apps.len()
            );
        } else if sources_changed {
            eventline::debug!("app_inhibit: matched application identities changed");
        } else if (first_poll || self.force_emit) && counts != (0, 0) {
            eventline::info!(
                "app_inhibit: counts {:?} -> {:?} (backend={}, apps_len={}, suspend_apps_len={})",
                (0u64, 0u64),
                counts,
                self.backend_name(),
                self.apps.len(),
                self.suspend_apps.len()
            );
        }

        if first_poll || changed || self.force_emit {
            self.last_counts = Some(counts);
            self.last_sources = (sources.clone(), suspend_sources.clone());
            self.force_emit = false;

            return Some(vec![
                Event::AppInhibitorCount {
                    count: counts.0,
                    suspend_count: counts.1,
                    now_ms,
                },
                Event::AppInhibitorSources {
                    sources,
                    suspend_sources,
                    now_ms,
                },
            ]);
        }

        None
    }

    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            Backend::Halley(_) => "halley",
            Backend::Hyprland(_) => "hyprland",
            Backend::Niri(_) => "niri",
            Backend::Proc(_) => "proc",
        }
    }
}

// ----------------------------- backend detection -----------------------------

fn detect_backend() -> Option<Backend> {
    detect_halley_backend()
        .or_else(detect_hyprland_backend)
        .or_else(detect_niri_backend)
}

fn detect_halley_backend() -> Option<Backend> {
    for key in [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ] {
        if let Ok(value) = env::var(key) {
            if value.to_lowercase().contains("halley") {
                return Some(Backend::Halley(HalleyBackend::default()));
            }
        }
    }

    if env::var("HALLEY_WL_BACKEND").is_ok() {
        return Some(Backend::Halley(HalleyBackend::default()));
    }

    None
}

fn detect_hyprland_backend() -> Option<Backend> {
    if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        return Some(Backend::Hyprland(HyprlandBackend::default()));
    }

    if let Ok(desktop) = env::var("XDG_CURRENT_DESKTOP") {
        if desktop.to_lowercase().contains("hyprland") {
            return Some(Backend::Hyprland(HyprlandBackend::default()));
        }
    }

    None
}

fn detect_niri_backend() -> Option<Backend> {
    if let Ok(desktop) = env::var("XDG_CURRENT_DESKTOP") {
        if desktop.to_lowercase().contains("niri") {
            return Some(Backend::Niri(NiriBackend::default()));
        }
    }

    if env::var("NIRI_SOCKET").is_ok() {
        return Some(Backend::Niri(NiriBackend::default()));
    }

    None
}

// ----------------------------- matching helpers ------------------------------

/// Returns `true` on the first matching pattern, short-circuiting the rest.
fn should_inhibit_app_id(app_id: &str, patterns: &[Pattern]) -> bool {
    if app_id.is_empty() {
        return false;
    }

    for pat in patterns {
        let matched = match pat {
            Pattern::Literal(s) => app_id_matches_literal(s, app_id),
            Pattern::Regex(r) => r.is_match(app_id),
        };
        if matched {
            return true;
        }
    }

    false
}

fn record_app_id(
    app_id: &str,
    apps: &[Pattern],
    suspend_apps: &[Pattern],
    seen: &mut HashSet<String>,
    suspend_seen: &mut HashSet<String>,
) {
    if should_inhibit_app_id(app_id, apps) {
        seen.insert(app_id.to_string());
    }
    if should_inhibit_app_id(app_id, suspend_apps) {
        suspend_seen.insert(app_id.to_string());
    }
}

fn app_id_matches_literal(pattern: &str, app_id: &str) -> bool {
    // Exact case-insensitive match.
    if pattern.eq_ignore_ascii_case(app_id) {
        return true;
    }
    // "firefox.exe" vs pattern "firefox".
    if app_id.ends_with(".exe") {
        let name = app_id.strip_suffix(".exe").unwrap_or(app_id);
        if pattern.eq_ignore_ascii_case(name) {
            return true;
        }
    }
    // Reverse dotted suffix: pattern "org.mozilla.firefox" vs app_id "firefox".
    if let Some(last) = pattern.split('.').last() {
        if last.eq_ignore_ascii_case(app_id) {
            return true;
        }
    }
    false
}

// ----------------------------- Halley (halleyctl) ----------------------------

impl HalleyBackend {
    /// Populates `seen` with matched Halley window app IDs. Caller must `clear()` first.
    fn count_into(
        apps: &[Pattern],
        suspend_apps: &[Pattern],
        seen: &mut HashSet<String>,
        suspend_seen: &mut HashSet<String>,
    ) -> Result<(), String> {
        let out = std::process::Command::new("halleyctl")
            .args(["node", "list", "--json"])
            .output()
            .map_err(|e| format!("halleyctl spawn failed: {e}"))?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("halleyctl node list --json failed: {}", err.trim()));
        }

        Self::count_json_into(apps, suspend_apps, seen, suspend_seen, &out.stdout)
    }

    fn count_json_into(
        apps: &[Pattern],
        suspend_apps: &[Pattern],
        seen: &mut HashSet<String>,
        suspend_seen: &mut HashSet<String>,
        json: &[u8],
    ) -> Result<(), String> {
        let v: serde_json::Value = serde_json::from_slice(json)
            .map_err(|e| format!("halleyctl json parse failed: {e}"))?;

        let outputs = v
            .get("outputs")
            .and_then(|x| x.as_array())
            .ok_or_else(|| "halleyctl json: expected outputs array".to_string())?;

        for output in outputs {
            let Some(nodes) = output.get("nodes").and_then(|x| x.as_array()) else {
                continue;
            };

            for node in nodes {
                let app_id = node.get("app_id").and_then(|x| x.as_str()).unwrap_or("");
                if app_id.is_empty() {
                    continue;
                }

                record_app_id(app_id, apps, suspend_apps, seen, suspend_seen);
            }
        }

        Ok(())
    }
}

// ----------------------------- Hyprland (hyprctl) ----------------------------

impl HyprlandBackend {
    /// Populates `seen` with matched window classes. Caller must `clear()` first.
    fn count_into(
        apps: &[Pattern],
        suspend_apps: &[Pattern],
        seen: &mut HashSet<String>,
        suspend_seen: &mut HashSet<String>,
    ) -> Result<(), String> {
        let out = std::process::Command::new("hyprctl")
            .args(["clients", "-j"])
            .output()
            .map_err(|e| format!("hyprctl spawn failed: {e}"))?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("hyprctl clients -j failed: {}", err.trim()));
        }

        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("hyprctl json parse failed: {e}"))?;

        let arr = v
            .as_array()
            .ok_or_else(|| "hyprctl json: expected array".to_string())?;

        for item in arr {
            let class = item.get("class").and_then(|x| x.as_str()).unwrap_or("");
            if class.is_empty() {
                continue;
            }
            record_app_id(class, apps, suspend_apps, seen, suspend_seen);
        }

        Ok(())
    }
}

// ----------------------------- Niri (niri msg windows) -----------------------

impl NiriBackend {
    /// Populates `seen` with matched app IDs. Caller must `clear()` first.
    fn count_into(
        apps: &[Pattern],
        suspend_apps: &[Pattern],
        seen: &mut HashSet<String>,
        suspend_seen: &mut HashSet<String>,
    ) -> Result<(), String> {
        let out = std::process::Command::new("niri")
            .args(["msg", "windows"])
            .output()
            .map_err(|e| format!("niri spawn failed: {e}"))?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("niri msg windows failed: {}", err.trim()));
        }

        let text = String::from_utf8_lossy(&out.stdout);

        for line in text.lines() {
            let Some(rest) = line.strip_prefix("  App ID: ") else {
                continue;
            };

            let app_id = rest.trim().trim_matches('"');
            if app_id.is_empty() {
                continue;
            }

            record_app_id(app_id, apps, suspend_apps, seen, suspend_seen);
        }

        Ok(())
    }
}

// ----------------------------- /proc (procfs crate) --------------------------

impl ProcBackend {
    /// Populates `seen` with matched process names via the `procfs` crate.
    ///
    /// The `procfs` crate reads `/proc` entries through a single `read_dir` pass
    /// and parses only the fields we actually need (`stat.comm` and `exe`), which
    /// avoids the overhead of manually re-implementing that logic and the full
    /// process list copy that `sysinfo` kept alive.
    ///
    /// Early-exit: once every literal pattern has a hit in `seen` *and* there
    /// are no regex patterns left to satisfy, further scanning cannot change the
    /// count so we break out of the loop immediately.
    fn count_into(
        apps: &[Pattern],
        suspend_apps: &[Pattern],
        seen: &mut HashSet<String>,
        suspend_seen: &mut HashSet<String>,
    ) {
        let has_regex = apps
            .iter()
            .chain(suspend_apps)
            .any(|p| matches!(p, Pattern::Regex(_)));

        // Pre-compute the exact keys that literal patterns produce so we can
        // check saturation in O(n_literals) rather than O(n_seen).
        let literal_keys: Vec<String> = apps
            .iter()
            .filter_map(|p| {
                if let Pattern::Literal(s) = p {
                    Some(s.to_lowercase())
                } else {
                    None
                }
            })
            .collect();
        let suspend_literal_keys: Vec<String> = suspend_apps
            .iter()
            .filter_map(|p| {
                if let Pattern::Literal(s) = p {
                    Some(s.to_lowercase())
                } else {
                    None
                }
            })
            .collect();

        let all_processes = match procfs::process::all_processes() {
            Ok(iter) => iter,
            Err(e) => {
                eventline::warn!("app_inhibit: procfs::all_processes failed: {e}");
                return;
            }
        };

        for prc in all_processes.flatten() {
            // Early-exit when all literal patterns are satisfied and there are no
            // regex patterns that could add new unique keys.
            if !has_regex
                && literal_keys.iter().all(|k| seen.contains(k.as_str()))
                && suspend_literal_keys
                    .iter()
                    .all(|k| suspend_seen.contains(k.as_str()))
            {
                break;
            }

            // Primary: `comm` — kernel-truncated to 15 chars but fast and
            // sufficient for most app names.
            let (comm_matched, suspend_comm_matched) = prc
                .stat()
                .ok()
                .map(|stat| {
                    let matched = Self::match_key(&stat.comm, apps)
                        .map(|key| seen.insert(key))
                        .is_some();
                    let suspend_matched = Self::match_key(&stat.comm, suspend_apps)
                        .map(|key| suspend_seen.insert(key))
                        .is_some();
                    (matched, suspend_matched)
                })
                .unwrap_or((false, false));

            let need_exe = (!apps.is_empty() && !comm_matched)
                || (!suspend_apps.is_empty() && !suspend_comm_matched);
            if !need_exe {
                continue;
            }

            // Fallback: exe basename — handles wrappers that rename comm or
            // apps whose name is longer than 15 characters.
            if let Ok(exe) = prc.exe()
                && let Some(name) = exe.file_name().and_then(|n| n.to_str())
            {
                if !comm_matched && let Some(key) = Self::match_key(name, apps) {
                    seen.insert(key);
                }
                if !suspend_comm_matched && let Some(key) = Self::match_key(name, suspend_apps) {
                    suspend_seen.insert(key);
                }
            }
        }
    }

    #[inline]
    fn match_key(hay: &str, apps: &[Pattern]) -> Option<String> {
        for p in apps {
            let matched = match p {
                Pattern::Literal(s) => hay.eq_ignore_ascii_case(s),
                Pattern::Regex(r) => r.is_match(hay),
            };
            if matched {
                return Some(hay.to_lowercase());
            }
        }
        None
    }
}

// ----------------------------- utils -----------------------------------------

/// Deduplicate patterns by their string representation so duplicate rules from
/// a config reload don't silently inflate match counts.
fn normalize_patterns(inhibit_apps: &[Pattern]) -> Vec<Pattern> {
    let mut seen_strs: HashSet<String> = HashSet::with_capacity(inhibit_apps.len());
    inhibit_apps
        .iter()
        .filter(|p| seen_strs.insert(p.to_string()))
        .cloned()
        .collect()
}

fn patterns_same(a: &[Pattern], b: &[Pattern]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .map(|p| p.to_string())
        .zip(b.iter().map(|p| p.to_string()))
        .all(|(x, y)| x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halley_json_classifies_both_rule_sets_in_one_scan() {
        let apps = vec![
            Pattern::Literal("firefox".to_string()),
            Pattern::Regex(regex::Regex::new(r"steam_app_.*").unwrap()),
        ];
        let suspend_apps = vec![
            Pattern::Literal("firefox".to_string()),
            Pattern::Literal("kitty".to_string()),
        ];
        let json = br#"
        {
          "outputs": [
            {
              "output": "DP-1",
              "nodes": [
                { "id": 1, "app_id": "firefox", "title": "one" },
                { "id": 2, "app_id": "firefox", "title": "two" },
                { "id": 3, "app_id": "kitty", "title": "shell" }
              ]
            },
            {
              "output": "DP-2",
              "nodes": [
                { "id": 4, "app_id": "steam_app_123", "title": "game" },
                { "id": 5, "app_id": null, "title": "missing app" }
              ]
            }
          ]
        }
        "#;

        let mut seen = HashSet::new();
        let mut suspend_seen = HashSet::new();
        HalleyBackend::count_json_into(&apps, &suspend_apps, &mut seen, &mut suspend_seen, json)
            .unwrap();

        assert_eq!(seen.len(), 2);
        assert!(seen.contains("firefox"));
        assert!(seen.contains("steam_app_123"));
        assert_eq!(suspend_seen.len(), 2);
        assert!(suspend_seen.contains("firefox"));
        assert!(suspend_seen.contains("kitty"));
    }

    #[test]
    fn halley_json_rejects_missing_outputs_array() {
        let mut seen = HashSet::new();
        let mut suspend_seen = HashSet::new();
        let err = HalleyBackend::count_json_into(
            &[],
            &[],
            &mut seen,
            &mut suspend_seen,
            br#"{"nodes": []}"#,
        )
        .expect_err("missing outputs should be invalid");

        assert!(err.contains("expected outputs array"));
    }
}
