// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon state document (`schemaVersion = 1`).
//!
//! This is the single payload shared by D-Bus `GetState`/`StateChanged`, the
//! GUI, the Quick Settings extension and `longcarctl`. It is intentionally a
//! flat, serde-only description of the current world so that clients never need
//! to know about the daemon's internal types.

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::config::GeneralConfig;
use crate::model::{
    Alignment, Av1Mode, CursorMode, DaemonState, Encoder, HevcMode, Position, Rect, ScreenState,
    StageError,
};

/// Current state document schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Top-level state document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateDoc {
    pub schema_version: u32,
    pub daemon: DaemonStatus,
    #[serde(default)]
    pub screens: Vec<ScreenEntry>,
    #[serde(default)]
    pub physical_monitors: Vec<PhysicalEntry>,
    #[serde(default)]
    pub presets: Vec<PresetEntry>,
    pub settings: SettingsEntry,
    #[serde(default)]
    pub pending_pairings: Vec<PendingPairing>,
    #[serde(default)]
    pub devices: Vec<DeviceEntry>,
    pub host: HostEntry,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl Default for StateDoc {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            daemon: DaemonStatus::now(DaemonState::Idle),
            screens: Vec::new(),
            physical_monitors: Vec::new(),
            presets: Vec::new(),
            settings: SettingsEntry::default(),
            pending_pairings: Vec::new(),
            devices: Vec::new(),
            host: HostEntry::default(),
            warnings: Vec::new(),
        }
    }
}

impl StateDoc {
    /// Serialize to JSON; never fails for this document.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::from("{}"))
    }

    /// Parse a state document produced by [`StateDoc::to_json`].
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// Daemon-wide status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonStatus {
    pub state: DaemonState,
    /// When `state` was entered (RFC 3339, UTC, second precision).
    pub since: String,
}

impl DaemonStatus {
    /// Status for `state` entered right now.
    pub fn now(state: DaemonState) -> Self {
        Self {
            state,
            since: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        }
    }
}

impl Default for DaemonStatus {
    fn default() -> Self {
        Self::now(DaemonState::Idle)
    }
}

/// One configured screen plus its runtime fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenEntry {
    pub id: String,
    pub name: String,
    pub state: ScreenState,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    pub scale: f64,
    pub slot: u8,
    pub position: Position,
    pub relative_to: String,
    pub alignment: Alignment,
    pub encoder: Encoder,
    pub hevc: HevcMode,
    pub av1: Av1Mode,
    pub max_bitrate_kbps: u32,
    pub max_resolution: String,
    pub cursor: CursorMode,
    #[serde(default)]
    pub logical: Option<Rect>,
    #[serde(default)]
    pub connector: Option<String>,
    #[serde(default)]
    pub node_id: Option<u32>,
    pub base_port: u16,
    pub web_port: u16,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub bitrate_kbps: u32,
    #[serde(default)]
    pub fps: u32,
    #[serde(default)]
    pub error: Option<StageError>,
}

/// A physical monitor as currently laid out by Mutter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalEntry {
    pub connector: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
}

/// A saved screen set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetEntry {
    pub id: String,
    pub name: String,
    pub screens: Vec<String>,
}

/// General settings mirrored into the state document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsEntry {
    pub autostart_screens: bool,
    pub notifications: bool,
    pub stop_when_idle_minutes: u32,
    pub default_preset: String,
    pub audio_screen: String,
    pub restore_layout_on_stop: bool,
    pub onboarding_complete: bool,
}

impl Default for SettingsEntry {
    fn default() -> Self {
        Self::from(&GeneralConfig::default())
    }
}

impl From<&GeneralConfig> for SettingsEntry {
    fn from(general: &GeneralConfig) -> Self {
        Self {
            autostart_screens: general.autostart_screens,
            notifications: general.notifications,
            stop_when_idle_minutes: general.stop_when_idle_minutes,
            default_preset: general.default_preset.clone(),
            audio_screen: general.audio_screen.clone(),
            restore_layout_on_stop: general.restore_layout_on_stop,
            onboarding_complete: general.onboarding_complete,
        }
    }
}

