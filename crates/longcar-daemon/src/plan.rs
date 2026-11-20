// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure helpers behind the orchestrator: layout inputs, quick screens, derived state,
//! and session history.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use longcar_core::config::{Config, Preset, ScreenConfig, create_private_dir, slugify};
use longcar_core::layout::{PhysicalMonitor, VirtualMonitor};
use longcar_core::model::{
    Alignment, Av1Mode, CursorMode, DaemonState, DisplayState, Encoder, HevcMode, Position, Rect,
    ScreenState, StageError,
};
use longcar_core::state::{
    DeviceEntry, HostAddressEntry, HostEntry, PendingPairing, PhysicalEntry, PresetEntry,
    ScreenEntry, SessionRecord, SettingsEntry,
};

use crate::error::DaemonError;
use crate::logbuf::LogBuffer;

/// Appearance of an active virtual output, used to replan the layout.
#[derive(Debug, Clone)]
pub struct VirtualSeed {
    pub screen_id: String,
    pub connector: String,
    pub mode_id: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub position: Position,
    pub relative_to: String,
    pub alignment: Alignment,
    pub physical_width: u32,
    pub physical_height: u32,
    pub supported_scales: Vec<f64>,
    pub modes: Vec<longcar_core::model::MonitorMode>,
}

impl VirtualSeed {
    pub fn from_screen(screen: &ScreenConfig, connector: &str, mode_id: &str) -> Self {
        Self {
            screen_id: screen.id.clone(),
            connector: connector.to_string(),
            mode_id: mode_id.to_string(),
            width: screen.width,
            height: screen.height,
            scale: screen.scale,
            position: screen.position,
            relative_to: screen.relative_to.clone(),
            alignment: screen.alignment,
            physical_width: screen.width,
            physical_height: screen.height,
            supported_scales: vec![screen.scale],
            modes: Vec::new(),
        }
    }
}

/// Build the planner's physical-monitor input from DisplayConfig's current state.
pub fn physical_monitors(state: &DisplayState) -> Vec<PhysicalMonitor> {
    let mut out = Vec::new();
    for logical in &state.logical_monitors {
        for reference in &logical.monitors {
            if reference.connector.starts_with("Meta-") {
                continue;
            }
            let Some(info) = state.connector(&reference.connector) else {
                continue;
            };
            let mode = info
                .modes
                .iter()
                .find(|m| m.id == reference.mode_id)
                .or_else(|| info.modes.first());
            let (width, height) = mode.map(|m| (m.width, m.height)).unwrap_or((0, 0));
            let logical_width = (width as f64 / logical.scale).round() as i32;
            let logical_height = (height as f64 / logical.scale).round() as i32;
            out.push(PhysicalMonitor {
                connector: reference.connector.clone(),
                name: info.display_name(),
                rect: Rect::new(logical.x, logical.y, logical_width, logical_height),
                scale: logical.scale,
                transform: logical.transform,
                primary: logical.primary,
                mode_id: reference.mode_id.clone(),
                physical_width: width,
                physical_height: height,
                supported_scales: mode.map(|m| m.supported_scales.clone()).unwrap_or_default(),
                modes: info.modes.clone(),
            });
        }
    }
    out
}

/// Build the planner's virtual-screen input from active outputs.
pub fn virtual_monitors(seeds: &[VirtualSeed]) -> Vec<VirtualMonitor> {
    seeds
        .iter()
        .map(|seed| VirtualMonitor {
            screen_id: seed.screen_id.clone(),
            connector: seed.connector.clone(),
            mode_id: seed.mode_id.clone(),
            width: seed.width,
            height: seed.height,
            scale: seed.scale,
            position: seed.position,
            relative_to: seed.relative_to.clone(),
            alignment: seed.alignment,
            physical_width: seed.physical_width,
            physical_height: seed.physical_height,
            supported_scales: seed.supported_scales.clone(),
            modes: seed.modes.clone(),
        })
        .collect()
}

/// Physical monitors as published in the state document.
pub fn physical_entries(state: &DisplayState) -> Vec<PhysicalEntry> {
    physical_monitors(state)
        .into_iter()
        .map(|monitor| PhysicalEntry {
            connector: monitor.connector,
            name: monitor.name,
            x: monitor.rect.x,
            y: monitor.rect.y,
            width: monitor.rect.width,
            height: monitor.rect.height,
            scale: monitor.scale,
            transform: monitor.transform,
            primary: monitor.primary,
        })
        .collect()
}

