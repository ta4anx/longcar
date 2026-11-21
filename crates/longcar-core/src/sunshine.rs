// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Sunshine discovery, version gate, and generated configuration.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

use crate::config::ScreenConfig;

/// Lowest Sunshine release Longcar can drive.
pub const MIN_VERSION: SunshineVersion = SunshineVersion {
    major: 2026,
    minor: 914,
    patch: 0,
};

/// Parsed Sunshine release, ordered by `(major, minor, patch)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SunshineVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl SunshineVersion {
    /// Extract the release from arbitrary `--version` output.
    pub fn parse(text: &str) -> Option<Self> {
        static VERSION_RE: LazyLock<Option<Regex>> =
            LazyLock::new(|| Regex::new(r"(\d{4})\.(\d{1,4})\.(\d{1,6})").ok());
        let caps = VERSION_RE.as_ref()?.captures(text)?;
        Some(Self {
            major: caps.get(1)?.as_str().parse().ok()?,
            minor: caps.get(2)?.as_str().parse().ok()?,
            patch: caps.get(3)?.as_str().parse().ok()?,
        })
    }

    pub fn is_supported(&self) -> bool {
        *self >= MIN_VERSION
    }
}

impl std::fmt::Display for SunshineVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SunshineError {
    #[error("Sunshine is not installed (install the native package; Flatpak is unsupported)")]
    NotInstalled,
    #[error("Flatpak Sunshine is not supported by Longcar; install the native package")]
    FlatpakUnsupported,
    #[error("Sunshine {found} is too old; 2026.914.0 or newer is required")]
    UnsupportedVersion { found: String },
    #[error("could not determine the Sunshine version")]
    VersionUnreadable,
    #[error("failed to run Sunshine: {0}")]
    Io(#[from] std::io::Error),
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn search_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// Resolve the native Sunshine executable: `SUNSHINE_BIN`, then `PATH`, then
/// `/usr/bin/sunshine`.
pub fn detect_sunshine_binary() -> Result<PathBuf, SunshineError> {
    if let Some(raw) = std::env::var_os("SUNSHINE_BIN") {
        let path = PathBuf::from(raw);
        if is_executable(&path) {
            return Ok(path);
        }
        return Err(SunshineError::NotInstalled);
    }
    if let Some(found) = search_path("sunshine") {
        return Ok(found);
    }
    let fallback = PathBuf::from("/usr/bin/sunshine");
    if is_executable(&fallback) {
        Ok(fallback)
    } else {
        Err(SunshineError::NotInstalled)
    }
}

/// Run `<bin> --version` and parse the release.
///
/// Sunshine only prints its version once a configuration directory resolves, and a
/// relocated (not installed) tree fails that step while looking for
/// `/usr/share/sunshine/apps.json`. A throwaway configuration directory makes the check
/// work for installed packages, extracted release archives and dev trees alike.
pub fn check_version(bin: &Path) -> Result<SunshineVersion, SunshineError> {
    let sandbox = std::env::temp_dir().join(format!(
        "longcar-version-check-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let config_dir = sandbox.join("sunshine");
    let cleaned = std::fs::create_dir_all(&config_dir).is_ok()
        && std::fs::write(
            config_dir.join("sunshine.conf"),
            "sunshine_name = Longcar version check\nmin_log_level = info\n",
        )
        .is_ok()
        && std::fs::write(config_dir.join("apps.json"), "{\"env\":{},\"apps\":[]}").is_ok();

    let mut command = Command::new(bin);
    command.arg("--version");
    if cleaned {
        command
            .env("XDG_CONFIG_HOME", &sandbox)
            .env("SUNSHINE_MIGRATE_CONFIG", "0");
    }
    let output = command.output();
    if cleaned {
        let _ = std::fs::remove_dir_all(&sandbox);
    }
    let output = output?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    SunshineVersion::parse(&text).ok_or(SunshineError::VersionUnreadable)
}

/// True when the only Sunshine installation is the unsupported Flatpak.
pub fn is_flatpak_only() -> bool {
    Command::new("flatpak")
        .args(["info", "dev.lizardbyte.app.Sunshine"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Render the exact Sunshine configuration for one screen.
pub fn render_config(screen: &ScreenConfig, base_port: u16, instance_dir: &Path) -> String {
    let mut out = String::new();
    out.push_str(&format!("sunshine_name = Longcar - {}\n", screen.name));
    out.push_str(&format!("port = {base_port}\n"));
    out.push_str("capture = portal\n");
    if let Some(encoder) = screen.encoder.sunshine_value() {
        out.push_str(&format!("encoder = {encoder}\n"));
    }
    out.push_str(&format!("hevc_mode = {}\n", screen.hevc.sunshine_value()));
    out.push_str(&format!("av1_mode = {}\n", screen.av1.sunshine_value()));
    if screen.max_bitrate_kbps > 0 {
        out.push_str(&format!("max_bitrate = {}\n", screen.max_bitrate_kbps));
    }
    if !screen.max_resolution.is_empty() {
        let (width, height) = screen.effective_max_resolution();
        out.push_str(&format!("max_stream_resolution = {width}x{height}\n"));
    }
    out.push_str(&format!(
        "stream_audio = {}\n",
        if screen.audio { "enabled" } else { "disabled" }
    ));
    out.push_str(&format!(
        "native_pen_touch = {}\n",
        if screen.touch { "enabled" } else { "disabled" }
    ));
    out.push_str("upnp = disabled\n");
    out.push_str("origin_web_ui_allowed = pc\n");
    out.push_str("system_tray = disabled\n");
    out.push_str("lan_encryption_mode = 1\n");
    out.push_str("min_log_level = info\n");
    out.push_str(&format!(
        "log_path = {}\n",
        instance_dir.join("sunshine/sunshine.log").display()
    ));
    out
}

/// Render `apps.json` for one screen, preserving Sunshine's expected key order.
pub fn render_apps(screen: &ScreenConfig) -> String {
    let name = serde_json::to_string(&screen.name).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "{{\"env\":{{\"PATH\":\"$(PATH):$(HOME)/.local/bin\"}},\"apps\":[{{\"name\":{name},\"image-path\":\"desktop.png\"}}]}}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Alignment, Av1Mode, CursorMode, Encoder, HevcMode, Position};

    fn screen() -> ScreenConfig {
        ScreenConfig {
            id: "tablet".into(),
            name: "Tablet".into(),
            width: 2560,
            height: 1600,
            refresh_hz: 120.0,
            scale: 1.5,
            position: Position::Right,
            relative_to: "primary".into(),
            alignment: Alignment::Center,
            slot: 0,
            encoder: Encoder::Auto,
            hevc: HevcMode::Auto,
            av1: Av1Mode::Off,
            max_bitrate_kbps: 30000,
            max_resolution: "2560x1600".into(),
            cursor: CursorMode::Embedded,
            audio: true,
            touch: true,
        }
    }

    #[test]
    fn parses_version_from_output() {
        assert_eq!(
            SunshineVersion::parse("Sunshine version: v2026.1201.120214"),
            Some(SunshineVersion {
                major: 2026,
                minor: 1201,
                patch: 120214
            })
        );
        assert_eq!(
            SunshineVersion::parse("2026.914.0"),
            Some(SunshineVersion {
                major: 2026,
                minor: 914,
                patch: 0
            })
        );
        assert_eq!(SunshineVersion::parse("no version here"), None);
        assert_eq!(SunshineVersion::parse("1.2.3"), None);
    }

    #[test]
    fn version_support_and_display() {
        assert!(MIN_VERSION.is_supported());
        let old = SunshineVersion::parse("2025.1231.0").unwrap();
        assert!(!old.is_supported());
        let new = SunshineVersion::parse("2027.1.0").unwrap();
        assert!(new.is_supported());
        assert_eq!(MIN_VERSION.to_string(), "2026.914.0");
        assert!(old < MIN_VERSION);
    }

    #[test]
    fn default_auto_config_has_exact_keys_and_order() {
        let text = render_config(&screen(), 48049, Path::new("/tmp/inst"));
        let expected = concat!(
            "sunshine_name = Longcar - Tablet\n",
            "port = 48049\n",
            "capture = portal\n",
            "hevc_mode = 0\n",
            "av1_mode = 1\n",
            "max_bitrate = 30000\n",
            "max_stream_resolution = 2560x1600\n",
            "stream_audio = enabled\n",
            "native_pen_touch = enabled\n",
            "upnp = disabled\n",
            "origin_web_ui_allowed = pc\n",
            "system_tray = disabled\n",
            "lan_encryption_mode = 1\n",
            "min_log_level = info\n",
            "log_path = /tmp/inst/sunshine/sunshine.log\n",
        );
        assert_eq!(text, expected);
        assert!(!text.contains("encoder ="));
    }

    #[test]
    fn every_encoder_and_codec_mode_maps() {
        for (encoder, expected) in [
            (Encoder::Vaapi, Some("vaapi")),
            (Encoder::Nvenc, Some("nvenc")),
            (Encoder::Vulkan, Some("vulkan")),
            (Encoder::Software, Some("software")),
            (Encoder::Auto, None),
        ] {
            let mut cfg = screen();
            cfg.encoder = encoder;
            let text = render_config(&cfg, 48049, Path::new("/i"));
            match expected {
                Some(value) => assert!(text.contains(&format!("encoder = {value}\n"))),
                None => assert!(!text.contains("encoder =")),
            }
        }
        for (mode, value) in [
            (HevcMode::Auto, 0),
            (HevcMode::Off, 1),
            (HevcMode::Main, 2),
            (HevcMode::Main10, 3),
        ] {
            let mut cfg = screen();
            cfg.hevc = mode;
            assert!(
                render_config(&cfg, 1, Path::new("/i")).contains(&format!("hevc_mode = {value}\n"))
            );
        }
        for (mode, value) in [
            (Av1Mode::Auto, 0),
            (Av1Mode::Off, 1),
            (Av1Mode::Main, 2),
            (Av1Mode::Main10, 3),
        ] {
            let mut cfg = screen();
            cfg.av1 = mode;
            assert!(
                render_config(&cfg, 1, Path::new("/i")).contains(&format!("av1_mode = {value}\n"))
            );
        }
    }

    #[test]
    fn omissions_and_audio_touch_flags() {
        let mut cfg = screen();
        cfg.max_bitrate_kbps = 0;
        cfg.max_resolution = String::new();
        let text = render_config(&cfg, 48050, Path::new("/i"));
        assert!(!text.contains("max_bitrate"));
        assert!(!text.contains("max_stream_resolution"));
        assert!(text.contains("stream_audio = enabled\n"));
        assert!(text.contains("native_pen_touch = enabled\n"));

        cfg.audio = false;
        cfg.touch = false;
        let text = render_config(&cfg, 48050, Path::new("/i"));
        assert!(text.contains("stream_audio = disabled\n"));
        assert!(text.contains("native_pen_touch = disabled\n"));
    }

    #[test]
    fn max_resolution_falls_back_to_screen_size() {
        let mut cfg = screen();
        cfg.width = 1920;
        cfg.height = 1200;
        cfg.max_resolution = "1280x800".into();
        assert!(
            render_config(&cfg, 48049, Path::new("/i"))
                .contains("max_stream_resolution = 1280x800\n")
        );
        cfg.max_resolution = "garbage".into();
        assert!(
            render_config(&cfg, 48049, Path::new("/i"))
                .contains("max_stream_resolution = 1920x1200\n")
        );
    }

    #[test]
    fn apps_json_is_exact() {
        let mut cfg = screen();
        cfg.name = "Desk \"quote\"".into();
        assert_eq!(
            render_apps(&screen()),
            "{\"env\":{\"PATH\":\"$(PATH):$(HOME)/.local/bin\"},\"apps\":[{\"name\":\"Tablet\",\"image-path\":\"desktop.png\"}]}"
        );
        let escaped = render_apps(&cfg);
        assert_eq!(
            escaped,
            "{\"env\":{\"PATH\":\"$(PATH):$(HOME)/.local/bin\"},\"apps\":[{\"name\":\"Desk \\\"quote\\\"\",\"image-path\":\"desktop.png\"}]}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&escaped).unwrap();
        assert_eq!(
            parsed["apps"][0]["name"],
            serde_json::Value::String("Desk \"quote\"".into())
        );
    }

    #[test]
    fn version_error_messages_are_exact() {
        assert_eq!(
            SunshineError::UnsupportedVersion {
                found: "2025.1.0".into()
            }
            .to_string(),
            "Sunshine 2025.1.0 is too old; 2026.914.0 or newer is required"
        );
        assert_eq!(
            SunshineError::FlatpakUnsupported.to_string(),
            "Flatpak Sunshine is not supported by Longcar; install the native package"
        );
    }
}
