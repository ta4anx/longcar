// SPDX-License-Identifier: GPL-3.0-or-later

//! Doctor checks and safe fixes.
//!
//! `collect` gathers a human-readable snapshot of the environment needed to run
//! Longcar. Everything here is read-only except `run_fix`, which only touches
//! Longcar-owned state or well-scoped desktop settings.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Output;

use gio::prelude::*;
use longcar_core::config::Config;
use longcar_core::ports::{PortFamily, check_ports};
use longcar_core::sunshine::{check_version, detect_sunshine_binary, is_flatpak_only};
use serde::Serialize;

use crate::error::DaemonError;

/// State of a single doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Error,
}

/// One doctor check with an optional repair action.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub id: String,
    pub title: String,
    pub status: CheckStatus,
    pub detail: String,
    pub fix: Option<String>,
}

/// The complete doctor result, serialized for `GetDoctor`.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"checks\":[]}".to_string())
    }

    /// Highest severity across all checks; `Ok` for an empty report.
    pub fn worst(&self) -> CheckStatus {
        self.checks
            .iter()
            .map(|check| check.status)
            .max()
            .unwrap_or(CheckStatus::Ok)
    }
}

const EXTENSION_UUID: &str = "longcar@ta4anx.github.io";
const EXTENSION_ZIP_NAME: &str = "longcar@ta4anx.github.io.shell-extension.zip";

fn check(id: &str, title: &str, status: CheckStatus, detail: impl Into<String>) -> Check {
    Check {
        id: id.to_string(),
        title: title.to_string(),
        status,
        detail: detail.into(),
        fix: None,
    }
}

fn with_fix(mut check: Check, fix: &str) -> Check {
    check.fix = Some(fix.to_string());
    check
}

async fn run_command(program: &str, args: &[&str]) -> Result<Output, String> {
    tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|err| format!("cannot run {program}: {err}"))
}

fn output_text(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    text
}

fn parse_major(text: &str) -> Option<u32> {
    text.split_whitespace().find_map(|token| {
        let digits: String = token.chars().take_while(char::is_ascii_digit).collect();
        (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
    })
}

/// Read one D-Bus property through the standard `Properties` interface.
async fn get_property(
    connection: &zbus::Connection,
    destination: &str,
    path: &str,
    interface: &str,
    property: &str,
) -> Result<zbus::OwnedVariant, String> {
    let proxy = zbus::Proxy::new(
        connection,
        destination,
        path,
        "org.freedesktop.DBus.Properties",
    )
    .await
    .map_err(|err| err.to_string())?;
    proxy
        .call("Get", &(interface, property))
        .await
        .map_err(|err| format!("{interface}.{property}: {err}"))
}

async fn get_all_properties(
    connection: &zbus::Connection,
    destination: &str,
    path: &str,
    interface: &str,
) -> Result<HashMap<String, zbus::OwnedVariant>, String> {
    let proxy = zbus::Proxy::new(
        connection,
        destination,
        path,
        "org.freedesktop.DBus.Properties",
    )
    .await
    .map_err(|err| err.to_string())?;
    proxy
        .call("GetAll", &(interface,))
        .await
        .map_err(|err| format!("{interface} GetAll: {err}"))
}

fn as_u32(value: &zbus::OwnedVariant) -> Option<u32> {
    if let Ok(v) = value.downcast_ref::<u32>() {
        return Some(v);
    }
    if let Ok(v) = value.downcast_ref::<i32>() {
        return u32::try_from(v).ok();
    }
    if let Ok(v) = value.downcast_ref::<u8>() {
        return Some(u32::from(v));
    }
    if let Ok(v) = value.downcast_ref::<i64>() {
        return u32::try_from(v).ok();
    }
    None
}

fn as_bool(value: &zbus::OwnedVariant) -> Option<bool> {
    value.downcast_ref::<bool>().ok()
}

fn open_settings(schema_id: &str) -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(schema_id, true)?;
    Some(gio::Settings::new(schema_id))
}