impl SettingsEntry {
    /// Copy these settings back into a config's `[general]` section.
    pub fn apply(&self, general: &mut GeneralConfig) {
        general.autostart_screens = self.autostart_screens;
        general.notifications = self.notifications;
        general.stop_when_idle_minutes = self.stop_when_idle_minutes;
        general.default_preset = self.default_preset.clone();
        general.audio_screen = self.audio_screen.clone();
        general.restore_layout_on_stop = self.restore_layout_on_stop;
        general.onboarding_complete = self.onboarding_complete;
    }
}

/// A client waiting for PIN approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPairing {
    pub screen_id: String,
    pub id: String,
    pub name: String,
    pub address: String,
}

/// A paired client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEntry {
    pub screen_id: String,
    pub uuid: String,
    pub name: String,
    pub enabled: bool,
}

/// Host networking summary.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostEntry {
    #[serde(default)]
    pub addresses: Vec<HostAddressEntry>,
}

/// One local address clients can reach the host on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostAddressEntry {
    pub interface: String,
    pub address: String,
    /// `wifi`, `ethernet` or `other`, matching the network module's classifier.
    pub kind: String,
    pub preferred: bool,
}

/// One streamed session, appended as a JSON line to `sessions.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    pub screen_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub encoder: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub bitrate_samples: Vec<u32>,
    pub avg_bitrate_kbps: Option<u32>,
    pub max_bitrate_kbps: Option<u32>,
    pub reason: Option<String>,
}

impl SessionRecord {
    /// Close the session and derive average/max bitrate from the samples.
    ///
    /// The average is an integer mean; an empty sample list yields `0` rather
    /// than `null` so consumers can format it unconditionally.
    pub fn finalize(&mut self, ended_at: String, reason: &str) {
        let max = self.bitrate_samples.iter().copied().max().unwrap_or(0);
        let avg = if self.bitrate_samples.is_empty() {
            0
        } else {
            let sum: u64 = self
                .bitrate_samples
                .iter()
                .map(|sample| u64::from(*sample))
                .sum();
            (sum / self.bitrate_samples.len() as u64) as u32
        };
        self.ended_at = Some(ended_at);
        self.avg_bitrate_kbps = Some(avg);
        self.max_bitrate_kbps = Some(max);
        self.reason = Some(reason.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GeneralConfig;
    use crate::model::Stage;

    const DOC: &str = r#"{
        "schemaVersion": 1,
        "daemon": {"state": "idle", "since": "2026-11-21T12:00:00Z"},
        "screens": [{
            "id": "tablet", "name": "Tablet",
            "state": "off",
            "width": 2560, "height": 1600, "refreshHz": 120.0, "scale": 1.5,
            "slot": 0, "position": "right", "relativeTo": "primary", "alignment": "center",
            "encoder": "auto", "hevc": "auto", "av1": "off",
            "maxBitrateKbps": 30000, "maxResolution": "2560x1600", "cursor": "embedded",
            "logical": null,
            "connector": null, "nodeId": null,
            "basePort": 48049, "webPort": 48050,
            "connected": false, "bitrateKbps": 0, "fps": 0,
            "error": null
        }],
        "physicalMonitors": [{
            "connector": "eDP-1", "name": "Built-in display",
            "x": 0, "y": 0, "width": 1536, "height": 960,
            "scale": 1.25, "transform": 0, "primary": true
        }],
        "presets": [{"id": "tablet", "name": "Tablet", "screens": ["tablet"]}],
        "settings": {"autostartScreens": false, "notifications": true, "stopWhenIdleMinutes": 0, "defaultPreset": "tablet", "audioScreen": "tablet", "restoreLayoutOnStop": true, "onboardingComplete": false},
        "pendingPairings": [], "devices": [],
        "host": {"addresses": [{"interface": "wlp3s0", "address": "192.168.2.11", "kind": "wifi", "preferred": true}]},
        "warnings": []
    }"#;

    #[test]
    fn state_document_round_trip() {
        let parsed = StateDoc::from_json(DOC).expect("parse document");
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.screens.len(), 1);
        assert_eq!(parsed.screens[0].refresh_hz, 120.0);
        assert_eq!(parsed.screens[0].base_port, 48049);
        assert_eq!(parsed.physical_monitors[0].connector, "eDP-1");

        let emitted: serde_json::Value =
            serde_json::from_str(&parsed.to_json()).expect("re-parse emitted JSON");
        let expected: serde_json::Value = serde_json::from_str(DOC).expect("parse fixture");
        assert_eq!(emitted, expected);
    }