/// Derive the aggregate daemon state from screens and pending work.
pub fn derive_daemon_state(
    screens: &[ScreenState],
    layout_restore_available: bool,
    stopping_all: bool,
    work_in_flight: bool,
) -> DaemonState {
    if stopping_all {
        return DaemonState::Stopping;
    }
    if work_in_flight {
        return DaemonState::Starting;
    }
    if layout_restore_available || screens.contains(&ScreenState::Error) {
        return DaemonState::Degraded;
    }
    if screens
        .iter()
        .any(|s| matches!(s, ScreenState::Ready | ScreenState::Streaming))
    {
        return DaemonState::Active;
    }
    DaemonState::Idle
}

/// Build a screen entry from config plus runtime values.
#[allow(clippy::too_many_arguments)]
pub fn screen_entry(
    screen: &ScreenConfig,
    state: ScreenState,
    logical: Option<Rect>,
    connector: Option<String>,
    node_id: Option<u32>,
    connected: bool,
    bitrate_kbps: u32,
    fps: u32,
    error: Option<StageError>,
) -> ScreenEntry {
    let ports = longcar_core::ports::PortFamily::for_slot(screen.slot).ok();
    ScreenEntry {
        id: screen.id.clone(),
        name: screen.name.clone(),
        state,
        width: screen.width,
        height: screen.height,
        refresh_hz: screen.refresh_hz,
        scale: screen.scale,
        slot: screen.slot,
        position: screen.position,
        relative_to: screen.relative_to.clone(),
        alignment: screen.alignment,
        encoder: screen.encoder,
        hevc: screen.hevc,
        av1: screen.av1,
        max_bitrate_kbps: screen.max_bitrate_kbps,
        max_resolution: screen.max_resolution.clone(),
        cursor: screen.cursor,
        logical,
        connector,
        node_id,
        base_port: ports.as_ref().map(|p| p.base).unwrap_or(0),
        web_port: ports.as_ref().map(|p| p.web).unwrap_or(0),
        connected,
        bitrate_kbps,
        fps,
        error,
    }
}

/// Presets as published in the state document.
pub fn preset_entries(config: &Config) -> Vec<PresetEntry> {
    config
        .presets
        .iter()
        .map(|preset| PresetEntry {
            id: preset.id.clone(),
            name: preset.name.clone(),
            screens: preset.screens.clone(),
        })
        .collect()
}

/// Settings as published in the state document.
pub fn settings_entry(config: &Config) -> SettingsEntry {
    SettingsEntry::from(&config.general)
}

/// Host addresses as published in the state document.
pub fn host_entry() -> HostEntry {
    HostEntry {
        addresses: longcar_core::network::host_addresses()
            .into_iter()
            .map(|address| HostAddressEntry {
                interface: address.interface,
                address: address.address,
                kind: address.kind.as_str().to_string(),
                preferred: address.preferred,
            })
            .collect(),
    }
}

/// Warnings sorted for stable output.
pub fn warnings_vec(warnings: &BTreeSet<String>) -> Vec<String> {
    warnings.iter().cloned().collect()
}

