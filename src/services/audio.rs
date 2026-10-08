// Author: Dustin Pilgrim
// License: GPL-3.0-only

//! Read audio state through the native server's tools. PipeWire never needs a
//! PulseAudio connection; the PulseAudio fallback rejects compatibility servers.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioBackend {
    PipeWire,
    PulseAudio,
}

impl AudioBackend {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::PipeWire => "pipewire (pw-dump)",
            Self::PulseAudio => "pulseaudio (pactl)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    Playback,
    Capture,
}

#[derive(Debug, Clone)]
pub(super) struct AudioStream {
    pub direction: Direction,
    pub running: bool,
    pub muted: bool,
    pub props: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub(super) struct AudioReader {
    backend: Option<AudioBackend>,
    retry_at: Option<Instant>,
    discovery_error: Option<String>,
    pipewire: Option<PipeWireMonitor>,
}

impl AudioReader {
    pub(super) fn backend_name(&self) -> &'static str {
        self.backend.map_or("unavailable", AudioBackend::name)
    }

    pub(super) async fn read(&mut self, direction: Direction) -> Result<Vec<AudioStream>, String> {
        if let Some(backend) = self.backend {
            match self.read_backend(backend, direction).await {
                Ok(streams) => return Ok(streams),
                Err(error) => {
                    self.backend = None;
                    // Keep the failed observation unknown and retry discovery
                    // on the next poll, rather than interpreting it as silence.
                    self.retry_at = None;
                    return Err(error);
                }
            }
        }
        if self.retry_at.is_some_and(|retry| Instant::now() < retry) {
            return Err(self
                .discovery_error
                .clone()
                .unwrap_or_else(|| "no native audio backend".into()));
        }

        let pipewire_error = match self.read_backend(AudioBackend::PipeWire, direction).await {
            Ok(streams) => {
                self.backend = Some(AudioBackend::PipeWire);
                self.retry_at = None;
                return Ok(streams);
            }
            Err(error) => error,
        };
        match self.read_backend(AudioBackend::PulseAudio, direction).await {
            Ok(streams) => {
                self.backend = Some(AudioBackend::PulseAudio);
                self.retry_at = None;
                Ok(streams)
            }
            Err(error) => {
                self.retry_at = Some(Instant::now() + Duration::from_secs(5));
                let error = format!("native audio unavailable: {pipewire_error}; {error}");
                self.discovery_error = Some(error.clone());
                Err(error)
            }
        }
    }

    async fn read_backend(
        &mut self,
        backend: AudioBackend,
        direction: Direction,
    ) -> Result<Vec<AudioStream>, String> {
        match backend {
            AudioBackend::PipeWire => {
                if self.pipewire.is_none() {
                    self.pipewire = Some(PipeWireMonitor::start()?);
                }
                let result = self.pipewire.as_mut().unwrap().read().await;
                if result.is_err() {
                    // A reconnect starts with a fresh registry. Preserve the
                    // consumer's last observation while this one is unknown.
                    self.pipewire = None;
                }
                result
            }
            AudioBackend::PulseAudio => read_pulse(direction).await,
        }
    }
}

// Keep the native connection open instead of launching and parsing a complete
// PipeWire registry on every media/microphone poll.
#[derive(Debug)]
struct PipeWireMonitor {
    snapshots: watch::Receiver<Option<Result<Vec<AudioStream>, String>>>,
    task: JoinHandle<()>,
}

impl PipeWireMonitor {
    fn start() -> Result<Self, String> {
        let mut child = Command::new("pw-dump")
            .args(["--monitor", "--no-colors"])
            .env("LC_ALL", "C")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("pw-dump monitor: {error}"))?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let (tx, snapshots) = watch::channel(None);
        let task = tokio::spawn(async move {
            let mut registry = HashMap::new();
            let mut pending = Vec::new();
            let mut chunk = [0u8; 16384];
            let result = async {
                loop {
                    let count = stdout
                        .read(&mut chunk)
                        .await
                        .map_err(|error| format!("pw-dump monitor read: {error}"))?;
                    if count == 0 {
                        return Err("pw-dump monitor disconnected".to_string());
                    }
                    pending.extend_from_slice(&chunk[..count]);
                    for changes in take_pipewire_batches(&mut pending)? {
                        update_pipewire_registry(&mut registry, changes)?;
                        let streams = pipewire_streams(registry.values());
                        if tx.send(Some(Ok(streams))).is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            .await;
            if let Err(error) = result {
                let _ = tx.send(Some(Err(error)));
            }
            // Explicitly reap the helper on an error or receiver shutdown.
            let _ = child.kill().await;
            let _ = child.wait().await;
        });
        Ok(Self { snapshots, task })
    }

    async fn read(&mut self) -> Result<Vec<AudioStream>, String> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if self.snapshots.has_changed().is_err() {
                    return Err("pw-dump monitor stopped".into());
                }
                if let Some(snapshot) = self.snapshots.borrow().clone() {
                    return snapshot;
                }
                self.snapshots
                    .changed()
                    .await
                    .map_err(|_| "pw-dump monitor stopped".to_string())?;
            }
        })
        .await
        .map_err(|_| "pw-dump monitor initial snapshot timed out".to_string())?
    }
}