    #[test]
    fn partial_screen_json_deserializes() {
        let json = r#"{
            "schemaVersion": 1,
            "daemon": {"state": "active", "since": "2026-11-21T12:00:00Z"},
            "screens": [{
                "id": "tablet", "name": "Tablet", "state": "ready",
                "width": 2560, "height": 1600, "refreshHz": 120.0, "scale": 1.5,
                "slot": 0, "position": "right", "relativeTo": "primary",
                "alignment": "center", "encoder": "auto", "hevc": "auto", "av1": "off",
                "maxBitrateKbps": 30000, "maxResolution": "2560x1600", "cursor": "embedded",
                "basePort": 48049, "webPort": 48050
            }],
            "settings": {
                "autostartScreens": false, "notifications": true,
                "stopWhenIdleMinutes": 0, "defaultPreset": "tablet",
                "audioScreen": "tablet", "restoreLayoutOnStop": true,
                "onboardingComplete": false
            },
            "host": {"addresses": []}
        }"#;
        let doc = StateDoc::from_json(json).expect("partial document");
        let screen = &doc.screens[0];
        assert_eq!(screen.state, ScreenState::Ready);
        assert!(screen.logical.is_none());
        assert!(screen.connector.is_none());
        assert!(screen.node_id.is_none());
        assert!(!screen.connected);
        assert_eq!(screen.bitrate_kbps, 0);
        assert_eq!(screen.fps, 0);
        assert!(screen.error.is_none());
        assert!(doc.physical_monitors.is_empty());
        assert!(doc.warnings.is_empty());
    }

    #[test]
    fn settings_general_config_round_trip() {
        let general = GeneralConfig {
            autostart_screens: true,
            notifications: false,
            stop_when_idle_minutes: 15,
            default_preset: "phone".to_string(),
            audio_screen: "tablet".to_string(),
            restore_layout_on_stop: false,
            onboarding_complete: true,
        };
        let entry = SettingsEntry::from(&general);
        assert_eq!(entry.stop_when_idle_minutes, 15);
        assert!(entry.autostart_screens);
        assert!(!entry.notifications);

        let json = serde_json::to_string(&entry).expect("serialize settings");
        let parsed: SettingsEntry = serde_json::from_str(&json).expect("parse settings");
        assert_eq!(parsed, entry);

        let mut target = GeneralConfig::default();
        parsed.apply(&mut target);
        assert_eq!(target, general);
    }

    #[test]
    fn session_finalize_averages_samples() {
        let mut record = SessionRecord {
            id: "s1".to_string(),
            screen_id: "tablet".to_string(),
            started_at: "2026-11-21T12:00:00Z".to_string(),
            ended_at: None,
            encoder: Some("vaapi".to_string()),
            width: Some(2560),
            height: Some(1600),
            fps: Some(60),
            bitrate_samples: vec![10_000, 20_000, 30_000, 21_000],
            avg_bitrate_kbps: None,
            max_bitrate_kbps: None,
            reason: None,
        };
        record.finalize("2026-11-21T13:00:00Z".to_string(), "disconnected");
        assert_eq!(record.avg_bitrate_kbps, Some(20_250));
        assert_eq!(record.max_bitrate_kbps, Some(30_000));
        assert_eq!(record.ended_at.as_deref(), Some("2026-11-21T13:00:00Z"));
        assert_eq!(record.reason.as_deref(), Some("disconnected"));

        let json = serde_json::to_string(&record).expect("serialize record");
        let expected = r#"{"id":"s1","screenId":"tablet","startedAt":"2026-11-21T12:00:00Z","endedAt":"2026-11-21T13:00:00Z","encoder":"vaapi","width":2560,"height":1600,"fps":60,"bitrateSamples":[10000,20000,30000,21000],"avgBitrateKbps":20250,"maxBitrateKbps":30000,"reason":"disconnected"}"#;
        assert_eq!(json, expected);
    }

    #[test]
    fn session_finalize_empty_samples() {
        let mut record = SessionRecord {
            id: "s2".to_string(),
            screen_id: "tablet".to_string(),
            started_at: "2026-11-21T12:00:00Z".to_string(),
            ended_at: None,
            encoder: None,
            width: None,
            height: None,
            fps: None,
            bitrate_samples: Vec::new(),
            avg_bitrate_kbps: None,
            max_bitrate_kbps: None,
            reason: None,
        };
        record.finalize("2026-11-21T12:01:00Z".to_string(), "client never connected");
        assert_eq!(record.avg_bitrate_kbps, Some(0));
        assert_eq!(record.max_bitrate_kbps, Some(0));
    }

    #[test]
    fn daemon_state_strings_match_schema() {
        let cases = [
            (DaemonState::Idle, "idle"),
            (DaemonState::Starting, "starting"),
            (DaemonState::Active, "active"),
            (DaemonState::Degraded, "degraded"),
            (DaemonState::Stopping, "stopping"),
        ];
        for (state, text) in cases {
            assert_eq!(state.as_str(), text);
            let json = serde_json::to_string(&DaemonStatus {
                state,
                since: "2026-11-21T12:00:00Z".to_string(),
            })
            .expect("serialize status");
            let value: serde_json::Value = serde_json::from_str(&json).expect("parse status");
            assert_eq!(value["state"], serde_json::Value::String(text.to_string()));
            assert_eq!(value["since"], "2026-11-21T12:00:00Z");
        }
    }

    #[test]
    fn default_document_has_schema_version() {
        let doc = StateDoc::default();
        assert_eq!(doc.schema_version, SCHEMA_VERSION);
        assert_eq!(doc.daemon.state, DaemonState::Idle);
        assert!(doc.screens.is_empty());
        assert!(doc.presets.is_empty());
        assert_eq!(doc.settings, SettingsEntry::from(&GeneralConfig::default()));
        let error_round_trip =
            serde_json::from_str::<StateDoc>(&doc.to_json()).expect("round trip");
        assert_eq!(error_round_trip.screens.len(), 0);
    }

    #[test]
    fn stage_error_keeps_pascal_case_stage() {
        let entry = ScreenEntry {
            id: "tablet".to_string(),
            name: "Tablet".to_string(),
            state: ScreenState::Error,
            width: 2560,
            height: 1600,
            refresh_hz: 120.0,
            scale: 1.5,
            slot: 0,
            position: Position::Right,
            relative_to: "primary".to_string(),
            alignment: Alignment::Center,
            encoder: Encoder::Auto,
            hevc: HevcMode::Auto,
            av1: Av1Mode::Off,
            max_bitrate_kbps: 30000,
            max_resolution: "2560x1600".to_string(),
            cursor: CursorMode::Embedded,
            logical: Some(Rect::new(1536, 0, 1707, 1067)),
            connector: Some("Meta-0".to_string()),
            node_id: Some(42),
            base_port: 48049,
            web_port: 48050,
            connected: true,
            bitrate_kbps: 25_000,
            fps: 60,
            error: Some(StageError::new(Stage::PortalAuth, "timed out")),
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&entry).expect("serialize"))
                .expect("parse");
        assert_eq!(value["error"]["stage"], "PortalAuth");
        assert_eq!(value["logical"]["x"], 1536);
        assert_eq!(value["nodeId"], 42);
    }
}
