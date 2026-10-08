// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use super::audio::{AudioReader, AudioStream, Direction};
use crate::core::config::Pattern;
use crate::core::events::Event;
use crate::core::manager_msg::ManagerMsg;

#[derive(Debug, Clone)]
pub struct MediaRules {
    pub epoch: u64, // forces watch::changed() on profile/reload even if values are identical
    pub monitor_media: bool,
    pub ignore_remote_media: bool,
    pub media_blacklist: Vec<Pattern>,
    pub suspend_inhibit_media: Vec<Pattern>,
}

/// Spawnable task: polls native PipeWire/PulseAudio playback for non-browser,
/// non-game media and emits events on change.
pub async fn run_media(tx: mpsc::Sender<ManagerMsg>, mut rules_rx: watch::Receiver<MediaRules>) {
    let initial = rules_rx.borrow().clone();
    let mut last_epoch = initial.epoch;
    let mut ignore_first_epoch_bump = true;

    let mut svc = MediaService::new(
        initial.ignore_remote_media,
        initial.media_blacklist.clone(),
        initial.suspend_inhibit_media.clone(),
    )
    .with_poll_interval_ms(500);

    eventline::info!(
        "media: started (monitor_media={}, ignore_remote_media={}, blacklist_len={}, suspend_media_len={}, backend={}) [native-audio-stream-state]",
        initial.monitor_media,
        initial.ignore_remote_media,
        svc.blacklist_len(),
        svc.suspend_media_len(),
        svc.backend_name(),
    );

    if initial.monitor_media {
        svc.force_emit_next();
        let now_ms = crate::core::utils::now_ms();
        if let Some(evs) = svc.poll(now_ms).await {
            for ev in evs {
                if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                    return;
                }
            }
        }
    } else {
        seed_idle(&tx).await;
    }

    let sleep_ms = 250u64;
    let mut last_enabled = initial.monitor_media;

    loop {
        tokio::select! {
            changed = rules_rx.changed() => {
                if changed.is_err() {
                    return;
                }

                let rules = rules_rx.borrow().clone();
                let MediaRules { epoch, monitor_media, ignore_remote_media, media_blacklist, suspend_inhibit_media } = rules;

                let epoch_bumped = epoch != last_epoch;
                if epoch_bumped {
                    last_epoch = epoch;
                }

                svc.reconfigure(
                    ignore_remote_media,
                    media_blacklist.clone(),
                    suspend_inhibit_media.clone(),
                );

                if monitor_media != last_enabled {
                    last_enabled = monitor_media;

                    if monitor_media {
                        svc.force_emit_next();
                        let now_ms = crate::core::utils::now_ms();
                        if let Some(evs) = svc.poll(now_ms).await {
                            for ev in evs {
                                if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    } else {
                        seed_idle(&tx).await;
                    }
                } else if monitor_media && epoch_bumped {
                    if ignore_first_epoch_bump {
                        ignore_first_epoch_bump = false;
                    } else {
                        svc.force_emit_next();
                        let now_ms = crate::core::utils::now_ms();
                        if let Some(evs) = svc.poll(now_ms).await {
                            for ev in evs {
                                if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }

            _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {
                let rules = rules_rx.borrow().clone();
                if !rules.monitor_media {
                    continue;
                }

                let now_ms = crate::core::utils::now_ms();
                if let Some(evs) = svc.poll(now_ms).await {
                    for ev in evs {
                        if tx.send(ManagerMsg::Event(ev)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn seed_idle(tx: &mpsc::Sender<ManagerMsg>) {
    let now_ms = crate::core::utils::now_ms();
    let _ = tx
        .send(ManagerMsg::Event(Event::MediaInhibitorCount {
            count: 0,
            suspend_count: 0,
            now_ms,
        }))
        .await;
    let _ = tx
        .send(ManagerMsg::Event(Event::MediaInhibitorSources {
            sources: Vec::new(),
            suspend_sources: Vec::new(),
            now_ms,
        }))
        .await;
}

#[derive(Debug)]
pub struct MediaService {
    ignore_remote_media: bool,
    media_blacklist: Vec<Pattern>,
    suspend_inhibit_media: Vec<Pattern>,
    audio: AudioReader,
    audio_error: Option<String>,
    last_streams: Vec<AudioStream>,

    poll_interval_ms: u64,
    last_poll_ms: u64,

    last_counts: Option<(u64, u64)>,
    last_sources: (Vec<String>, Vec<String>),

    force_emit: bool,
}

impl MediaService {
    pub fn new(
        ignore_remote_media: bool,
        media_blacklist: Vec<Pattern>,
        suspend_inhibit_media: Vec<Pattern>,
    ) -> Self {
        Self {
            ignore_remote_media,
            media_blacklist,
            suspend_inhibit_media,
            audio: AudioReader::default(),
            audio_error: None,
            last_streams: Vec::new(),
            poll_interval_ms: 1000,
            last_poll_ms: 0,
            last_counts: None,
            last_sources: (Vec::new(), Vec::new()),
            force_emit: false,
        }
    }

    pub fn with_poll_interval_ms(mut self, ms: u64) -> Self {
        self.poll_interval_ms = ms.max(100);
        self
    }

    pub fn blacklist_len(&self) -> usize {
        self.media_blacklist.len()
    }

    pub fn suspend_media_len(&self) -> usize {
        self.suspend_inhibit_media.len()
    }

    pub fn backend_name(&self) -> &'static str {
        self.audio.backend_name()
    }

    pub fn reconfigure(
        &mut self,
        ignore_remote_media: bool,
        media_blacklist: Vec<Pattern>,
        suspend_inhibit_media: Vec<Pattern>,
    ) {
        let changed = self.ignore_remote_media != ignore_remote_media
            || !patterns_same(&self.media_blacklist, &media_blacklist)
            || !patterns_same(&self.suspend_inhibit_media, &suspend_inhibit_media);

        self.ignore_remote_media = ignore_remote_media;
        self.media_blacklist = media_blacklist;
        self.suspend_inhibit_media = suspend_inhibit_media;

        if changed {
            self.force_emit_next();
            eventline::info!(
                "media: reconfigured (ignore_remote_media={}, blacklist_len={}, suspend_media_len={})",
                self.ignore_remote_media,
                self.media_blacklist.len(),
                self.suspend_inhibit_media.len()
            );
        }
    }

    pub fn force_emit_next(&mut self) {
        self.force_emit = true;
        self.last_poll_ms = 0;
    }

    pub async fn poll(&mut self, now_ms: u64) -> Option<Vec<Event>> {
        if now_ms < self.last_poll_ms.saturating_add(self.poll_interval_ms) {
            return None;
        }
        self.last_poll_ms = now_ms;

        let previous_backend = self.audio.backend_name();
        match self.audio.read(Direction::Playback).await {
            Ok(streams) => {
                if previous_backend != self.audio.backend_name() || self.audio_error.is_some() {
                    eventline::info!("media: connected to {}", self.audio.backend_name());
                }
                self.audio_error = None;
                self.last_streams = streams;
            }
            Err(error) => {
                if self.audio_error.as_ref() != Some(&error) {
                    eventline::warn!("media: audio query failed (keeping previous): {}", error);
                }
                self.audio_error = Some(error);
                self.last_counts?;
            }
        }
        let sources = audio_stream_sources(
            &self.last_streams,
            self.ignore_remote_media,
            &self.media_blacklist,
            &self.suspend_inhibit_media,
        );

        let counts = (sources.0.len() as u64, sources.1.len() as u64);

        self.emit_counts(now_ms, counts, sources)
    }

    fn emit_counts(
        &mut self,
        now_ms: u64,
        counts: (u64, u64),
        sources: (Vec<String>, Vec<String>),
    ) -> Option<Vec<Event>> {
        let first_poll = self.last_counts.is_none();
        let previous = self.last_counts.unwrap_or((0, 0));
        let counts_changed = !first_poll && previous != counts;
        let sources_changed = !first_poll && self.last_sources != sources;
        let changed = counts_changed || sources_changed;

        if counts_changed {
            eventline::info!(
                "media: counts {:?} -> {:?} (full, suspend-only)",
                previous,
                counts
            );
        } else if sources_changed {
            eventline::debug!("media: source identities changed");
        } else if (first_poll || self.force_emit) && counts != (0, 0) {
            eventline::info!(
                "media: counts {:?} -> {:?} (full, suspend-only)",
                (0u64, 0u64),
                counts
            );
        }

        if first_poll || changed || self.force_emit {
            self.last_counts = Some(counts);
            self.last_sources = sources.clone();
            self.force_emit = false;
            return Some(vec![
                Event::MediaInhibitorCount {
                    count: counts.0,
                    suspend_count: counts.1,
                    now_ms,
                },
                Event::MediaInhibitorSources {
                    sources: sources.0,
                    suspend_sources: sources.1,
                    now_ms,
                },
            ]);
        }

        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaInhibitKind {
    Full,
    Suspend,
}

fn audio_stream_sources(
    streams: &[AudioStream],
    ignore_remote_media: bool,
    media_blacklist: &[Pattern],
    suspend_inhibit_media: &[Pattern],
) -> (Vec<String>, Vec<String>) {
    let mut sources = (Vec::new(), Vec::new());
    for stream in streams {
        let Some(kind) = audio_stream_inhibit_kind(
            stream,
            ignore_remote_media,
            media_blacklist,
            suspend_inhibit_media,
        ) else {
            continue;
        };
        let label = media_source_label(&stream.props);
        match kind {
            MediaInhibitKind::Full => sources.0.push(label),
            MediaInhibitKind::Suspend => sources.1.push(label),
        }
    }
    sources.0.sort();
    sources.1.sort();
    sources
}

fn media_source_label(props: &HashMap<String, String>) -> String {
    for key in [
        "application.name",
        "app.name",
        "application.process.binary",
        "application.id",
        "node.description",
    ] {
        if let Some(value) = props.get(key).filter(|value| !value.trim().is_empty()) {
            return value.trim().to_string();
        }
    }
    "unresolved media stream".to_string()
}

fn audio_stream_inhibit_kind(
    stream: &AudioStream,
    ignore_remote_media: bool,
    media_blacklist: &[Pattern],
    suspend_inhibit_media: &[Pattern],
) -> Option<MediaInhibitKind> {
    let props = &stream.props;
    if stream.direction != Direction::Playback
        || !stream.running
        || stream.muted
        || props.is_empty()
    {
        return None;
    }

    // Browser/media-session ownership belongs in dbus.rs. media.rs should only
    // act as a narrow local-audio fallback, so aggressively exclude browser,
    // TTS/synthetic, and other system-ish streams here.
    if audio_stream_is_browser(props) {
        return None;
    }

    if audio_stream_is_synthetic_or_tts(props) {
        return None;
    }

    if audio_stream_is_systemish(props) {
        return None;
    }

    if audio_stream_is_game(props) {
        return None;
    }

    if audio_stream_is_blacklisted(media_blacklist, props) {
        return None;
    }

    if ignore_remote_media && audio_stream_is_remote(props) {
        return None;
    }

    let hay_lc = audio_stream_haystack(props);
    if suspend_inhibit_media
        .iter()
        .any(|pattern| pattern.matches_lc(&hay_lc))
    {
        Some(MediaInhibitKind::Suspend)
    } else {
        Some(MediaInhibitKind::Full)
    }
}

fn audio_stream_is_synthetic_or_tts(props: &HashMap<String, String>) -> bool {
    const NEEDLES: &[&str] = &[
        "speech-dispatcher",
        "speech dispatcher",
        "speech-dispatcher-dummy",
        "sd_dummy",
        "speechd",
        "espeak",
        "espeak-ng",
        "festival",
        "flite",
        "piper",
        "rhvoice",
        "orca",
        "screen reader",
        "accessibility",
    ];

    haystack_contains_any(&audio_stream_identity_haystack(props), NEEDLES)
}

fn audio_stream_is_systemish(props: &HashMap<String, String>) -> bool {
    const NEEDLES: &[&str] = &[
        "event sound",
        "notification",
        "system sound",
        "alert",
        "bell",
        "beep",
        "xdg-desktop-portal",
        "wireplumber",
    ];

    haystack_contains_any(&audio_stream_identity_haystack(props), NEEDLES)
}

fn audio_stream_is_blacklisted(blacklist: &[Pattern], props: &HashMap<String, String>) -> bool {
    if blacklist.is_empty() {
        return false;
    }

    let hay_lc = audio_stream_haystack(props);

    blacklist.iter().any(|p| p.matches_lc(&hay_lc))
}

pub(super) fn audio_stream_is_browser(props: &HashMap<String, String>) -> bool {
    const NEEDLES: &[&str] = &[
        "firefox",
        "chromium",
        "google-chrome",
        "google chrome",
        "chrome",
        "brave",
        "vivaldi",
        "microsoft-edge",
        "msedge",
        "opera",
        "tor browser",
        "zen browser",
        "zen-browser",
        "waterfox",
        "librewolf",
    ];

    haystack_contains_any(&audio_stream_identity_haystack(props), NEEDLES)
}

fn audio_stream_is_game(props: &HashMap<String, String>) -> bool {
    const NEEDLES: &[&str] = &[
        "steam",
        "gamescope",
        "lutris",
        "heroic",
        "prismlauncher",
        "minecraft",
        "wine",
        "proton",
        "retroarch",
        "dolphin-emu",
        "pcsx2",
        "rpcs3",
        "citra",
        "yuzu",
        "ryujinx",
    ];

    haystack_contains_any(&audio_stream_identity_haystack(props), NEEDLES)
}

fn audio_stream_is_remote(props: &HashMap<String, String>) -> bool {
    const NEEDLES: &[&str] = &[
        "spotify connect",
        "chromecast",
        "airplay",
        "raop",
        "dlna",
        "upnp",
        "snapcast",
        "shairport",
        "network stream",
        "http://",
        "https://",
        "rtsp://",
        "rtmp://",
        "mms://",
        "icy://",
    ];

    haystack_contains_any(&audio_stream_identity_haystack(props), NEEDLES)
}

/// Haystack restricted to identity fields only (process binary, app name, etc).
/// Use this for browser/game/systemish/tts/remote filters so that technical
/// plumbing fields like `client.api` can never cause false
/// positives.
fn audio_stream_identity_haystack(props: &HashMap<String, String>) -> String {
    let identity_keys = [
        "application.name",
        "application.process.binary",
        "application.process.executable",
        "application.icon_name",
        "app.name",
        "application.id",
        "node.name",
        "node.description",
    ];

    let mut parts = Vec::new();
    for key in identity_keys {
        if let Some(v) = props.get(key) {
            if !v.trim().is_empty() {
                parts.push(v.trim().to_string());
            }
        }
    }

    parts.join(" ").to_lowercase()
}

fn audio_stream_haystack(props: &HashMap<String, String>) -> String {
    let ordered_keys = [
        "application.name",
        "application.process.binary",
        "application.process.executable",
        "application.icon_name",
        "media.name",
        "media.title",
        "media.artist",
        "media.filename",
        "media.role",
        "node.name",
        "node.description",
        "application.id",
        "app.name",
    ];

    let mut parts = Vec::new();

    for key in ordered_keys {
        if let Some(v) = props.get(key) {
            if !v.trim().is_empty() {
                parts.push(v.trim().to_string());
            }
        }
    }

    for (k, v) in props {
        if ordered_keys.contains(&k.as_str()) {
            continue;
        }
        if !v.trim().is_empty() {
            parts.push(format!("{k} {}", v.trim()));
        }
    }

    parts.join(" ").to_lowercase()
}

fn haystack_contains_any(hay_lc: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay_lc.contains(n))
}

fn pattern_key(p: &Pattern) -> String {
    match p {
        Pattern::Literal(s) => s.clone(),
        Pattern::Regex(r) => format!("/{}/", r.as_str()),
    }
}

fn patterns_same(a: &[Pattern], b: &[Pattern]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    a.iter()
        .map(pattern_key)
        .zip(b.iter().map(pattern_key))
        .all(|(x, y)| x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal(value: &str) -> Pattern {
        Pattern::Literal(value.to_string())
    }

    fn stream(name: &str) -> AudioStream {
        AudioStream {
            direction: Direction::Playback,
            running: true,
            muted: false,
            props: HashMap::from([("application.name".into(), name.into())]),
        }
    }

    #[test]
    fn counts_full_and_suspend_only_streams_separately() {
        let sources = audio_stream_sources(
            &[stream("Spotify"), stream("VLC media player")],
            false,
            &[],
            &[literal("spotify")],
        );
        assert_eq!(
            sources,
            (vec!["VLC media player".into()], vec!["Spotify".into()])
        );
    }

    #[test]
    fn blacklist_and_remote_filters_take_precedence_over_suspend_rules() {
        let streams = [stream("Spotify"), stream("Spotify Connect")];
        let suspend = [literal("spotify")];
        assert_eq!(
            audio_stream_sources(&streams, true, &[literal("spotify")], &suspend),
            (vec![], vec![])
        );
        assert_eq!(
            audio_stream_sources(&streams, true, &[], &suspend),
            (vec![], vec!["Spotify".into()])
        );
    }

    #[test]
    fn inactive_capture_browser_game_and_system_streams_are_not_inhibitors() {
        let mut paused = stream("Spotify");
        paused.running = false;
        let mut muted = stream("VLC media player");
        muted.muted = true;
        let mut capture = stream("Recorder");
        capture.direction = Direction::Capture;
        let streams = [
            paused,
            muted,
            capture,
            stream("Firefox"),
            stream("Steam"),
            stream("speech-dispatcher"),
            stream("notification"),
        ];
        assert_eq!(
            audio_stream_sources(&streams, false, &[], &[]),
            (vec![], vec![])
        );
    }

    #[test]
    fn transport_metadata_does_not_filter_real_media() {
        let mut vlc = stream("VLC");
        vlc.props
            .insert("client.api".into(), "pipewire-pulse".into());
        assert_eq!(
            audio_stream_sources(&[vlc], false, &[], &[]),
            (vec!["VLC".into()], vec![])
        );
    }
}