impl Drop for PipeWireMonitor {
    fn drop(&mut self) {
        self.task.abort(); // Dropping the task also kills its owned child.
    }
}

fn take_pipewire_batches(pending: &mut Vec<u8>) -> Result<Vec<Vec<Value>>, String> {
    // Bound an incomplete/malformed document rather than growing indefinitely.
    if pending.len() > 16 * 1024 * 1024 {
        return Err("pw-dump monitor snapshot exceeds 16 MiB".into());
    }
    let mut decoder = serde_json::Deserializer::from_slice(pending).into_iter::<Vec<Value>>();
    let mut batches = Vec::new();
    let mut consumed = 0;
    while let Some(batch) = decoder.next() {
        match batch {
            Ok(batch) => {
                batches.push(batch);
                consumed = decoder.byte_offset();
            }
            Err(error) if error.is_eof() => break,
            Err(error) => return Err(format!("pw-dump monitor JSON: {error}")),
        }
    }
    pending.drain(..consumed);
    Ok(batches)
}

fn update_pipewire_registry(
    registry: &mut HashMap<u64, Value>,
    changes: Vec<Value>,
) -> Result<(), String> {
    for object in changes {
        let id = object["id"]
            .as_u64()
            .ok_or("pw-dump monitor object has no id")?;
        if object.get("info").is_some_and(Value::is_null) {
            registry.remove(&id);
        } else if matches!(
            object["type"].as_str(),
            Some("PipeWire:Interface:Client" | "PipeWire:Interface:Node")
        ) {
            // pw-dump publishes the full current object for a change, and an
            // id/info:null tombstone when it disappears. Ignore ports/devices.
            registry.insert(id, object);
        }
    }
    Ok(())
}

async fn read_pulse(direction: Direction) -> Result<Vec<AudioStream>, String> {
    // Recheck the endpoint on every read, including after a server restart.
    // Never query streams through pipewire-pulse.
    let info = query("pactl", &["--format=json", "info"]).await?;
    require_native_pulse(&info)?;
    let kind = match direction {
        Direction::Playback => "sink-inputs",
        Direction::Capture => "source-outputs",
    };
    parse_pulse(
        &query("pactl", &["--format=json", "list", kind]).await?,
        direction,
    )
}

async fn query(program: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut command = Command::new(program);
    // Other session/server environment is inherited, including PIPEWIRE_REMOTE
    // and PULSE_SERVER. Only diagnostic localization is fixed here.
    command.args(args).env("LC_ALL", "C").kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(2), command.output())
        .await
        .map_err(|_| format!("{program} query timed out"))?
        .map_err(|error| format!("{program} query: {error}"))?;
    if !output.status.success() {
        return Err(format!("{program} query failed ({})", output.status));
    }
    Ok(output.stdout)
}

fn require_native_pulse(text: &[u8]) -> Result<(), String> {
    let info: Value = serde_json::from_slice(text).map_err(|e| format!("pactl info JSON: {e}"))?;
    match info.get("server_name").and_then(Value::as_str) {
        Some(name) if name.eq_ignore_ascii_case("pulseaudio") => Ok(()),
        _ => Err(
            "pactl endpoint is not a native PulseAudio server (pipewire-pulse is unsupported)"
                .into(),
        ),
    }
}

fn properties(value: &Value) -> HashMap<String, String> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Bool(_) | Value::Number(_) => value.to_string(),
                _ => return None,
            };
            Some((key.to_ascii_lowercase(), text))
        })
        .collect()
}

fn bool_value(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| match value.as_str()? {
        "true" | "yes" => Some(true),
        "false" | "no" => Some(false),
        _ => None,
    })
}

#[cfg(test)]
fn parse_pipewire(text: &[u8]) -> Result<Vec<AudioStream>, String> {
    let objects: Vec<Value> =
        serde_json::from_slice(text).map_err(|e| format!("pw-dump JSON: {e}"))?;
    Ok(pipewire_streams(objects.iter()))
}

