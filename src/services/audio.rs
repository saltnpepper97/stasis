// Author: Dustin Pilgrim
// License: GPL-3.0-only

//! Read audio state through the native server's tools. PipeWire never needs a
//! PulseAudio connection; the PulseAudio fallback rejects compatibility servers.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::Command;

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
}

impl AudioReader {
    pub(super) fn backend_name(&self) -> &'static str {
        self.backend.map_or("unavailable", AudioBackend::name)
    }

    pub(super) async fn read(&mut self, direction: Direction) -> Result<Vec<AudioStream>, String> {
        if let Some(backend) = self.backend {
            match read_backend(backend, direction).await {
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

        let pipewire_error = match read_backend(AudioBackend::PipeWire, direction).await {
            Ok(streams) => {
                self.backend = Some(AudioBackend::PipeWire);
                self.retry_at = None;
                return Ok(streams);
            }
            Err(error) => error,
        };
        match read_backend(AudioBackend::PulseAudio, direction).await {
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
}

async fn read_backend(
    backend: AudioBackend,
    direction: Direction,
) -> Result<Vec<AudioStream>, String> {
    match backend {
        AudioBackend::PipeWire => parse_pipewire(&query("pw-dump", &["--no-colors"]).await?),
        AudioBackend::PulseAudio => {
            // Recheck the endpoint on every read, including after a server
            // restart. Never query streams through pipewire-pulse.
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
    }
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

fn parse_pipewire(text: &[u8]) -> Result<Vec<AudioStream>, String> {
    let objects: Vec<Value> =
        serde_json::from_slice(text).map_err(|e| format!("pw-dump JSON: {e}"))?;
    let clients: HashMap<String, HashMap<String, String>> = objects
        .iter()
        .filter(|object| object["type"] == "PipeWire:Interface:Client")
        .map(|object| {
            (
                object["id"].to_string(),
                properties(&object["info"]["props"]),
            )
        })
        .collect();
    let mut streams = Vec::new();
    for object in &objects {
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
    Ok(streams)
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
        println!(
            "backend={}; streams={}",
            reader.backend_name(),
            streams.len()
        );
    }
}
