// SPDX-License-Identifier: GPL-3.0-or-later

//! Host audio discovery and default-sink restoration.
//!
//! Sunshine creates virtual sinks (`sink-sunshine-stereo`, …) and switches the default
//! sink to them while streaming, so the host speakers go quiet and the capture has a
//! clean mix. If Sunshine is killed before it restores the previous sink, the desktop
//! stays on the virtual sink. Longcar snapshots the host sink before a screen starts and
//! puts it back when the last screen stops.

use std::process::Command;

use serde::{Deserialize, Serialize};

/// Prefix of the virtual sinks Sunshine creates on Linux.
pub const SUNSHINE_SINK_PREFIX: &str = "sink-sunshine-";

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("cannot run pw-dump: {0}")]
    Dump(String),
    #[error("cannot parse pw-dump output: {0}")]
    Parse(String),
    #[error("cannot change the default sink: {0}")]
    Restore(String),
}

/// One PipeWire sink.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSink {
    /// PipeWire node id, used by `wpctl set-default`.
    pub id: u32,
    /// `node.name`, what Sunshine's `audio_sink` expects.
    pub name: String,
    /// Human description (`node.description`, falling back to the name).
    pub description: String,
    pub is_default: bool,
    /// 0.0–1.0 gain of the sink.
    #[serde(default)]
    pub volume: f32,
    /// Whether the sink is muted (a 0.0 volume is also silent, but not muted).
    #[serde(default)]
    pub muted: bool,
}

impl AudioSink {
    /// True when this is one of Sunshine's loopback sinks.
    pub fn is_sunshine(&self) -> bool {
        is_sunshine_sink(&self.name)
    }
}

/// True for Sunshine's virtual sinks.
pub fn is_sunshine_sink(name: &str) -> bool {
    name.starts_with(SUNSHINE_SINK_PREFIX)
}

/// Parse `pw-dump` output into sinks, newest ids last.
pub fn parse_pw_dump(text: &str) -> Result<Vec<AudioSink>, AudioError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| AudioError::Parse(e.to_string()))?;
    let objects = value
        .as_array()
        .ok_or_else(|| AudioError::Parse("expected a JSON array".to_string()))?;

    let mut default_name: Option<String> = None;
    let mut sinks = Vec::new();

    for object in objects {
        match object.get("type").and_then(|t| t.as_str()) {
            Some("PipeWire:Interface:Metadata") => {
                if let Some(entries) = object.get("metadata").and_then(|m| m.as_array()) {
                    for entry in entries {
                        if entry.get("key").and_then(|k| k.as_str()) != Some("default.audio.sink") {
                            continue;
                        }
                        let value = entry.get("value");
                        // Either a plain name or `{"name": "..."}` depending on the version.
                        default_name = value
                            .and_then(|v| v.get("name").and_then(|n| n.as_str()))
                            .map(str::to_string)
                            .or_else(|| value.and_then(|v| v.as_str()).map(str::to_string));
                    }
                }
            }
            Some("PipeWire:Interface:Node") => {
                let Some(props) = object
                    .get("info")
                    .and_then(|info| info.get("props"))
                    .and_then(|props| props.as_object())
                else {
                    continue;
                };
                if props.get("media.class").and_then(|c| c.as_str()) != Some("Audio/Sink") {
                    continue;
                }
                let Some(id) = object.get("id").and_then(|id| id.as_u64()) else {
                    continue;
                };
                let Some(name) = props.get("node.name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let description = props
                    .get("node.description")
                    .and_then(|d| d.as_str())
                    .or_else(|| props.get("node.nick").and_then(|d| d.as_str()))
                    .unwrap_or(name);
                sinks.push(AudioSink {
                    id: id as u32,
                    name: name.to_string(),
                    description: description.to_string(),
                    is_default: false,
                    // Gain and mute live in the mixer, not in the dump.
                    volume: 1.0,
                    muted: false,
                });
            }
            _ => {}
        }
    }

    if let Some(default) = default_name {
        for sink in &mut sinks {
            if sink.name == default {
                sink.is_default = true;
            }
        }
    }
    sinks.sort_by_key(|sink| sink.id);
    Ok(sinks)
}

