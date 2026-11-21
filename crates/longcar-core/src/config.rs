// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent configuration (`~/.config/longcar/config.toml`).

use std::collections::HashSet;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Alignment, Av1Mode, CursorMode, Encoder, HevcMode, Position};

/// Current config file schema version.
pub const CONFIG_VERSION: u32 = 1;
/// Highest usable port slot (2 * 4 slots + 1 = index 0..=7).
pub const MAX_SLOT: u8 = 7;

const ID_MAX: usize = 32;
const NAME_MAX: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read or write configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot parse configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("cannot serialize configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("unsupported configuration version {found}, expected {expected}")]
    Version { found: u32, expected: u32 },
    #[error("invalid id '{id}': {reason}")]
    InvalidId { id: String, reason: String },
    #[error("duplicate id '{0}'")]
    DuplicateId(String),
    #[error("invalid screen name '{0}'")]
    InvalidName(String),
    #[error("screen '{screen}': {field} {value} is out of range")]
    OutOfRange {
        screen: String,
        field: &'static str,
        value: String,
    },
    #[error("screens '{a}' and '{b}' share the same resolution {width}x{height}")]
    DuplicateResolution {
        width: u32,
        height: u32,
        a: String,
        b: String,
    },
    #[error("screens '{a}' and '{b}' share slot {slot}")]
    DuplicateSlot { slot: u8, a: String, b: String },
    #[error("screen '{screen}' is placed relative to unknown monitor '{target}'")]
    InvalidRelativeTo { screen: String, target: String },
    #[error("relative placement cycle involving screen '{0}'")]
    RelativeCycle(String),
    #[error("screen '{0}' does not exist")]
    ScreenNotFound(String),
    #[error("preset '{0}' does not exist")]
    PresetNotFound(String),
    #[error("preset '{preset}' is invalid: {reason}")]
    InvalidPreset { preset: String, reason: String },
    #[error("default preset '{0}' does not exist")]
    InvalidDefaultPreset(String),
    #[error("default preset must be set when screens are configured")]
    MissingDefaultPreset,
    #[error("audio screen '{0}' does not exist")]
    InvalidAudioScreen(String),
    #[error("invalid resolution '{0}', expected WxH with even values between 640 and 7680")]
    InvalidResolution(String),
    #[error("port {port}/{proto} is already in use; run: {hint}")]
    PortInUse {
        port: u16,
        proto: &'static str,
        hint: String,
    },
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub screens: Vec<ScreenConfig>,
    #[serde(default)]
    pub presets: Vec<Preset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub autostart_screens: bool,
    pub notifications: bool,
    pub stop_when_idle_minutes: u32,
    pub default_preset: String,
    /// Deprecated: per-screen `audio` flags replaced it. Still written/read for
    /// migration, omitted from new files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub audio_screen: String,
    pub restore_layout_on_stop: bool,
    pub onboarding_complete: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            autostart_screens: false,
            notifications: true,
            stop_when_idle_minutes: 0,
            default_preset: String::new(),
            audio_screen: String::new(),
            restore_layout_on_stop: true,
            onboarding_complete: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenConfig {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    #[serde(alias = "refreshHz")]
    pub refresh_hz: f64,
    pub scale: f64,
    pub position: Position,
    #[serde(alias = "relativeTo")]
    pub relative_to: String,
    pub alignment: Alignment,
    pub slot: u8,
    pub encoder: Encoder,
    pub hevc: HevcMode,
    pub av1: Av1Mode,
    #[serde(alias = "maxBitrateKbps")]
    pub max_bitrate_kbps: u32,
    #[serde(alias = "maxResolution")]
    pub max_resolution: String,
    pub cursor: CursorMode,
    /// Stream this screen's audio to the client (`stream_audio` in Sunshine).
    #[serde(default = "default_true")]
    pub audio: bool,
    /// Send native pen/touch events instead of translating them to pointer input
    /// (`native_pen_touch` in Sunshine).
    #[serde(default = "default_true")]
    pub touch: bool,
}

fn default_true() -> bool {
    true
}

impl ScreenConfig {
    /// Effective stream resolution cap, falling back to the monitor size.
    pub fn effective_max_resolution(&self) -> (u32, u32) {
        match parse_resolution(&self.max_resolution) {
            Some((w, h)) => (w, h),
            None => (self.width, self.height),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub screens: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        let screen = ScreenConfig {
            id: "tablet".to_string(),
            name: "Tablet".to_string(),
            width: 2560,
            height: 1600,
            refresh_hz: 120.0,
            scale: 1.5,
            position: Position::Right,
            relative_to: "primary".to_string(),
            alignment: Alignment::Center,
            slot: 0,
            encoder: Encoder::Auto,
            hevc: HevcMode::Auto,
            av1: Av1Mode::Off,
            max_bitrate_kbps: 30000,
            max_resolution: "2560x1600".to_string(),
            cursor: CursorMode::Embedded,
            audio: true,
            touch: true,
        };
        Self {
            version: CONFIG_VERSION,
            general: GeneralConfig {
                default_preset: "tablet".to_string(),
                ..GeneralConfig::default()
            },
            presets: vec![Preset {
                id: "tablet".to_string(),
                name: "Tablet".to_string(),
                screens: vec!["tablet".to_string()],
            }],
            screens: vec![screen],
        }
    }
}

/// True when `id` is a valid screen or preset id.
pub fn is_valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() < 2 || bytes.len() > ID_MAX {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// Parse `WxH` with even values in the supported range.
pub fn parse_resolution(value: &str) -> Option<(u32, u32)> {
    let (w, h) = value.split_once(['x', 'X'])?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    if is_valid_dimension(w) && is_valid_dimension(h) {
        Some((w, h))
    } else {
        None
    }
}

fn is_valid_dimension(v: u32) -> bool {
    (640..=7680).contains(&v) && v.is_multiple_of(2)
}

/// Slugify a screen name into a candidate id.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > ID_MAX {
        out.truncate(ID_MAX);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        out.insert(0, 's');
    }
    if out.len() < 2 {
        out.push('0');
    }
    out
}

impl Config {
    /// Migrate older configs: `general.audio_screen` used to pick the single screen that
    /// carried audio. Fold it into the per-screen flags and clear it.
    pub fn migrate(&mut self) {
        let audio_screen = self.general.audio_screen.trim().to_string();
        if audio_screen.is_empty() {
            return;
        }
        if self.screens.iter().any(|screen| screen.id == audio_screen) {
            for screen in &mut self.screens {
                screen.audio = screen.id == audio_screen;
            }
        }
        self.general.audio_screen.clear();
    }

    /// First screen that streams audio, for the legacy `settings.audioScreen` view.
    pub fn audio_screen(&self) -> String {
        self.screens
            .iter()
            .find(|screen| screen.audio)
            .map(|screen| screen.id.clone())
            .unwrap_or_default()
    }

    pub fn screen(&self, id: &str) -> Option<&ScreenConfig> {
        self.screens.iter().find(|s| s.id == id)
    }

    pub fn screen_mut(&mut self, id: &str) -> Option<&mut ScreenConfig> {
        self.screens.iter_mut().find(|s| s.id == id)
    }

    pub fn preset(&self, id: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.id == id)
    }

    /// Slots currently taken by other configured screens.
    pub fn used_slots(&self) -> Vec<u8> {
        let mut slots: Vec<u8> = self.screens.iter().map(|s| s.slot).collect();
        slots.sort_unstable();
        slots.dedup();
        slots
    }

    /// Validate every cross-reference. Returns the first problem found.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version != CONFIG_VERSION {
            return Err(ConfigError::Version {
                found: self.version,
                expected: CONFIG_VERSION,
            });
        }

        let mut ids: HashSet<&str> = HashSet::new();
        let mut slots: Vec<(u8, &str)> = Vec::new();
        let mut resolutions: Vec<(u32, u32, &str)> = Vec::new();

        for screen in &self.screens {
            if !is_valid_id(&screen.id) {
                return Err(ConfigError::InvalidId {
                    id: screen.id.clone(),
                    reason: "must match ^[a-z][a-z0-9-]{1,31}$".to_string(),
                });
            }
            if !ids.insert(screen.id.as_str()) {
                return Err(ConfigError::DuplicateId(screen.id.clone()));
            }
            let trimmed = screen.name.trim();
            if trimmed.is_empty() || trimmed.len() > NAME_MAX || trimmed.contains(['\n', '\r', '#'])
            {
                return Err(ConfigError::InvalidName(screen.name.clone()));
            }
            for (field, value) in [("width", screen.width), ("height", screen.height)] {
                if !is_valid_dimension(value) {
                    return Err(ConfigError::OutOfRange {
                        screen: screen.id.clone(),
                        field,
                        value: value.to_string(),
                    });
                }
            }
            if !(24.0..=240.0).contains(&screen.refresh_hz) {
                return Err(ConfigError::OutOfRange {
                    screen: screen.id.clone(),
                    field: "refresh_hz",
                    value: format!("{}", screen.refresh_hz),
                });
            }
            if !(0.5..=3.0).contains(&screen.scale) {
                return Err(ConfigError::OutOfRange {
                    screen: screen.id.clone(),
                    field: "scale",
                    value: format!("{}", screen.scale),
                });
            }
            if screen.slot > MAX_SLOT {
                return Err(ConfigError::OutOfRange {
                    screen: screen.id.clone(),
                    field: "slot",
                    value: screen.slot.to_string(),
                });
            }
            if let Some((_, other)) = slots.iter().find(|(slot, _)| *slot == screen.slot) {
                return Err(ConfigError::DuplicateSlot {
                    slot: screen.slot,
                    a: other.to_string(),
                    b: screen.id.clone(),
                });
            }
            slots.push((screen.slot, screen.id.as_str()));

            if let Some((w, h, other)) = resolutions
                .iter()
                .find(|(w, h, _)| *w == screen.width && *h == screen.height)
            {
                return Err(ConfigError::DuplicateResolution {
                    width: *w,
                    height: *h,
                    a: other.to_string(),
                    b: screen.id.clone(),
                });
            }
            resolutions.push((screen.width, screen.height, screen.id.as_str()));

            if !screen.max_resolution.trim().is_empty()
                && parse_resolution(&screen.max_resolution).is_none()
            {
                return Err(ConfigError::InvalidResolution(
                    screen.max_resolution.clone(),
                ));
            }
            if screen.max_bitrate_kbps != 0 && !(1000..=200_000).contains(&screen.max_bitrate_kbps)
            {
                return Err(ConfigError::OutOfRange {
                    screen: screen.id.clone(),
                    field: "max_bitrate_kbps",
                    value: screen.max_bitrate_kbps.to_string(),
                });
            }
        }

        // Relative placement must reference "primary" or an existing screen.
        for screen in &self.screens {
            if screen.relative_to == "primary" || screen.relative_to.is_empty() {
                continue;
            }
            if self.screen(&screen.relative_to).is_none() {
                return Err(ConfigError::InvalidRelativeTo {
                    screen: screen.id.clone(),
                    target: screen.relative_to.clone(),
                });
            }
        }
        self.check_relative_cycles()?;

        if !self.general.audio_screen.trim().is_empty()
            && self.screen(&self.general.audio_screen).is_none()
        {
            return Err(ConfigError::InvalidAudioScreen(
                self.general.audio_screen.clone(),
            ));
        }

        let mut preset_ids: HashSet<&str> = HashSet::new();
        let mut preset_names: HashSet<&str> = HashSet::new();
        for preset in &self.presets {
            if !is_valid_id(&preset.id) {
                return Err(ConfigError::InvalidId {
                    id: preset.id.clone(),
                    reason: "must match ^[a-z][a-z0-9-]{1,31}$".to_string(),
                });
            }
            if !preset_ids.insert(preset.id.as_str()) {
                return Err(ConfigError::DuplicateId(preset.id.clone()));
            }
            let trimmed_name = preset.name.trim();
            if trimmed_name.is_empty()
                || trimmed_name.len() > NAME_MAX
                || trimmed_name.contains(['\n', '\r', '#'])
            {
                return Err(ConfigError::InvalidName(preset.name.clone()));
            }
            if !preset_names.insert(trimmed_name) {
                return Err(ConfigError::InvalidPreset {
                    preset: preset.id.clone(),
                    reason: format!("duplicate preset name '{trimmed_name}'"),
                });
            }
            if preset.screens.is_empty() {
                return Err(ConfigError::InvalidPreset {
                    preset: preset.id.clone(),
                    reason: "preset has no screens".to_string(),
                });
            }
            for id in &preset.screens {
                if self.screen(id).is_none() {
                    return Err(ConfigError::InvalidPreset {
                        preset: preset.id.clone(),
                        reason: format!("unknown screen '{id}'"),
                    });
                }
            }
        }

        let default_preset = self.general.default_preset.trim();
        if !default_preset.is_empty() && self.preset(default_preset).is_none() {
            return Err(ConfigError::InvalidDefaultPreset(
                default_preset.to_string(),
            ));
        }
        if !self.screens.is_empty() && default_preset.is_empty() {
            return Err(ConfigError::MissingDefaultPreset);
        }

        Ok(())
    }

    fn check_relative_cycles(&self) -> Result<(), ConfigError> {
        for screen in &self.screens {
            let mut seen: Vec<&str> = Vec::new();
            let mut current = screen.relative_to.as_str();
            while !current.is_empty() && current != "primary" {
                if current == screen.id {
                    return Err(ConfigError::RelativeCycle(screen.id.clone()));
                }
                if seen.contains(&current) {
                    return Err(ConfigError::RelativeCycle(screen.id.clone()));
                }
                seen.push(current);
                match self.screen(current) {
                    Some(next) => current = next.relative_to.as_str(),
                    None => break,
                }
            }
        }
        Ok(())
    }
}