fn pipewire_streams<'a>(objects: impl Iterator<Item = &'a Value> + Clone) -> Vec<AudioStream> {
    let clients: HashMap<String, HashMap<String, String>> = objects
        .clone()
        .filter(|object| object["type"] == "PipeWire:Interface:Client")
        .map(|object| {
            (
                object["id"].to_string(),
                properties(&object["info"]["props"]),
            )
        })
        .collect();
    let mut streams = Vec::new();
    for object in objects {
        if object["type"] != "PipeWire:Interface:Node" {
            continue;
        }
        let info = &object["info"];
        let node_props = properties(&info["props"]);
        let direction = match node_props.get("media.class").map(String::as_str) {
            Some("Stream/Output/Audio") => Direction::Playback,
            Some("Stream/Input/Audio") => Direction::Capture,
            _ => continue,
        };
        let mut props = node_props
            .get("client.id")
            .and_then(|id| clients.get(id))
            .cloned()
            .unwrap_or_default();
        props.extend(node_props);
        let muted = info["params"]["Props"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|param| {
                bool_value(&param["mute"]) == Some(true)
                    || bool_value(&param["softMute"]) == Some(true)
            });
        let corked = props
            .get("pulse.corked")
            .is_some_and(|v| v == "true" || v == "yes");
        streams.push(AudioStream {
            direction,
            running: info["state"] == "running" && !corked,
            muted,
            props,
        });
    }
    streams
}

