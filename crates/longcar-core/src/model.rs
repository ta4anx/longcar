// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared data types: enums, geometry, monitor/display state and error records.
//!
//! Everything here is pure data so that `longcar-core` stays free of D-Bus and
//! GTK dependencies.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "io.github.ta4anx.Longcar";
pub const BUS_NAME: &str = "io.github.ta4anx.Longcar.Daemon";
pub const OBJECT_PATH: &str = "/io/github/ta4anx/Longcar/Daemon";
pub const INTERFACE: &str = "io.github.ta4anx.Longcar.Daemon.Control";
pub const EXTENSION_UUID: &str = "longcar@ta4anx.github.io";

/// Warning identifiers published in the daemon state.
pub mod warnings {
    pub const LAYOUT_RESTORE_AVAILABLE: &str = "layout-restore-available";
    pub const PORTAL_TIMEOUT: &str = "portal-timeout";
    pub const SUNSHINE_MISSING: &str = "sunshine-missing";
    pub const DAEMON_DEGRADED: &str = "daemon-degraded";
    pub const RELATIVE_TO_INACTIVE: &str = "relative-to-inactive";
    pub const SESSION_CLOSED: &str = "session-closed";
}

/// Video encoder selection handed to Sunshine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoder {
    #[default]
    Auto,
    Vaapi,
    Nvenc,
    Vulkan,
    Software,
}

impl Encoder {
    /// Value written to Sunshine's `encoder` key.
    pub fn sunshine_value(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Vaapi => Some("vaapi"),
            Self::Nvenc => Some("nvenc"),
            Self::Vulkan => Some("vulkan"),
            Self::Software => Some("software"),
        }
    }
}

/// HEVC advertisement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HevcMode {
    #[default]
    Auto,
    Off,
    Main,
    Main10,
}

impl HevcMode {
    pub fn sunshine_value(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Off => 1,
            Self::Main => 2,
            Self::Main10 => 3,
        }
    }
}

/// AV1 advertisement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Av1Mode {
    #[default]
    Auto,
    Off,
    Main,
    Main10,
}

impl Av1Mode {
    pub fn sunshine_value(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Off => 1,
            Self::Main => 2,
            Self::Main10 => 3,
        }
    }
}

/// Cursor handling for the virtual output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorMode {
    Hidden,
    #[default]
    Embedded,
    Metadata,
}

impl CursorMode {
    /// Value for Mutter `RecordVirtual`'s `cursor-mode` property.
    pub fn mutter_value(self) -> u32 {
        match self {
            Self::Hidden => 0,
            Self::Embedded => 1,
            Self::Metadata => 2,
        }
    }
}

/// Placement of a virtual screen relative to its reference monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Left,
    #[default]
    Right,
    Above,
    Below,
}

/// Alignment along the axis perpendicular to [`Position`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Alignment {
    Start,
    #[default]
    Center,
    End,
}

/// Runtime state of one screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScreenState {
    #[default]
    Off,
    Preparing,
    WaitingPortal,
    Starting,
    Ready,
    Streaming,
    Stopping,
    Error,
}

impl ScreenState {
    /// True when the screen holds resources (display, unit or both).
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::WaitingPortal | Self::Starting | Self::Ready | Self::Streaming
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Preparing => "preparing",
            Self::WaitingPortal => "waitingPortal",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Streaming => "streaming",
            Self::Stopping => "stopping",
            Self::Error => "error",
        }
    }
}

/// Aggregate daemon state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DaemonState {
    #[default]
    Idle,
    Starting,
    Active,
    Degraded,
    Stopping,
}

impl DaemonState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Active => "active",
            Self::Degraded => "degraded",
            Self::Stopping => "stopping",
        }
    }
}

/// Stage a screen failed in. Serialized verbatim (PascalCase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    Validate,
    MutterSession,
    RecordVirtual,
    PipeWireNode,
    Layout,
    SunshineSpawn,
    SunshineApi,
    PortalAuth,
    Pairing,
    Api,
    Io,
}

/// Failure record published in `screens[].error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageError {
    pub stage: Stage,
    pub message: String,
}

impl StageError {
    pub fn new(stage: Stage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }
}

impl std::error::Error for StageError {}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.stage, self.message)
    }
}

/// Axis-aligned rectangle in logical or physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> i32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height
    }

    /// Overlap test that treats shared edges as non-intersecting.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// One connector/mode pair inside a logical monitor config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorRef {
    pub connector: String,
    pub mode_id: String,
}

/// A logical monitor to hand to Mutter's `ApplyMonitorsConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicalMonitorConfig {
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
    pub monitors: Vec<MonitorRef>,
}

/// A supported output mode as reported by DisplayConfig.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorMode {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    pub preferred_scale: f64,
    pub supported_scales: Vec<f64>,
}

/// A physical or virtual monitor from DisplayConfig.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub connector: String,
    pub vendor: String,
    pub product: String,
    pub serial: String,
    pub modes: Vec<MonitorMode>,
    #[serde(default)]
    pub properties: HashMap<String, serde_json::Value>,
}

impl MonitorInfo {
    pub fn display_name(&self) -> String {
        let product = self.product.trim();
        if product.is_empty() {
            self.connector.clone()
        } else {
            product.to_string()
        }
    }
}

/// A logical monitor from DisplayConfig's current state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicalMonitorState {
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
    pub monitors: Vec<MonitorRef>,
}

/// Result of `GetCurrentState`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DisplayState {
    pub serial: u32,
    pub monitors: Vec<MonitorInfo>,
    pub logical_monitors: Vec<LogicalMonitorState>,
    #[serde(default)]
    pub properties: HashMap<String, serde_json::Value>,
}

impl DisplayState {
    pub fn connector(&self, connector: &str) -> Option<&MonitorInfo> {
        self.monitors.iter().find(|m| m.connector == connector)
    }

    pub fn connectors(&self) -> Vec<String> {
        self.monitors.iter().map(|m| m.connector.clone()).collect()
    }

    /// Logical rect currently occupied by `connector`.
    pub fn logical_rect(&self, connector: &str) -> Option<Rect> {
        self.logical_monitors
            .iter()
            .find(|lm| lm.monitors.iter().any(|m| m.connector == connector))
            .map(|lm| Rect::new(lm.x, lm.y, 0, 0))
    }
}

/// One monitor entry of a saved layout snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotMonitor {
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
    pub connector: String,
    pub mode_id: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
}

/// Saved pre-start layout, restorable with `RestoreLayout`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutSnapshot {
    pub version: u32,
    pub taken_at: String,
    pub logical_monitors: Vec<SnapshotMonitor>,
    pub virtual_connectors: Vec<String>,
}

pub const LAYOUT_SNAPSHOT_VERSION: u32 = 1;
