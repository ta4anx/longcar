// SPDX-License-Identifier: GPL-3.0-or-later

//! Filesystem locations used by Longcar.

use std::path::PathBuf;

/// `~/.config/longcar`.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("longcar")
}

/// `~/.config/longcar/config.toml`.
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// `~/.config/longcar/instances`.
pub fn instances_dir() -> PathBuf {
    config_dir().join("instances")
}

/// `~/.config/longcar/instances/<screen-id>`.
pub fn instance_dir(screen_id: &str) -> PathBuf {
    instances_dir().join(screen_id)
}

/// `~/.local/state/longcar`.
pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("longcar")
}

/// `~/.local/state/longcar/layout-snapshot.json`.
pub fn layout_snapshot_path() -> PathBuf {
    state_dir().join("layout-snapshot.json")
}

/// `~/.local/state/longcar/sessions.jsonl`.
pub fn sessions_path() -> PathBuf {
    state_dir().join("sessions.jsonl")
}