/// Read a sink's gain and mute state (`wpctl get-volume`).
pub fn read_volume(id: u32) -> Option<(f32, bool)> {
    let output = Command::new("wpctl")
        .args(["get-volume", &id.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // `Volume: 0.58` or `Volume: 0.00 [MUTED]`
    let value = text.split_whitespace().nth(1)?.parse::<f32>().ok()?;
    Some((value.clamp(0.0, 1.0), text.contains("[MUTED]")))
}

fn wpctl(args: &[String], action: &str) -> Result<(), AudioError> {
    let output = Command::new("wpctl")
        .args(args)
        .output()
        .map_err(|e| AudioError::Restore(format!("{action}: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(AudioError::Restore(format!(
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Set a sink's gain from a percentage (0–100).
pub fn set_volume_percent(id: u32, percent: u8) -> Result<(), AudioError> {
    let percent = percent.min(100);
    wpctl(
        &[
            "set-volume".to_string(),
            id.to_string(),
            format!("{percent}%"),
        ],
        "cannot set the sink volume",
    )
}

/// Mute or unmute a sink.
pub fn set_muted(id: u32, muted: bool) -> Result<(), AudioError> {
    wpctl(
        &[
            "set-mute".to_string(),
            id.to_string(),
            if muted { "1" } else { "0" }.to_string(),
        ],
        "cannot change the sink mute state",
    )
}

/// The host output as it was before a screen started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostAudioSnapshot {
    pub id: u32,
    pub name: String,
    pub volume: f32,
    pub muted: bool,
}

/// Capture the default sink, its gain and its mute state.
pub fn snapshot_host() -> Result<Option<HostAudioSnapshot>, AudioError> {
    let Some(sink) = default_sink()? else {
        return Ok(None);
    };
    if sink.is_sunshine() {
        return Ok(None);
    }
    let (volume, muted) = read_volume(sink.id).unwrap_or((sink.volume, sink.muted));
    Ok(Some(HostAudioSnapshot {
        id: sink.id,
        name: sink.name,
        volume,
        muted,
    }))
}

/// Put the default sink, gain and mute state back after streaming.
///
/// Sunshine switches the default sink to its virtual sink and mutes the physical one; a
/// killed stream leaves both behind, so restoring the snapshot is what makes the host
/// speakers audible again.
pub fn restore_host(snapshot: &HostAudioSnapshot) -> Result<(), AudioError> {
    set_default_sink(snapshot.id)?;
    set_volume_percent(snapshot.id, (snapshot.volume * 100.0).round() as u8)?;
    set_muted(snapshot.id, snapshot.muted)?;
    Ok(())
}

/// Every sink PipeWire exposes, with the default marked.
pub fn list_sinks() -> Result<Vec<AudioSink>, AudioError> {
    let output = Command::new("pw-dump")
        .output()
        .map_err(|e| AudioError::Dump(e.to_string()))?;
    if !output.status.success() {
        return Err(AudioError::Dump(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let mut sinks = parse_pw_dump(&String::from_utf8_lossy(&output.stdout))?;
    for sink in &mut sinks {
        if let Some((volume, muted)) = read_volume(sink.id) {
            sink.volume = volume;
            sink.muted = muted;
        }
    }
    Ok(sinks)
}

/// The current default sink.
pub fn default_sink() -> Result<Option<AudioSink>, AudioError> {
    Ok(list_sinks()?.into_iter().find(|sink| sink.is_default))
}

/// Make `id` the default sink (`wpctl set-default`).
pub fn set_default_sink(id: u32) -> Result<(), AudioError> {
    let output = Command::new("wpctl")
        .args(["set-default", &id.to_string()])
        .output()
        .map_err(|e| AudioError::Restore(e.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(AudioError::Restore(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

/// Put the host back on a physical sink when a stale Sunshine sink is still default.
///
/// Returns the sink that is default afterwards, or `None` when nothing had to change.
pub fn restore_host_default() -> Result<Option<AudioSink>, AudioError> {
    let sinks = list_sinks()?;
    let Some(current) = sinks.iter().find(|sink| sink.is_default) else {
        return Ok(None);
    };
    if !current.is_sunshine() {
        return Ok(None);
    }
    let Some(target) = sinks.iter().find(|sink| !sink.is_sunshine()) else {
        return Ok(None);
    };
    set_default_sink(target.id)?;
    Ok(Some(target.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"[
      {"id": 40, "type": "PipeWire:Interface:Metadata",
       "metadata": [
         {"subject": 0, "key": "default.configured.audio.sink", "value": {"name": "sink-sunshine-stereo"}},
         {"subject": 0, "key": "default.audio.sink", "value": {"name": "sink-sunshine-stereo"}}
       ]},
      {"id": 57, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Sink", "node.name": "alsa_output.speaker",
                          "node.description": "Ryzen Speaker"}}},
      {"id": 92, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Sink", "node.name": "sink-sunshine-stereo",
                          "node.description": "sink-sunshine-stereo"}}},
      {"id": 59, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.mic"}}},
      {"id": 60, "type": "PipeWire:Interface:Client", "info": {"props": {}}}
    ]"#;

    #[test]
    fn sinks_are_parsed_and_default_marked() {
        let sinks = parse_pw_dump(DUMP).unwrap();
        assert_eq!(sinks.len(), 2, "{sinks:?}");
        assert_eq!(sinks[0].id, 57);
        assert_eq!(sinks[0].name, "alsa_output.speaker");
        assert_eq!(sinks[0].description, "Ryzen Speaker");
        assert!(!sinks[0].is_default);
        assert!(sinks[1].is_default);
        // The dump itself carries no gain; `list_sinks` fills it from the mixer.
        assert_eq!(sinks[0].volume, 1.0);
        assert!(!sinks[0].muted);
        assert!(sinks[1].is_sunshine());
    }

    #[test]
    fn missing_metadata_leaves_no_default() {
        let text = r#"[{"id": 1, "type": "PipeWire:Interface:Node",
            "info": {"props": {"media.class": "Audio/Sink", "node.name": "x"}}}]"#;
        let sinks = parse_pw_dump(text).unwrap();
        assert_eq!(sinks.len(), 1);
        assert!(!sinks[0].is_default);
        // Without a description the name is used.
        assert_eq!(sinks[0].description, "x");
    }

    #[test]
    fn plain_string_metadata_is_accepted() {
        let text = r#"[{"id": 7, "type": "PipeWire:Interface:Metadata",
            "metadata": [{"key": "default.audio.sink", "value": "a"}]},
            {"id": 8, "type": "PipeWire:Interface:Node",
             "info": {"props": {"media.class": "Audio/Sink", "node.name": "a"}}}]"#;
        let sinks = parse_pw_dump(text).unwrap();
        assert!(sinks[0].is_default);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(matches!(
            parse_pw_dump("not json"),
            Err(AudioError::Parse(_))
        ));
        assert!(matches!(parse_pw_dump("{}"), Err(AudioError::Parse(_))));
    }

    #[test]
    fn volume_line_is_parsed() {
        // Pure parsing check: the same rule `read_volume` applies to wpctl output.
        for (line, expected) in [
            ("Volume: 0.58\n", (0.58_f32, false)),
            ("Volume: 0.00 [MUTED]\n", (0.0, true)),
            ("Volume: 1.00\n", (1.0, false)),
        ] {
            let value: f32 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
            assert_eq!((value, line.contains("[MUTED]")), expected, "{line}");
        }
    }

    #[test]
    fn sunshine_sink_prefix_is_recognised() {
        assert!(is_sunshine_sink("sink-sunshine-stereo"));
        assert!(is_sunshine_sink("sink-sunshine-surround51"));
        assert!(!is_sunshine_sink(
            "alsa_output.pci-0000_04_00.6.HiFi__Speaker__sink"
        ));
    }
}