impl Config {
    /// Load the config, creating or repairing it when needed.
    ///
    /// Returns the config and whether a new file was written.
    pub fn load_or_create(path: &Path) -> Result<(Self, bool), ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(mut config) => {
                    config.migrate();
                    let validation = config.validate();
                    match validation {
                        Ok(()) => Ok((config, false)),
                        Err(_validation) => {
                            quarantine(path, "invalid")?;
                            let default = Config::default();
                            default.save(path)?;
                            Ok((default, true))
                        }
                    }
                }
                Err(_parse) => {
                    quarantine(path, "invalid")?;
                    let default = Config::default();
                    default.save(path)?;
                    Ok((default, true))
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let default = Config::default();
                default.save(path)?;
                Ok((default, true))
            }
            Err(err) => Err(ConfigError::Io(err)),
        }
    }

    /// Serialize to the canonical TOML form.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Atomically write the config with mode 0600.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let text = self.to_toml()?;
        let tmp = path.with_extension("toml.tmp");
        {
            let mut file = std::fs::File::create(&tmp)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Create `dir` (and parents) with mode 0700.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn quarantine(path: &Path, suffix: &str) -> Result<PathBuf, ConfigError> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!(
        "{}.{}-{}",
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "config.toml".to_string()),
        suffix,
        stamp
    );
    let target = path.with_file_name(name);
    std::fs::rename(path, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_dir<T>(f: impl FnOnce(&Path) -> T) -> T {
        let dir = std::env::temp_dir().join(format!(
            "longcar-config-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = f(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn default_config_is_valid_and_round_trips() {
        let config = Config::default();
        config.validate().unwrap();
        let text = config.to_toml().unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn default_config_matches_documented_schema() {
        let text = Config::default().to_toml().unwrap();
        for needle in [
            "version = 1",
            "autostart_screens = false",
            "stop_when_idle_minutes = 0",
            "default_preset = \"tablet\"",
            "audio = true",
            "touch = true",
            "restore_layout_on_stop = true",
            "onboarding_complete = false",
            "[[screens]]",
            "id = \"tablet\"",
            "refresh_hz = 120.0",
            "scale = 1.5",
            "position = \"right\"",
            "relative_to = \"primary\"",
            "alignment = \"center\"",
            "slot = 0",
            "encoder = \"auto\"",
            "hevc = \"auto\"",
            "av1 = \"off\"",
            "max_bitrate_kbps = 30000",
            "max_resolution = \"2560x1600\"",
            "cursor = \"embedded\"",
            "[[presets]]",
        ] {
            assert!(text.contains(needle), "missing {needle} in\n{text}");
        }
    }

    #[test]
    fn duplicate_resolution_is_rejected() {
        let mut config = Config::default();
        let mut second = config.screens[0].clone();
        second.id = "tablet2".to_string();
        second.name = "Tablet 2".to_string();
        second.slot = 1;
        config.screens.push(second);
        config.presets[0].screens.push("tablet2".to_string());
        match config.validate() {
            Err(ConfigError::DuplicateResolution { a, b, .. }) => {
                assert_eq!(a, "tablet");
                assert_eq!(b, "tablet2");
            }
            other => panic!("expected duplicate resolution, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_slot_is_rejected() {
        let mut config = Config::default();
        let mut second = config.screens[0].clone();
        second.id = "phone".to_string();
        second.name = "Phone".to_string();
        second.width = 1080;
        second.height = 2400;
        config.screens.push(second);
        config.presets[0].screens.push("phone".to_string());
        assert!(matches!(
            config.validate(),
            Err(ConfigError::DuplicateSlot { slot: 0, .. })
        ));
    }

    #[test]
    fn unknown_preset_screen_is_rejected() {
        let mut config = Config::default();
        config.presets[0].screens.push("ghost".to_string());
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidPreset { .. })
        ));
    }

    #[test]
    fn empty_preset_is_rejected() {
        let mut config = Config::default();
        config.presets[0].screens.clear();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidPreset { .. })
        ));
    }

    #[test]
    fn no_screens_allows_empty_presets() {
        let mut config = Config::default();
        config.screens.clear();
        config.presets.clear();
        config.general.default_preset.clear();
        config.general.audio_screen.clear();
        config.validate().unwrap();
    }

    #[test]
    fn missing_default_preset_with_screens_is_rejected() {
        let mut config = Config::default();
        config.general.default_preset.clear();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::MissingDefaultPreset)
        ));
    }

    #[test]
    fn relative_cycle_is_rejected() {
        let mut config = Config::default();
        let mut second = config.screens[0].clone();
        second.id = "phone".to_string();
        second.name = "Phone".to_string();
        second.width = 1080;
        second.height = 2400;
        second.slot = 1;
        second.relative_to = "tablet".to_string();
        config.screens[0].relative_to = "phone".to_string();
        config.screens.push(second);
        config.presets[0].screens.push("phone".to_string());
        assert!(matches!(
            config.validate(),
            Err(ConfigError::RelativeCycle(_))
        ));
    }

    #[test]
    fn odd_and_out_of_range_geometry_is_rejected() {
        let mut config = Config::default();
        config.screens[0].width = 2561;
        assert!(config.validate().is_err());
        let mut config = Config::default();
        config.screens[0].scale = 0.25;
        assert!(config.validate().is_err());
        let mut config = Config::default();
        config.screens[0].refresh_hz = 300.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn invalid_ids_and_names_are_rejected() {
        for id in ["Tablet", "1tablet", "a", "tab let", ""] {
            let mut config = Config::default();
            config.screens[0].id = id.to_string();
            config.presets[0].screens = vec![id.to_string()];
            assert!(
                matches!(config.validate(), Err(ConfigError::InvalidId { .. })),
                "id {id:?} should be invalid"
            );
        }
        let mut config = Config::default();
        config.screens[0].name = "  ".to_string();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidName(_))
        ));
    }

    #[test]
    fn open_creates_default_file_with_private_mode() {
        with_temp_dir(|dir| {
            let path = dir.join("config.toml");
            let (config, created) = Config::load_or_create(&path).unwrap();
            assert!(created);
            assert_eq!(config, Config::default());
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let (again, created_again) = Config::load_or_create(&path).unwrap();
            assert!(!created_again);
            assert_eq!(again, config);
        });
    }

    #[test]
    fn invalid_file_is_quarantined_not_overwritten() {
        with_temp_dir(|dir| {
            let path = dir.join("config.toml");
            std::fs::write(&path, "version = 1\nnot valid toml ===\n").unwrap();
            let (config, created) = Config::load_or_create(&path).unwrap();
            assert!(created);
            assert_eq!(config, Config::default());
            let backups: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.contains("invalid"))
                .collect();
            assert_eq!(backups.len(), 1, "{backups:?}");
            assert_eq!(
                std::fs::read_to_string(dir.join(&backups[0])).unwrap(),
                "version = 1\nnot valid toml ===\n"
            );
        });
    }

    #[test]
    fn validation_failure_is_quarantined() {
        with_temp_dir(|dir| {
            let path = dir.join("config.toml");
            let mut config = Config::default();
            config.screens[0].width = 2561;
            std::fs::write(&path, config.to_toml().unwrap()).unwrap();
            let (loaded, created) = Config::load_or_create(&path).unwrap();
            assert!(created);
            assert_eq!(loaded, Config::default());
            assert!(
                std::fs::read_dir(dir)
                    .unwrap()
                    .filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().contains("invalid"))
            );
        });
    }

    #[test]
    fn slugify_produces_valid_ids() {
        assert_eq!(slugify("Tablet"), "tablet");
        assert_eq!(slugify("Living Room TV"), "living-room-tv");
        assert_eq!(slugify("  Pixel 9 Pro  "), "pixel-9-pro");
        assert!(is_valid_id(&slugify("Tablet")));
        assert!(is_valid_id(&slugify("123")));
    }

    #[test]
    fn screen_accepts_camel_case_json_and_snake_case_toml() {
        let json = r#"{"id":"phone","name":"Phone","width":1080,"height":2400,
            "refreshHz":90.0,"scale":1.0,"position":"below","relativeTo":"primary",
            "alignment":"start","slot":2,"encoder":"auto","hevc":"auto","av1":"off",
            "maxBitrateKbps":20000,"maxResolution":"1080x2400","cursor":"embedded"}"#;
        let screen: ScreenConfig = serde_json::from_str(json).unwrap();
        assert_eq!(screen.refresh_hz, 90.0);
        assert_eq!(screen.relative_to, "primary");
        assert_eq!(screen.max_bitrate_kbps, 20000);
        assert_eq!(screen.max_resolution, "1080x2400");
        let toml_text = Config::default().to_toml().unwrap();
        assert!(toml_text.contains("refresh_hz = 120.0"));
        assert!(toml_text.contains("relative_to = \"primary\""));
        // Snake-case JSON payloads stay valid too, so either shape round-trips.
        let snake = serde_json::to_string(&screen).unwrap();
        let reparsed: ScreenConfig = serde_json::from_str(&snake).unwrap();
        assert_eq!(reparsed, screen);
    }

    #[test]
    fn migrate_folds_audio_screen_into_screen_flags() {
        let mut config = Config::default();
        config.general.audio_screen = "tablet".to_string();
        let mut phone = config.screens[0].clone();
        phone.id = "phone".to_string();
        phone.name = "Phone".to_string();
        phone.width = 1080;
        phone.height = 2400;
        phone.slot = 1;
        config.screens.push(phone);
        config.presets[0].screens.push("phone".to_string());
        config.screens[0].audio = true;
        config.screens[1].audio = true;

        config.migrate();
        assert!(config.general.audio_screen.is_empty());
        assert!(config.screens[0].audio);
        assert!(!config.screens[1].audio);
        assert_eq!(config.audio_screen(), "tablet");
        config.validate().unwrap();
    }

    #[test]
    fn audio_screen_reports_empty_when_all_disabled() {
        let mut config = Config::default();
        config.screens[0].audio = false;
        assert_eq!(config.audio_screen(), "");
    }

    #[test]
    fn resolution_parsing_rules() {
        assert_eq!(parse_resolution("2560x1600"), Some((2560, 1600)));
        assert_eq!(parse_resolution("2561x1600"), None);
        assert_eq!(parse_resolution("100x100"), None);
        assert_eq!(parse_resolution(""), None);
        assert_eq!(parse_resolution("2560"), None);
    }
}
