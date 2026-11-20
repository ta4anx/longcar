// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure data model, configuration, layout planning and Sunshine protocol
//! helpers shared by every Longcar component.

pub mod config;
pub mod layout;
pub mod logparse;
pub mod model;
pub mod network;
pub mod paths;
pub mod ports;
pub mod state;
pub mod sunshine;
pub mod sunshine_api;

pub use config::{Config, ConfigError, GeneralConfig, Preset, ScreenConfig};
pub use model::{
    Alignment, Av1Mode, CursorMode, DaemonState, DisplayState, Encoder, HevcMode, LayoutSnapshot,
    LogicalMonitorConfig, LogicalMonitorState, MonitorInfo, MonitorMode, MonitorRef, Position,
    Rect, ScreenState, SnapshotMonitor, Stage, StageError,
};