/// Screen ids in a preset, filtered to screens that exist.
pub fn preset_screen_ids(config: &Config, preset_id: &str) -> Vec<String> {
    config
        .preset(preset_id)
        .map(|preset| {
            preset
                .screens
                .iter()
                .filter(|id| config.screen(id).is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// First free port slot for a new screen.
fn free_slot(config: &Config, preferred: Option<u8>) -> u8 {
    let used: std::collections::HashSet<u8> = config.screens.iter().map(|s| s.slot).collect();
    if let Some(slot) = preferred
        && !used.contains(&slot)
    {
        return slot;
    }
    (0u8..=longcar_core::config::MAX_SLOT)
        .find(|slot| !used.contains(slot))
        .unwrap_or(1)
}

/// First free id derived from `base` (`base`, `base-2`, `base-3`, ...).
pub fn unique_id(config: &Config, base: &str) -> String {
    let slug = slugify(base);
    if config.screen(&slug).is_none() {
        return slug;
    }
    for suffix in 2..1000 {
        let candidate = format!("{slug}-{suffix}");
        if candidate.len() <= 32 && config.screen(&candidate).is_none() {
            return candidate;
        }
    }
    format!("{}-x", &slug[..slug.len().min(30)])
}

/// Default phone-portrait screen for `AddQuickScreen("phone_portrait")`.
pub fn quick_phone_screen(config: &Config) -> (ScreenConfig, Preset) {
    let has_tablet = config.screen("tablet").is_some();
    let id = if config.screen("phone").is_none() {
        "phone".to_string()
    } else {
        unique_id(config, "phone")
    };
    let screen = ScreenConfig {
        id: id.clone(),
        name: if id == "phone" {
            "Phone".to_string()
        } else {
            format!("Phone {}", config.screens.len() + 1)
        },
        width: 1080,
        height: 2400,
        refresh_hz: 90.0,
        scale: 1.0,
        position: Position::Right,
        relative_to: if has_tablet {
            "tablet".to_string()
        } else {
            "primary".to_string()
        },
        alignment: Alignment::Center,
        slot: free_slot(config, None),
        encoder: Encoder::Auto,
        hevc: HevcMode::Auto,
        av1: Av1Mode::Off,
        max_bitrate_kbps: 20000,
        max_resolution: "1080x2400".to_string(),
        cursor: CursorMode::Embedded,
    };
    let mut screens = Vec::new();
    if has_tablet {
        screens.push("tablet".to_string());
    }
    if !screens.contains(&id) {
        screens.push(id.clone());
    }
    let preset = Preset {
        id: if has_tablet {
            "tablet-phone".to_string()
        } else {
            "phone".to_string()
        },
        name: if has_tablet {
            "Tablet + Phone".to_string()
        } else {
            "Phone".to_string()
        },
        screens,
    };
    (screen, preset)
}

/// Default tablet screen for `AddQuickScreen("tablet_landscape")`.
pub fn quick_tablet_screen(config: &Config) -> (ScreenConfig, Preset) {
    let has_tablet = config.screen("tablet").is_some();
    let id = if has_tablet {
        "tablet2".to_string()
    } else {
        "tablet".to_string()
    };
    let screen = ScreenConfig {
        id: id.clone(),
        name: if id == "tablet" {
            "Tablet".to_string()
        } else {
            format!("Tablet {}", config.screens.len() + 1)
        },
        width: 2560,
        height: 1600,
        refresh_hz: 120.0,
        scale: 1.5,
        position: Position::Right,
        relative_to: "primary".to_string(),
        alignment: Alignment::Center,
        slot: free_slot(config, Some(0)),
        encoder: Encoder::Auto,
        hevc: HevcMode::Auto,
        av1: Av1Mode::Off,
        max_bitrate_kbps: 30000,
        max_resolution: "2560x1600".to_string(),
        cursor: CursorMode::Embedded,
    };
    let mut screens = config
        .preset("tablet")
        .map(|p| p.screens.clone())
        .unwrap_or_default();
    if !screens.contains(&id) {
        screens.push(id.clone());
    }
    let preset = Preset {
        id: "tablet".to_string(),
        name: "Tablet".to_string(),
        screens,
    };
    (screen, preset)
}

/// Append a finished session to `sessions.jsonl`, rotating at 1 MiB.
pub fn append_session(record: &SessionRecord) -> std::io::Result<()> {
    let path = longcar_core::paths::sessions_path();
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    if let Ok(meta) = std::fs::metadata(&path)
        && meta.len() > 1024 * 1024
    {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.1"));
    }
    let line = serde_json::to_string(record).map_err(|e| std::io::Error::other(e.to_string()))?;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Pairing entries rebuilt from the API for one screen.
pub fn pending_for_screen(
    screen: &str,
    requests: Vec<longcar_core::sunshine_api::PinRequest>,
) -> Vec<PendingPairing> {
    requests
        .into_iter()
        .map(|request| PendingPairing {
            screen_id: screen.to_string(),
            id: request.id,
            name: request.name,
            address: request.address,
        })
        .collect()
}

/// Client entries rebuilt from the API for one screen.
pub fn devices_for_screen(
    screen: &str,
    clients: Vec<longcar_core::sunshine_api::ClientEntry>,
) -> Vec<DeviceEntry> {
    clients
        .into_iter()
        .map(|client| DeviceEntry {
            screen_id: screen.to_string(),
            uuid: client.uuid,
            name: client.name,
            enabled: client.enabled,
        })
        .collect()
}

fn bool_field(key: &str, value: &serde_json::Value) -> Result<bool, DaemonError> {
    value
        .as_bool()
        .ok_or_else(|| DaemonError::invalid(format!("{key} must be a boolean")))
}

fn string_field(key: &str, value: &serde_json::Value) -> Result<String, DaemonError> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| DaemonError::invalid(format!("{key} must be a string")))
}

/// Apply `UpdateSettings` JSON, leaving absent keys unchanged.
pub fn apply_settings_json(
    general: &mut longcar_core::config::GeneralConfig,
    json: &str,
) -> Result<(), DaemonError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| DaemonError::invalid(format!("settings are not valid JSON: {e}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| DaemonError::invalid("settings must be a JSON object"))?;
    for (key, value) in object {
        match key.as_str() {
            "autostartScreens" => general.autostart_screens = bool_field(key, value)?,
            "notifications" => general.notifications = bool_field(key, value)?,
            "stopWhenIdleMinutes" => {
                general.stop_when_idle_minutes = value
                    .as_u64()
                    .ok_or_else(|| DaemonError::invalid(format!("{key} must be an integer")))?
                    as u32;
            }
            "defaultPreset" => general.default_preset = string_field(key, value)?,
            "audioScreen" => general.audio_screen = string_field(key, value)?,
            "restoreLayoutOnStop" => general.restore_layout_on_stop = bool_field(key, value)?,
            "onboardingComplete" => general.onboarding_complete = bool_field(key, value)?,
            other => {
                return Err(DaemonError::invalid(format!(
                    "unknown settings key '{other}'"
                )));
            }
        }
    }
    Ok(())
}

/// Which parts of a screen config changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenDiff {
    pub geometry: bool,
    pub stream: bool,
    pub slot: bool,
    pub layout: bool,
    pub name: bool,
}

impl ScreenDiff {
    pub fn is_empty(&self) -> bool {
        !(self.geometry || self.stream || self.slot || self.layout || self.name)
    }

    /// True when the change needs the stream restarted (token preserved).
    pub fn needs_restart(&self) -> bool {
        self.geometry || self.stream || self.name
    }
}

pub fn diff_screens(old: &ScreenConfig, new: &ScreenConfig) -> ScreenDiff {
    ScreenDiff {
        geometry: old.width != new.width
            || old.height != new.height
            || (old.refresh_hz - new.refresh_hz).abs() > f64::EPSILON
            || (old.scale - new.scale).abs() > f64::EPSILON
            || old.cursor != new.cursor,
        stream: old.encoder != new.encoder
            || old.hevc != new.hevc
            || old.av1 != new.av1
            || old.max_bitrate_kbps != new.max_bitrate_kbps
            || old.max_resolution != new.max_resolution,
        slot: old.slot != new.slot,
        layout: old.position != new.position
            || old.relative_to != new.relative_to
            || old.alignment != new.alignment,
        name: old.name != new.name,
    }
}

/// Shared in-memory log buffers, readable without the actor.
#[derive(Clone)]
pub struct LogHub {
    daemon: Arc<Mutex<LogBuffer>>,
    screens: Arc<Mutex<HashMap<String, Arc<LogBuffer>>>>,
}

impl Default for LogHub {
    fn default() -> Self {
        Self {
            daemon: Arc::new(Mutex::new(LogBuffer::new(crate::logbuf::DEFAULT_CAPACITY))),
            screens: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl LogHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn daemon_line(&self, line: impl Into<String>) {
        if let Ok(buffer) = self.daemon.lock() {
            buffer.push(line);
        }
    }

    pub fn screen_buffer(&self, screen: &str) -> Arc<LogBuffer> {
        let mut map = match self.screens.lock() {
            Ok(map) => map,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.entry(screen.to_string())
            .or_insert_with(|| Arc::new(LogBuffer::new(crate::logbuf::DEFAULT_CAPACITY)))
            .clone()
    }

    pub fn screen_line(&self, screen: &str, line: impl Into<String>) {
        self.screen_buffer(screen).push(line);
    }

    pub fn daemon_tail(&self, limit: usize) -> Vec<String> {
        self.daemon
            .lock()
            .map(|b| b.tail(limit))
            .unwrap_or_default()
    }

    pub fn screen_tail(&self, screen: &str, limit: usize) -> Vec<String> {
        let map = match self.screens.lock() {
            Ok(map) => map,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.get(screen)
            .map(|buffer| buffer.tail(limit))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use longcar_core::config::Config;

    #[test]
    fn daemon_state_derivation_follows_the_contract() {
        assert_eq!(
            derive_daemon_state(&[ScreenState::Ready], true, true, true),
            DaemonState::Stopping
        );
        assert_eq!(
            derive_daemon_state(&[ScreenState::Ready], false, false, true),
            DaemonState::Starting
        );
        assert_eq!(
            derive_daemon_state(&[ScreenState::Error], false, false, false),
            DaemonState::Degraded
        );
        assert_eq!(
            derive_daemon_state(&[ScreenState::Ready], true, false, false),
            DaemonState::Degraded
        );
        assert_eq!(
            derive_daemon_state(&[ScreenState::Streaming], false, false, false),
            DaemonState::Active
        );
        assert_eq!(
            derive_daemon_state(&[ScreenState::Off], false, false, false),
            DaemonState::Idle
        );
    }

    #[test]
    fn quick_phone_defaults_match_the_spec() {
        let config = Config::default();
        let (screen, preset) = quick_phone_screen(&config);
        assert_eq!(screen.id, "phone");
        assert_eq!((screen.width, screen.height), (1080, 2400));
        assert_eq!(screen.refresh_hz, 90.0);
        assert_eq!(screen.scale, 1.0);
        assert_eq!(screen.relative_to, "tablet");
        assert_eq!(screen.position, Position::Right);
        assert_eq!(screen.alignment, Alignment::Center);
        assert_eq!(screen.slot, 1);
        assert_eq!(screen.max_bitrate_kbps, 20000);
        assert_eq!(screen.max_resolution, "1080x2400");
        assert_eq!(screen.cursor, CursorMode::Embedded);
        assert_eq!(preset.id, "tablet-phone");
        assert_eq!(preset.screens, vec!["tablet", "phone"]);
    }

    #[test]
    fn quick_phone_without_tablet_targets_primary() {
        let mut config = Config::default();
        config.screens.clear();
        config.presets.clear();
        config.general.default_preset.clear();
        config.general.audio_screen.clear();
        let (screen, preset) = quick_phone_screen(&config);
        assert_eq!(screen.relative_to, "primary");
        assert_eq!(preset.id, "phone");
        assert_eq!(preset.screens, vec!["phone"]);
    }

    #[test]
    fn quick_tablet_defaults_and_id_reuse() {
        let config = Config::default();
        let (screen, preset) = quick_tablet_screen(&config);
        assert_eq!(screen.id, "tablet2");
        assert_eq!((screen.width, screen.height), (2560, 1600));
        assert_eq!(screen.slot, 1);
        assert_eq!(preset.id, "tablet");
        assert_eq!(preset.screens, vec!["tablet", "tablet2"]);

        let mut empty = Config::default();
        empty.screens.clear();
        empty.presets.clear();
        empty.general.default_preset.clear();
        let (screen, preset) = quick_tablet_screen(&empty);
        assert_eq!(screen.id, "tablet");
        assert_eq!(screen.slot, 0);
        assert_eq!(preset.screens, vec!["tablet"]);
    }

    #[test]
    fn unique_id_appends_suffixes() {
        let config = Config::default();
        assert_eq!(unique_id(&config, "Tablet"), "tablet-2");
        assert_eq!(unique_id(&config, "phone"), "phone");
    }

    #[test]
    fn settings_json_is_partial_and_strict() {
        let mut general = longcar_core::config::GeneralConfig::default();
        apply_settings_json(&mut general, r#"{"notifications":false}"#).unwrap();
        assert!(!general.notifications);
        assert_eq!(general.stop_when_idle_minutes, 0);
        apply_settings_json(
            &mut general,
            r#"{"stopWhenIdleMinutes":15,"defaultPreset":"x"}"#,
        )
        .unwrap();
        assert_eq!(general.stop_when_idle_minutes, 15);
        assert_eq!(general.default_preset, "x");
        assert!(apply_settings_json(&mut general, r#"{"bogus":1}"#).is_err());
        assert!(apply_settings_json(&mut general, r#"{"notifications":"yes"}"#).is_err());
        assert!(apply_settings_json(&mut general, "not json").is_err());
    }

    #[test]
    fn screen_diff_classifies_changes() {
        let old = Config::default().screens[0].clone();
        let mut new = old.clone();
        assert!(diff_screens(&old, &new).is_empty());
        new.max_bitrate_kbps = 20000;
        assert!(diff_screens(&old, &new).stream);
        let mut new = old.clone();
        new.width = 1920;
        new.height = 1200;
        assert!(diff_screens(&old, &new).geometry);
        let mut new = old.clone();
        new.slot = 3;
        assert!(diff_screens(&old, &new).slot);
        assert!(!diff_screens(&old, &new).needs_restart());
        let mut new = old.clone();
        new.position = Position::Left;
        let diff = diff_screens(&old, &new);
        assert!(diff.layout);
        assert!(!diff.needs_restart());
        let mut new = old.clone();
        new.name = "Other".to_string();
        assert!(diff_screens(&old, &new).needs_restart());
    }

    #[test]
    fn preset_screens_are_filtered_to_existing() {
        let mut config = Config::default();
        config.presets[0].screens.push("ghost".to_string());
        assert_eq!(preset_screen_ids(&config, "tablet"), vec!["tablet"]);
        assert!(preset_screen_ids(&config, "missing").is_empty());
    }

    #[test]
    fn screen_entry_ports_come_from_the_slot() {
        let config = Config::default();
        let entry = screen_entry(
            &config.screens[0],
            ScreenState::Off,
            None,
            None,
            None,
            false,
            0,
            0,
            None,
        );
        assert_eq!(entry.base_port, 48049);
        assert_eq!(entry.web_port, 48050);
        let mut screen = config.screens[0].clone();
        screen.slot = 1;
        let entry = screen_entry(
            &screen,
            ScreenState::Off,
            None,
            None,
            None,
            false,
            0,
            0,
            None,
        );
        assert_eq!(entry.base_port, 48109);
        assert_eq!(entry.web_port, 48110);
    }

    #[test]
    fn log_hub_keeps_daemon_and_screen_buffers_separate() {
        let hub = LogHub::new();
        hub.daemon_line("daemon line");
        hub.screen_line("tablet", "screen line");
        assert_eq!(hub.daemon_tail(10), vec!["daemon line"]);
        assert_eq!(hub.screen_tail("tablet", 10), vec!["screen line"]);
        assert!(hub.screen_tail("phone", 10).is_empty());
    }

    #[test]
    fn physical_entries_skip_virtual_connectors() {
        use longcar_core::model::{
            DisplayState, LogicalMonitorState, MonitorInfo, MonitorMode, MonitorRef,
        };
        let state = DisplayState {
            serial: 1,
            monitors: vec![
                MonitorInfo {
                    connector: "eDP-1".to_string(),
                    vendor: "Vendor".to_string(),
                    product: "Panel".to_string(),
                    serial: "0".to_string(),
                    modes: vec![MonitorMode {
                        id: "1920x1200@60".to_string(),
                        width: 1920,
                        height: 1200,
                        refresh_hz: 60.0,
                        preferred_scale: 1.0,
                        supported_scales: vec![1.0, 1.25],
                    }],
                    properties: Default::default(),
                },
                MonitorInfo {
                    connector: "Meta-0".to_string(),
                    vendor: "Meta".to_string(),
                    product: "Virtual".to_string(),
                    serial: "0".to_string(),
                    modes: vec![MonitorMode {
                        id: "2560x1600@120".to_string(),
                        width: 2560,
                        height: 1600,
                        refresh_hz: 120.0,
                        preferred_scale: 1.0,
                        supported_scales: vec![1.0, 1.5],
                    }],
                    properties: Default::default(),
                },
            ],
            logical_monitors: vec![
                LogicalMonitorState {
                    x: 0,
                    y: 0,
                    scale: 1.25,
                    transform: 0,
                    primary: true,
                    monitors: vec![MonitorRef {
                        connector: "eDP-1".to_string(),
                        mode_id: "1920x1200@60".to_string(),
                    }],
                },
                LogicalMonitorState {
                    x: 1536,
                    y: 0,
                    scale: 1.5,
                    transform: 0,
                    primary: false,
                    monitors: vec![MonitorRef {
                        connector: "Meta-0".to_string(),
                        mode_id: "2560x1600@120".to_string(),
                    }],
                },
            ],
            properties: Default::default(),
        };
        let entries = physical_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].connector, "eDP-1");
        assert_eq!(entries[0].width, 1536);
        assert_eq!(entries[0].height, 960);
        assert!(entries[0].primary);
    }
}