fn parse_pulse(text: &[u8], direction: Direction) -> Result<Vec<AudioStream>, String> {
    let inputs: Vec<Value> =
        serde_json::from_slice(text).map_err(|e| format!("pactl streams JSON: {e}"))?;
    Ok(inputs
        .iter()
        .map(|input| AudioStream {
            direction,
            // Missing cork/mute state is unknown, not evidence of playback.
            running: bool_value(&input["corked"]) == Some(false),
            muted: bool_value(&input["mute"]) != Some(false),
            props: properties(&input["properties"]),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(class: &str, state: &str, muted: bool) -> Value {
        json!({"type":"PipeWire:Interface:Node", "info": {
            "state":state, "props":{"media.class":class,"application.name":"VLC"},
            "params":{"Props":[{"mute":muted}]}
        }})
    }

    #[test]
    fn monitor_decodes_fragmented_consecutive_batches() {
        let first = json!([{"id":12,"info":{"props":{"application.name":"a ] \\\" b"}}}]);
        let second = json!([{"id":12,"info":null}]);
        let wire = format!("{}\n{}\n", first, second);
        let mut pending = Vec::new();
        let mut batches = Vec::new();
        for byte in wire.bytes() {
            pending.push(byte);
            batches.extend(take_pipewire_batches(&mut pending).unwrap());
        }
        assert_eq!(
            batches,
            vec![
                first.as_array().unwrap().clone(),
                second.as_array().unwrap().clone()
            ]
        );
        assert!(pending.iter().all(u8::is_ascii_whitespace));

        let mut pending = wire.into_bytes();
        assert_eq!(take_pipewire_batches(&mut pending).unwrap(), batches);
    }

    #[test]
    fn monitor_rejects_invalid_and_unbounded_documents() {
        assert!(take_pipewire_batches(&mut b"invalid".to_vec()).is_err());
        assert!(take_pipewire_batches(&mut vec![b' '; 16 * 1024 * 1024 + 1]).is_err());
        let mut incomplete = br#"[{"id":12,"info":{"props": "#.to_vec();
        assert!(take_pipewire_batches(&mut incomplete).unwrap().is_empty());
        assert!(!incomplete.is_empty());
    }

    #[test]
    fn monitor_reconciles_state_identity_removals_and_reused_ids() {
        let mut registry = HashMap::new();
        let mut stream = node("Stream/Input/Audio", "running", false);
        stream["id"] = json!(5);
        stream["info"]["props"] = json!({"media.class":"Stream/Input/Audio","client.id":12});
        let client = json!({"id":12,"type":"PipeWire:Interface:Client","info":{"props":{"application.process.binary":"firefox"}}});
        update_pipewire_registry(&mut registry, vec![client.clone(), stream.clone()]).unwrap();
        let streams = pipewire_streams(registry.values());
        assert!(streams[0].running && !streams[0].muted);
        assert_eq!(streams[0].props["application.process.binary"], "firefox");

        stream["info"]["state"] = json!("idle");
        stream["info"]["params"]["Props"][0]["mute"] = json!(true);
        update_pipewire_registry(&mut registry, vec![stream]).unwrap();
        let streams = pipewire_streams(registry.values());
        assert!(!streams[0].running && streams[0].muted);
        update_pipewire_registry(
            &mut registry,
            vec![json!({"id":5,"info":null}), json!({"id":12,"info":null})],
        )
        .unwrap();
        assert!(pipewire_streams(registry.values()).is_empty());

        let mut replacement = node("Stream/Output/Audio", "running", false);
        replacement["id"] = json!(5);
        update_pipewire_registry(&mut registry, vec![replacement]).unwrap();
        let streams = pipewire_streams(registry.values());
        assert_eq!(streams[0].direction, Direction::Playback);
        assert!(!streams[0].props.contains_key("application.process.binary"));
        assert!(update_pipewire_registry(&mut registry, vec![json!({"info":null})]).is_err());
    }

    #[tokio::test]
    async fn disconnected_monitor_does_not_return_its_last_snapshot_as_current() {
        let (tx, snapshots) = watch::channel(Some(Ok(Vec::new())));
        let task = tokio::spawn(std::future::pending());
        let mut monitor = PipeWireMonitor { snapshots, task };
        assert!(monitor.read().await.unwrap().is_empty());
        drop(tx);
        assert!(monitor.read().await.unwrap_err().contains("stopped"));
    }

    #[test]
    fn pipewire_selects_audio_streams_and_preserves_state() {
        let objects = json!([
            node("Audio/Sink", "running", false),
            node("Video/Source", "running", false),
            node("Stream/Output/Audio", "running", false),
            node("Stream/Output/Audio", "idle", false),
            node("Stream/Output/Audio", "running", true),
            node("Stream/Input/Audio", "running", false)
        ]);
        let streams = parse_pipewire(&serde_json::to_vec(&objects).unwrap()).unwrap();
        assert_eq!(streams.len(), 4);
        assert!(streams[0].running && !streams[0].muted);
        assert!(!streams[1].running);
        assert!(streams[2].muted);
        assert_eq!(streams[3].direction, Direction::Capture);
    }

    #[test]
    fn pipewire_merges_client_identity_and_respects_cork_soft_mute() {
        let mut stream = node("Stream/Output/Audio", "running", false);
        stream["info"]["props"] = json!({"media.class":"Stream/Output/Audio","client.id":12,"application.name":"Spotify","pulse.corked":true});
        stream["info"]["params"]["Props"][0]["softMute"] = json!(true);
        let clients = json!([
            {"type":"PipeWire:Interface:Client","id":12,"info":{"props":{"application.name":"Client name","application.process.binary":"spotify","application.process.id":123}}},
            stream
        ]);
        let streams = parse_pipewire(&serde_json::to_vec(&clients).unwrap()).unwrap();
        assert_eq!(streams[0].props["application.name"], "Spotify");
        assert_eq!(streams[0].props["application.process.id"], "123");
        assert_eq!(streams[0].props["application.process.binary"], "spotify");
        assert!(!streams[0].running);
        assert!(streams[0].muted);
    }

    #[test]
    fn pulse_compatibility_servers_are_rejected() {
        assert!(require_native_pulse(br#"{"server_name":"pulseaudio"}"#).is_ok());
        for text in [
            br#"{"server_name":"PulseAudio (on PipeWire 1.6.9)"}"#.as_slice(),
            br#"{}"#,
            b"invalid",
        ] {
            assert!(require_native_pulse(text).is_err());
        }
    }

    #[test]
    fn pulse_requires_known_uncorked_unmuted_state() {
        let streams = parse_pulse(
            br#"[
            {"corked":false,"mute":false,"properties":{"application.name":"VLC"}},
            {"corked":true,"mute":false},
            {"corked":false,"mute":true},
            {"properties":{"application.name":"Unknown"}}
        ]"#,
            Direction::Playback,
        )
        .unwrap();
        assert!(streams[0].running && !streams[0].muted);
        assert!(!streams[1].running);
        assert!(streams[2].muted);
        assert!(!streams[3].running && streams[3].muted);
    }

    #[test]
    fn malformed_snapshots_are_errors_not_silence() {
        for text in [b"invalid".as_slice(), b"{}"] {
            assert!(parse_pipewire(text).is_err());
            assert!(parse_pulse(text, Direction::Capture).is_err());
        }
        assert!(parse_pipewire(b"[]").unwrap().is_empty());
    }

    #[tokio::test]
    async fn hung_audio_commands_time_out_without_blocking_the_runtime() {
        let started = Instant::now();
        let command = query("sleep", &["10"]);
        tokio::pin!(command);
        tokio::select! {
            result = &mut command => panic!("command ended before the timer: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(25)) => {},
        }
        assert!(command.await.unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[tokio::test]
    #[ignore = "requires a running local PipeWire server and pw-dump"]
    async fn local_native_pipewire_probe() {
        let mut reader = AudioReader::default();
        let streams = reader.read(Direction::Playback).await.unwrap();
        assert_eq!(reader.backend, Some(AudioBackend::PipeWire));
        let monitor = reader.pipewire.as_ref().unwrap().task.id();
        reader.read(Direction::Capture).await.unwrap();
        reader.read(Direction::Playback).await.unwrap();
        assert_eq!(reader.pipewire.as_ref().unwrap().task.id(), monitor);
        println!(
            "backend={}; streams={}",
            reader.backend_name(),
            streams.len()
        );
    }
}