fn enabled_extensions() -> Vec<String> {
    use gio::prelude::*;

    open_settings("org.gnome.shell")
        .map(|settings| {
            settings
                .strv("enabled-extensions")
                .into_iter()
                .map(|value| value.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Port availability check reused by `collect` and its unit test.
fn ports_check(config: &Config, active_screens: &[String]) -> (CheckStatus, String) {
    if config.screens.is_empty() {
        return (CheckStatus::Ok, "no screens configured".to_string());
    }
    let mut conflicts = Vec::new();
    let mut checked = 0usize;
    for screen in &config.screens {
        if active_screens.iter().any(|id| id == &screen.id) {
            continue;
        }
        checked += 1;
        match PortFamily::for_slot(screen.slot) {
            Ok(family) => {
                if let Err(err) = check_ports(&family) {
                    conflicts.push(format!(
                        "screen '{}' (slot {}): {err}",
                        screen.id, screen.slot
                    ));
                }
            }
            Err(err) => conflicts.push(format!("screen '{}': {err}", screen.id)),
        }
    }
    if checked == 0 {
        return (
            CheckStatus::Ok,
            "every configured screen is running; its own ports are in use".to_string(),
        );
    }
    if conflicts.is_empty() {
        (
            CheckStatus::Ok,
            format!(
                "{} port families are free (running screens are skipped)",
                checked
            ),
        )
    } else {
        (
            CheckStatus::Error,
            format!("{} conflict(s): {}", conflicts.len(), conflicts.join("; ")),
        )
    }
}

/// Host sinks, the default sink, and whether a screen's configured sink exists.
fn audio_check(config: &Config, active_screens: &[String]) -> (CheckStatus, String) {
    let sinks = match longcar_core::audio::list_sinks() {
        Ok(sinks) => sinks,
        Err(err) => return (CheckStatus::Warn, format!("cannot list host sinks: {err}")),
    };
    if sinks.is_empty() {
        return (CheckStatus::Warn, "no host audio sinks found".to_string());
    }
    let default = sinks.iter().find(|sink| sink.is_default);
    let mut problems = Vec::new();
    for screen in &config.screens {
        if screen.audio_sink.trim().is_empty() {
            continue;
        }
        if !sinks.iter().any(|sink| sink.name == screen.audio_sink) {
            problems.push(format!(
                "screen '{}' wants missing sink '{}'",
                screen.id, screen.audio_sink
            ));
        }
    }
    let stale =
        default.map(|sink| sink.is_sunshine()).unwrap_or(false) && active_screens.is_empty();
    let default_label = default
        .map(|sink| format!("{} ({})", sink.name, sink.description))
        .unwrap_or_else(|| "<none>".to_string());
    if stale {
        return (
            CheckStatus::Warn,
            format!(
                "the default sink is Sunshine's '{}' while no screen is streaming; the host speakers are muted",
                default.map(|sink| sink.name.clone()).unwrap_or_default()
            ),
        );
    }
    if !problems.is_empty() {
        return (CheckStatus::Error, problems.join("; "));
    }
    (
        CheckStatus::Ok,
        format!(
            "default sink: {default_label}; {} sink(s) available",
            sinks.len()
        ),
    )
}

/// Gather the full doctor report.
/// `active_screens` are screens that are running (or hold a virtual output); their own
/// ports are busy by definition and must not be reported as conflicts.
pub async fn collect(
    config: &Config,
    snapshot_present: bool,
    active_screens: &[String],
) -> DoctorReport {
    let mut checks = Vec::new();

    // 1. Session.
    let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "<unset>".into());
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "<unset>".into());
    let session_ok = session_type.eq_ignore_ascii_case("wayland")
        && desktop.to_ascii_uppercase().contains("GNOME");
    checks.push(check(
        "session",
        "GNOME Wayland session",
        if session_ok {
            CheckStatus::Ok
        } else {
            CheckStatus::Error
        },
        format!("XDG_SESSION_TYPE={session_type}, XDG_CURRENT_DESKTOP={desktop}"),
    ));

    // 2. GNOME Shell version.
    let gnome = match run_command("gnome-shell", &["--version"]).await {
        Ok(output) => {
            let text = output_text(&output);
            match parse_major(&text) {
                Some(major) if major >= 50 => {
                    check("gnome", "GNOME Shell 50 or newer", CheckStatus::Ok, text)
                }
                Some(major) => check(
                    "gnome",
                    "GNOME Shell 50 or newer",
                    CheckStatus::Error,
                    format!("GNOME Shell {major} is too old (50 required): {text}"),
                ),
                None => check(
                    "gnome",
                    "GNOME Shell 50 or newer",
                    CheckStatus::Warn,
                    format!("cannot parse the GNOME Shell version from '{text}'"),
                ),
            }
        }
        Err(err) => check("gnome", "GNOME Shell 50 or newer", CheckStatus::Warn, err),
    };
    checks.push(gnome);

    let session_bus = zbus::Connection::session()
        .await
        .map_err(|err| err.to_string());

    // 3. Mutter ScreenCast API.
    let mutter = match &session_bus {
        Ok(connection) => {
            match get_property(
                connection,
                "org.gnome.Mutter.ScreenCast",
                "/org/gnome/Mutter/ScreenCast",
                "org.gnome.Mutter.ScreenCast",
                "Version",
            )
            .await
            {
                Ok(value) => match as_u32(&value) {
                    Some(version) if version >= 4 => check(
                        "mutter",
                        "Mutter ScreenCast API",
                        CheckStatus::Ok,
                        format!("org.gnome.Mutter.ScreenCast Version={version}"),
                    ),
                    Some(version) => check(
                        "mutter",
                        "Mutter ScreenCast API",
                        CheckStatus::Error,
                        format!("ScreenCast version {version} is too old (4 required)"),
                    ),
                    None => check(
                        "mutter",
                        "Mutter ScreenCast API",
                        CheckStatus::Error,
                        format!("ScreenCast Version has unexpected type: {value:?}"),
                    ),
                },
                Err(err) => check(
                    "mutter",
                    "Mutter ScreenCast API",
                    CheckStatus::Error,
                    format!("cannot read ScreenCast properties: {err}"),
                ),
            }
        }
        Err(err) => check(
            "mutter",
            "Mutter ScreenCast API",
            CheckStatus::Error,
            format!("session bus unavailable: {err}"),
        ),
    };
    checks.push(mutter);

    // 4. Desktop portal screen capture.
    let portal = match &session_bus {
        Ok(connection) => {
            let version = get_property(
                connection,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.ScreenCast",
                "version",
            )
            .await;
            let sources = get_property(
                connection,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.ScreenCast",
                "AvailableSourceTypes",
            )
            .await;
            match (version, sources) {
                (Ok(version), Ok(sources)) => {
                    let version = as_u32(&version);
                    let sources = as_u32(&sources);
                    match (version, sources) {
                        (Some(version), Some(sources)) if version >= 4 && sources & 1 != 0 => {
                            check(
                                "portal",
                                "Desktop portal screen capture",
                                CheckStatus::Ok,
                                format!(
                                    "ScreenCast version={version}, AvailableSourceTypes={sources} (monitor sources)"
                                ),
                            )
                        }
                        (Some(version), Some(sources)) => check(
                            "portal",
                            "Desktop portal screen capture",
                            CheckStatus::Error,
                            format!(
                                "ScreenCast version={version}, AvailableSourceTypes={sources}; need version>=4 with monitor sources (bit 0)"
                            ),
                        ),
                        _ => check(
                            "portal",
                            "Desktop portal screen capture",
                            CheckStatus::Error,
                            "ScreenCast properties have unexpected types".to_string(),
                        ),
                    }
                }
                (version, sources) => {
                    let err = version.err().or_else(|| sources.err()).unwrap_or_default();
                    check(
                        "portal",
                        "Desktop portal screen capture",
                        CheckStatus::Error,
                        format!("cannot read xdg-desktop-portal ScreenCast properties: {err}"),
                    )
                }
            }
        }
        Err(err) => check(
            "portal",
            "Desktop portal screen capture",
            CheckStatus::Error,
            format!("session bus unavailable: {err}"),
        ),
    };
    checks.push(portal);

    // 5. Sunshine.
    let sunshine = if is_flatpak_only() && detect_sunshine_binary().is_err() {
        check(
            "sunshine",
            "Sunshine 2026.914.0 or newer",
            CheckStatus::Error,
            "only Flatpak Sunshine is installed; Longcar needs the native package (Flatpak is unsupported)",
        )
    } else {
        match detect_sunshine_binary() {
            Ok(binary) => match check_version(&binary) {
                Ok(version) if version.is_supported() => check(
                    "sunshine",
                    "Sunshine 2026.914.0 or newer",
                    CheckStatus::Ok,
                    format!("{} at {}", version, binary.display()),
                ),
                Ok(version) => check(
                    "sunshine",
                    "Sunshine 2026.914.0 or newer",
                    CheckStatus::Error,
                    format!("{} is too old; 2026.914.0 or newer is required", version),
                ),
                Err(err) => check(
                    "sunshine",
                    "Sunshine 2026.914.0 or newer",
                    CheckStatus::Error,
                    format!("cannot read the version of {}: {err}", binary.display()),
                ),
            },
            Err(err) => check(
                "sunshine",
                "Sunshine 2026.914.0 or newer",
                CheckStatus::Error,
                err.to_string(),
            ),
        }
    };
    checks.push(sunshine);

    // 6. systemd user manager.
    let systemd_state = run_command("systemctl", &["--user", "is-system-running"]).await;
    let systemd_run = run_command("systemd-run", &["--version"]).await;
    let systemd = match (&systemd_state, &systemd_run) {
        (Ok(state), Ok(run)) => {
            let state_text = output_text(state);
            let runner_text = output_text(run);
            let state_ok = state_text.contains("running") || state_text.contains("degraded");
            if state_ok && runner_text.to_ascii_lowercase().contains("systemd") {
                check(
                    "systemd",
                    "systemd user manager",
                    CheckStatus::Ok,
                    format!(
                        "user manager '{state_text}', {}",
                        runner_text.lines().next().unwrap_or("")
                    ),
                )
            } else {
                check(
                    "systemd",
                    "systemd user manager",
                    CheckStatus::Error,
                    format!("systemctl --user is-system-running: '{state_text}'"),
                )
            }
        }
        (Err(err), _) => check(
            "systemd",
            "systemd user manager",
            CheckStatus::Error,
            format!("cannot query the user manager: {err}"),
        ),
        (_, Err(err)) => check(
            "systemd",
            "systemd user manager",
            CheckStatus::Error,
            format!("systemd-run unavailable: {err}"),
        ),
    };
    checks.push(systemd);

    // 7. GStreamer plugins.
    let pipewire = run_command("gst-inspect-1.0", &["pipewiresrc"]).await;
    let paintable = run_command("gst-inspect-1.0", &["gtk4paintablesink"]).await;
    let gstreamer = match pipewire {
        Ok(output) if output.status.success() => {
            let paint_ok = paintable.map(|out| out.status.success()).unwrap_or(false);
            if paint_ok {
                check(
                    "gstreamer",
                    "GStreamer plugins",
                    CheckStatus::Ok,
                    "pipewiresrc and gtk4paintablesink are installed".to_string(),
                )
            } else {
                check(
                    "gstreamer",
                    "GStreamer plugins",
                    CheckStatus::Warn,
                    "pipewiresrc is installed; gtk4paintablesink is missing, so the GUI preview will be unavailable"
                        .to_string(),
                )
            }
        }
        Ok(output) => check(
            "gstreamer",
            "GStreamer plugins",
            CheckStatus::Error,
            format!(
                "gst-inspect-1.0 pipewiresrc failed: {}",
                output_text(&output)
            ),
        ),
        Err(err) => check(
            "gstreamer",
            "GStreamer plugins",
            CheckStatus::Error,
            format!("GStreamer tools unavailable: {err}"),
        ),
    };
    checks.push(gstreamer);

    // 8. Fractional scaling.
    let fractional = match open_settings("org.gnome.mutter") {
        Some(settings) => {
            use gio::prelude::*;

            let features: Vec<String> = settings
                .strv("experimental-features")
                .into_iter()
                .map(|value| value.to_string())
                .collect();
            if features.iter().any(|f| f == "scale-monitor-framebuffer") {
                check(
                    "fractional_scaling",
                    "Fractional scaling",
                    CheckStatus::Ok,
                    format!("scale-monitor-framebuffer enabled (features: {})", features.join(", ")),
                )
            } else {
                with_fix(
                    check(
                        "fractional_scaling",
                        "Fractional scaling",
                        CheckStatus::Warn,
                        format!(
                            "scale-monitor-framebuffer missing from org.gnome.mutter experimental-features (features: {})",
                            if features.is_empty() {
                                "<none>".to_string()
                            } else {
                                features.join(", ")
                            }
                        ),
                    ),
                    "enable-fractional-scaling",
                )
            }
        }
        None => check(
            "fractional_scaling",
            "Fractional scaling",
            CheckStatus::Warn,
            "org.gnome.mutter GSettings schema is unavailable; enable scale-monitor-framebuffer manually".to_string(),
        ),
    };
    checks.push(fractional);

    // 9. Display configuration mode.
    let layout_mode = match &session_bus {
        Ok(connection) => {
            let single = get_property(
                connection,
                "org.gnome.Mutter.DisplayConfig",
                "/org/gnome/Mutter/DisplayConfig",
                "org.gnome.Mutter.DisplayConfig",
                "LayoutMode",
            )
            .await;
            let from_all = get_all_properties(
                connection,
                "org.gnome.Mutter.DisplayConfig",
                "/org/gnome/Mutter/DisplayConfig",
                "org.gnome.Mutter.DisplayConfig",
            )
            .await;
            let mode = match &single {
                Ok(value) => as_u32(value),
                Err(_) => from_all
                    .as_ref()
                    .ok()
                    .and_then(|all| all.get("LayoutMode"))
                    .and_then(as_u32),
            };
            let global_scale = from_all
                .as_ref()
                .ok()
                .and_then(|all| all.get("GlobalScaleRequired"))
                .and_then(as_bool);
            let apply_allowed = from_all
                .as_ref()
                .ok()
                .and_then(|all| all.get("ApplyMonitorsConfigAllowed"))
                .and_then(as_bool);
            if global_scale == Some(true) {
                check(
                    "layout_mode",
                    "Display configuration mode",
                    CheckStatus::Error,
                    "GlobalScaleRequired is true; Longcar needs per-monitor scaling".to_string(),
                )
            } else {
                match mode {
                    Some(1) => check(
                        "layout_mode",
                        "Display configuration mode",
                        CheckStatus::Ok,
                        "LayoutMode=1 (per-monitor layout)".to_string(),
                    ),
                    Some(2) => check(
                        "layout_mode",
                        "Display configuration mode",
                        CheckStatus::Error,
                        "LayoutMode=2 means one monitor per physical display; Longcar cannot manage virtual monitors in this mode"
                            .to_string(),
                    ),
                    Some(other) => check(
                        "layout_mode",
                        "Display configuration mode",
                        CheckStatus::Error,
                        format!("unexpected LayoutMode={other}"),
                    ),
                    // This Mutter build (GNOME 50 on Ubuntu) does not expose
                    // LayoutMode at all; per-monitor layout is the only mode it
                    // supports, so absence is fine unless configs are blocked.
                    None => {
                        if apply_allowed == Some(false) {
                            check(
                                "layout_mode",
                                "Display configuration mode",
                                CheckStatus::Error,
                                "ApplyMonitorsConfigAllowed is false; Longcar cannot apply a monitor layout"
                                    .to_string(),
                            )
                        } else {
                            check(
                                "layout_mode",
                                "Display configuration mode",
                                CheckStatus::Ok,
                                format!(
                                    "LayoutMode is not exposed by this Mutter build (per-monitor layout assumed), ApplyMonitorsConfigAllowed={apply_allowed:?}"
                                ),
                            )
                        }
                    }
                }
            }
        }
        Err(err) => check(
            "layout_mode",
            "Display configuration mode",
            CheckStatus::Error,
            format!("session bus unavailable: {err}"),
        ),
    };
    checks.push(layout_mode);

    // 10. Input injection device.
    let uinput = match std::fs::metadata("/dev/uinput") {
        Ok(metadata) => {
            use std::os::unix::fs::PermissionsExt;

            let mode = metadata.permissions().mode();
            if mode & 0o400 != 0 && mode & 0o200 != 0 {
                check(
                    "uinput",
                    "Input injection",
                    CheckStatus::Ok,
                    format!("/dev/uinput present with mode {:o}", mode & 0o777),
                )
            } else {
                check(
                    "uinput",
                    "Input injection",
                    CheckStatus::Warn,
                    format!(
                        "/dev/uinput mode {:o} is not readable and writable here; remote input may not work",
                        mode & 0o777
                    ),
                )
            }
        }
        Err(err) => check(
            "uinput",
            "Input injection",
            CheckStatus::Warn,
            format!("/dev/uinput is unavailable ({err}); remote input will not work"),
        ),
    };
    checks.push(uinput);

    // 11. Port families.
    let (port_status, port_detail) = ports_check(config, active_screens);
    checks.push(check("ports", "Port families", port_status, port_detail));

    // 12. Host audio.
    let (audio_status, audio_detail) = audio_check(config, active_screens);
    let audio = check("audio", "Host audio", audio_status, audio_detail);
    checks.push(if audio_status == CheckStatus::Ok {
        audio
    } else {
        with_fix(audio, "restore-host-audio")
    });

    // 13. Pending layout snapshot.
    if snapshot_present && active_screens.is_empty() {
        checks.push(with_fix(
            check(
                "snapshot",
                "Pending layout snapshot",
                CheckStatus::Warn,
                "a layout snapshot from an earlier run is waiting to be restored".to_string(),
            ),
            "restore-layout",
        ));
    } else if snapshot_present {
        checks.push(check(
            "snapshot",
            "Pending layout snapshot",
            CheckStatus::Ok,
            "a layout snapshot is held by the running screens and restored on stop".to_string(),
        ));
    } else {
        checks.push(check(
            "snapshot",
            "Pending layout snapshot",
            CheckStatus::Ok,
            "no pending layout snapshot".to_string(),
        ));
    }

    // 14. Quick Settings extension.
    let installed = extension_installed();
    let enabled = enabled_extensions().iter().any(|id| id == EXTENSION_UUID);
    let extension = if installed && enabled {
        check(
            "extension",
            "Quick Settings extension",
            CheckStatus::Ok,
            format!("{EXTENSION_UUID} is installed and enabled"),
        )
    } else if installed {
        with_fix(
            check(
                "extension",
                "Quick Settings extension",
                CheckStatus::Warn,
                format!(
                    "{EXTENSION_UUID} is installed but not in org.gnome.shell enabled-extensions"
                ),
            ),
            "install-extension",
        )
    } else {
        with_fix(
            check(
                "extension",
                "Quick Settings extension",
                CheckStatus::Warn,
                format!("{EXTENSION_UUID} is not installed for this user"),
            ),
            "install-extension",
        )
    };
    checks.push(extension);

    // 15. Unique screen resolutions.
    let duplicates = match config.validate() {
        Ok(()) => check(
            "duplicates",
            "Unique screen resolutions",
            CheckStatus::Ok,
            format!(
                "{} configured screens are valid and unique",
                config.screens.len()
            ),
        ),
        Err(err) => check(
            "duplicates",
            "Unique screen resolutions",
            CheckStatus::Error,
            err.to_string(),
        ),
    };
    checks.push(duplicates);

    DoctorReport { checks }
}

/// Apply one of the fixes the daemon can perform safely by itself.
pub async fn run_fix(fix_id: &str) -> Result<String, DaemonError> {
    match fix_id {
        "enable-fractional-scaling" => enable_fractional_scaling(),
        "install-extension" => install_extension().await,
        other => Err(DaemonError::InvalidArgs(format!(
            "unknown fix id '{other}'"
        ))),
    }
}

fn enable_fractional_scaling() -> Result<String, DaemonError> {
    use gio::prelude::*;

    let settings = open_settings("org.gnome.mutter").ok_or_else(|| {
        DaemonError::Io("org.gnome.mutter GSettings schema is unavailable".to_string())
    })?;
    let existing: Vec<String> = settings
        .strv("experimental-features")
        .into_iter()
        .map(|value| value.to_string())
        .collect();
    let mut features = existing.clone();
    if !features.iter().any(|f| f == "scale-monitor-framebuffer") {
        features.push("scale-monitor-framebuffer".to_string());
    }
    let refs: Vec<&str> = features.iter().map(String::as_str).collect();
    settings
        .set_strv("experimental-features", refs.as_slice())
        .map_err(|err| {
            DaemonError::Io(format!(
                "cannot write org.gnome.mutter experimental-features: {err}"
            ))
        })?;
    Ok(format!(
        "enabled scale-monitor-framebuffer; existing features: {}",
        if existing.is_empty() {
            "<none>".to_string()
        } else {
            existing.join(", ")
        }
    ))
}

async fn install_extension() -> Result<String, DaemonError> {
    let zip = extension_zip().ok_or_else(|| {
        DaemonError::NotFound(format!(
            "extension zip not found; looked in: {}",
            extension_zip_candidates()
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;

    let install = run_command(
        "gnome-extensions",
        &["install", "--force", &zip.display().to_string()],
    )
    .await
    .map_err(DaemonError::Io)?;
    if !install.status.success() {
        return Err(DaemonError::Io(format!(
            "gnome-extensions install --force {} failed: {}",
            zip.display(),
            output_text(&install)
        )));
    }
    let enable = run_command("gnome-extensions", &["enable", EXTENSION_UUID])
        .await
        .map_err(DaemonError::Io)?;
    if enable.status.success() {
        return Ok(format!("installed and enabled {EXTENSION_UUID}"));
    }

    // The running Shell cannot enable an extension it has not seen yet (it reports
    // "does not exist" until the next login). Enable it through GSettings instead so the
    // next login loads it.
    let output = output_text(&enable);
    enable_extension_via_gsettings()?;
    Ok(format!(
        "installed {EXTENSION_UUID} and added it to enabled-extensions ({output}); log out and back in once"
    ))
}

/// Append the extension UUID to `org.gnome.shell enabled-extensions`, keeping every
/// existing entry.
fn enable_extension_via_gsettings() -> Result<(), DaemonError> {
    let settings = gio::Settings::new("org.gnome.shell");
    let mut enabled = settings.strv("enabled-extensions");
    let uuid = EXTENSION_UUID.to_string();
    if !enabled.iter().any(|entry| entry.as_str() == uuid.as_str()) {
        enabled.push(uuid.clone().into());
        settings
            .set_strv("enabled-extensions", &enabled)
            .map_err(|e| DaemonError::Io(format!("cannot enable {uuid}: {e}")))?;
    }
    Ok(())
}

fn extension_zip_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("LONGCAR_EXTENSION_ZIP") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from("/usr/share/longcar").join(EXTENSION_ZIP_NAME));
    candidates.push(PathBuf::from("build").join(EXTENSION_ZIP_NAME));
    if let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR") {
        candidates.push(
            PathBuf::from(manifest)
                .join("..")
                .join("..")
                .join("build")
                .join(EXTENSION_ZIP_NAME),
        );
    }
    candidates
}

fn resolve_zip(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.is_file()).cloned()
}

/// Locate the packaged extension zip.
pub fn extension_zip() -> Option<PathBuf> {
    resolve_zip(&extension_zip_candidates())
}

fn extension_metadata_path() -> Option<PathBuf> {
    dirs::data_dir().map(|data| {
        data.join("gnome-shell")
            .join("extensions")
            .join(EXTENSION_UUID)
            .join("metadata.json")
    })
}

/// Whether the extension is installed for the current user.
pub fn extension_installed() -> bool {
    extension_metadata_path().is_some_and(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use longcar_core::config::ScreenConfig;
    use longcar_core::model::{Alignment, Av1Mode, CursorMode, Encoder, HevcMode, Position};
    use std::net::TcpListener;

    fn sample_screen(slot: u8) -> ScreenConfig {
        ScreenConfig {
            id: "tablet".to_string(),
            name: "Tablet".to_string(),
            width: 2560,
            height: 1600,
            refresh_hz: 120.0,
            scale: 1.5,
            position: Position::Right,
            relative_to: "primary".to_string(),
            alignment: Alignment::Center,
            slot,
            encoder: Encoder::Auto,
            hevc: HevcMode::Auto,
            av1: Av1Mode::Off,
            max_bitrate_kbps: 0,
            max_resolution: String::new(),
            cursor: CursorMode::Embedded,
            audio: true,
            touch: true,
            keyboard: true,
            mouse: true,
            controller: true,
            audio_sink: String::new(),
        }
    }

    #[test]
    fn report_json_has_checks_array() {
        let report = DoctorReport {
            checks: vec![check("session", "Session", CheckStatus::Ok, "fine")],
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&report.to_json()).expect("valid JSON");
        assert!(parsed["checks"].as_array().is_some());
        assert_eq!(parsed["checks"][0]["id"], "session");
    }

    #[test]
    fn report_serializes_lowercase_status_and_null_fix() {
        let report = DoctorReport {
            checks: vec![
                check("a", "A", CheckStatus::Ok, "ok"),
                with_fix(
                    check("b", "B", CheckStatus::Warn, "warn"),
                    "install-extension",
                ),
                check("c", "C", CheckStatus::Error, "error"),
            ],
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&report.to_json()).expect("valid JSON");
        assert_eq!(parsed["checks"][0]["status"], "ok");
        assert_eq!(parsed["checks"][1]["status"], "warn");
        assert_eq!(parsed["checks"][2]["status"], "error");
        assert!(parsed["checks"][0]["fix"].is_null());
        assert_eq!(parsed["checks"][1]["fix"], "install-extension");
    }

    #[test]
    fn worst_reports_highest_severity() {
        let ok = DoctorReport {
            checks: vec![check("a", "A", CheckStatus::Ok, "")],
        };
        assert_eq!(ok.worst(), CheckStatus::Ok);
        assert_eq!(DoctorReport { checks: vec![] }.worst(), CheckStatus::Ok);

        let mixed = DoctorReport {
            checks: vec![
                check("a", "A", CheckStatus::Ok, ""),
                check("b", "B", CheckStatus::Warn, ""),
                check("c", "C", CheckStatus::Error, ""),
            ],
        };
        assert_eq!(mixed.worst(), CheckStatus::Error);

        let warned = DoctorReport {
            checks: vec![
                check("a", "A", CheckStatus::Ok, ""),
                check("b", "B", CheckStatus::Warn, ""),
            ],
        };
        assert_eq!(warned.worst(), CheckStatus::Warn);
    }

    #[test]
    fn resolve_zip_prefers_existing_file() {
        let dir = std::env::temp_dir().join(format!("longcar-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let zip = dir.join(EXTENSION_ZIP_NAME);
        std::fs::write(&zip, b"zip").expect("write zip");

        let missing = dir.join("missing.zip");
        let candidates = [missing.clone(), zip.clone()];
        assert_eq!(resolve_zip(&candidates), Some(zip));
        assert_eq!(resolve_zip(&[missing]), None);
        assert_eq!(resolve_zip(&[]), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn port_check_skips_running_screens() {
        let mut config = Config::default();
        let mut phone = sample_screen(1);
        phone.id = "phone".to_string();
        phone.name = "Phone".to_string();
        config.screens.push(phone);
        // Occupy the second screen's web port so a check would report a conflict.
        let family = PortFamily::for_slot(1).expect("slot 1");
        let listener = match TcpListener::bind(("127.0.0.1", family.web)) {
            Ok(listener) => listener,
            Err(_) => return, // port already busy for another reason; nothing to prove
        };
        let (status, detail) = ports_check(&config, &[]);
        assert_eq!(status, CheckStatus::Error, "{detail}");
        assert!(detail.contains("48110"), "{detail}");
        drop(listener);

        let active = vec!["phone".to_string()];
        let (status, detail) = ports_check(&config, &active);
        assert_eq!(status, CheckStatus::Ok, "{detail}");
        assert!(detail.contains("running"), "{detail}");
    }

    #[test]
    fn port_conflict_detail_names_ss_hint() {
        // Slot 7's https port, chosen for the lowest collision risk.
        let family = PortFamily::for_slot(7).expect("slot 7");
        let Ok(_listener) = TcpListener::bind(("127.0.0.1", family.https)) else {
            return;
        };
        let config = Config {
            screens: vec![sample_screen(7)],
            ..Config::default()
        };
        let (status, detail) = ports_check(&config, &[]);
        assert_eq!(status, CheckStatus::Error, "detail: {detail}");
        assert!(detail.contains("ss -ltnup"), "detail: {detail}");
    }

    #[test]
    fn unknown_fix_is_rejected() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let err = runtime
            .block_on(run_fix("reset-instance:tablet"))
            .expect_err("unknown fix");
        assert!(matches!(&err, DaemonError::InvalidArgs(_)));
        assert!(err.to_string().contains("unknown fix id"));
    }

    #[test]
    #[ignore = "requires a live GNOME session"]
    fn real_collect_report() {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let report = runtime.block_on(collect(&Config::default(), false, &[]));
        println!("{}", report.to_json());
    }
}
