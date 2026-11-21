// SPDX-License-Identifier: GPL-3.0-or-later

//! Transient systemd user units, one per screen.
//!
//! Each unit gives Sunshine its own process identity, so the portal sees a distinct
//! application per screen and stopping one screen cannot affect another.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Environment inherited from the daemon's own session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InheritedEnv {
    pub wayland_display: Option<String>,
    pub display: Option<String>,
    pub dbus_session_bus_address: Option<String>,
    pub xdg_runtime_dir: Option<String>,
}

impl InheritedEnv {
    /// Read the values the daemon must forward to Sunshine.
    pub fn from_process() -> Self {
        let get = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        Self {
            wayland_display: get("WAYLAND_DISPLAY"),
            display: get("DISPLAY"),
            dbus_session_bus_address: get("DBUS_SESSION_BUS_ADDRESS"),
            xdg_runtime_dir: get("XDG_RUNTIME_DIR"),
        }
    }
}

/// Everything needed to launch one screen's Sunshine unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSpec {
    /// Screen id, used to build the unit name.
    pub screen_id: String,
    /// `~/.config/longcar/instances/<screen-id>`, the unit's `XDG_CONFIG_HOME`.
    pub instance_dir: PathBuf,
    /// Absolute path to the native Sunshine binary.
    pub sunshine_bin: PathBuf,
    pub env: InheritedEnv,
}

/// Sanitize a screen id for use in a unit name: lowercase `[a-z0-9-]`, max 32 chars.
pub fn sanitize_unit_component(screen_id: &str) -> String {
    let mut out = String::new();
    for ch in screen_id.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() || ch == '-' {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("screen");
    }
    out
}

/// `longcar-sunshine-<sanitized-id>`.
pub fn unit_name(screen_id: &str) -> String {
    format!("longcar-sunshine-{}", sanitize_unit_component(screen_id))
}

/// Exactly the argument vector passed to `systemd-run --user`.
pub fn build_systemd_run_args(spec: &UnitSpec) -> Vec<String> {
    let name = unit_name(&spec.screen_id);
    let instance = spec.instance_dir.to_string_lossy().to_string();
    let env = |key: &str, value: &str| format!("--setenv={key}={value}");
    vec![
        "systemd-run".to_string(),
        "--user".to_string(),
        format!("--unit={name}"),
        "--collect".to_string(),
        "--property=PartOf=longcar-daemon.service".to_string(),
        "--property=After=longcar-daemon.service graphical-session.target".to_string(),
        "--property=UMask=0077".to_string(),
        env("XDG_CONFIG_HOME", &instance),
        env("SUNSHINE_MIGRATE_CONFIG", "0"),
        env("XDG_SESSION_DESKTOP", "ubuntu"),
        env("XDG_CURRENT_DESKTOP", "GNOME"),
        env("XDG_SESSION_TYPE", "wayland"),
        env(
            "WAYLAND_DISPLAY",
            spec.env.wayland_display.as_deref().unwrap_or(""),
        ),
        env("DISPLAY", spec.env.display.as_deref().unwrap_or("")),
        env(
            "DBUS_SESSION_BUS_ADDRESS",
            spec.env.dbus_session_bus_address.as_deref().unwrap_or(""),
        ),
        env(
            "XDG_RUNTIME_DIR",
            spec.env.xdg_runtime_dir.as_deref().unwrap_or(""),
        ),
        "--".to_string(),
        spec.sunshine_bin.to_string_lossy().to_string(),
    ]
}

/// Prefix used to find leftover units from a previous daemon run.
pub const UNIT_PREFIX: &str = "longcar-sunshine-";

fn systemctl_args(unit: &str, verb: &str) -> Vec<String> {
    vec![
        "systemctl".to_string(),
        "--user".to_string(),
        verb.to_string(),
        unit.to_string(),
    ]
}

async fn run_quiet(mut cmd: tokio::process::Command) -> Result<Option<String>, String> {
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let output = cmd.output().await.map_err(|e| format!("cannot run: {e}"))?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("exited with {}", output.status)
        } else {
            stderr
        })
    }
}

/// Start the screen's Sunshine unit. Returns the unit name.
pub async fn start(spec: &UnitSpec) -> Result<String, String> {
    let mut args = build_systemd_run_args(spec);
    let program = args.remove(0);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&args);
    run_quiet(cmd)
        .await
        .map_err(|e| format!("systemd-run failed: {e}"))?;
    Ok(unit_name(&spec.screen_id))
}

/// `systemctl --user stop --no-block <unit>`: ask systemd to stop without blocking the
/// caller.
pub fn stop_args(unit: &str) -> Vec<String> {
    vec![
        "--user".to_string(),
        "stop".to_string(),
        "--no-block".to_string(),
        unit.to_string(),
    ]
}

/// Grace period before escalating a stuck stop to SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Stop a unit and clear its failed state.
///
/// Sunshine can be blocked inside a portal request and ignore SIGTERM, which would make
/// `systemctl stop` hang for systemd's 90 s stop timeout while the orchestrator waits.
/// So the stop is issued without blocking, polled for a bounded time, then SIGKILLed.
pub async fn stop(unit: &str) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("systemctl");
    cmd.args(stop_args(unit));
    let _ = run_quiet(cmd).await;

    let deadline = tokio::time::Instant::now() + STOP_GRACE;
    loop {
        let state = unit_state(unit).await;
        if state.is_empty() || state == "inactive" || state == "failed" {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let mut kill = tokio::process::Command::new("systemctl");
            kill.args(["--user", "kill", "-s", "KILL", unit]);
            let _ = run_quiet(kill).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    reset_failed(unit).await?;
    Ok(())
}

async fn reset_failed(unit: &str) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("systemctl");
    cmd.args(&systemctl_args(unit, "reset-failed")[1..]);
    let _ = run_quiet(cmd).await;
    Ok(())
}

/// Unit names currently known to systemd that belong to Longcar.
pub async fn list_units() -> Vec<String> {
    let mut cmd = tokio::process::Command::new("systemctl");
    cmd.args([
        "--user",
        "list-units",
        "--all",
        "--no-legend",
        "--plain",
        &format!("{UNIT_PREFIX}*"),
    ]);
    match run_quiet(cmd).await {
        Ok(Some(stdout)) => stdout
            .lines()
            .filter_map(|line| {
                let name = line.split_whitespace().next()?;
                name.starts_with(UNIT_PREFIX)
                    .then(|| name.trim_end_matches(".service").to_string())
                    .map(|n| format!("{n}.service"))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Stop every leftover Sunshine unit from a previous daemon run.
pub async fn stop_leftovers() -> Vec<String> {
    let units = list_units().await;
    for unit in &units {
        let _ = stop(unit).await;
    }
    units
}

/// Last log lines of a unit, used when the Sunshine log file never appears.
pub async fn journal_tail(unit: &str, lines: usize) -> String {
    let mut cmd = tokio::process::Command::new("journalctl");
    cmd.args(["--user", "-u", unit, "-n", &lines.to_string(), "--no-pager"]);
    match run_quiet(cmd).await {
        Ok(Some(out)) => out,
        Ok(None) => String::new(),
        Err(e) => format!("journalctl failed: {e}"),
    }
}

/// Unit state string (`active`, `failed`, `inactive`, ...); empty when unknown.
pub async fn unit_state(unit: &str) -> String {
    let output = tokio::process::Command::new("systemctl")
        .args(["--user", "is-active", unit])
        .output()
        .await;
    match output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            } else {
                text
            }
        }
        Err(_) => String::new(),
    }
}

/// Path used for the previous run's Sunshine log.
pub fn previous_log_path(instance_dir: &Path) -> PathBuf {
    instance_dir.join("sunshine/sunshine.log.longcar-prev")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> UnitSpec {
        UnitSpec {
            screen_id: "tablet".to_string(),
            instance_dir: PathBuf::from("/home/user/.config/longcar/instances/tablet"),
            sunshine_bin: PathBuf::from("/usr/bin/sunshine"),
            env: InheritedEnv {
                wayland_display: Some("wayland-0".to_string()),
                display: Some(":0".to_string()),
                dbus_session_bus_address: Some("unix:path=/run/user/1000/bus".to_string()),
                xdg_runtime_dir: Some("/run/user/1000".to_string()),
            },
        }
    }

    #[test]
    fn systemd_run_args_are_exact() {
        let args = build_systemd_run_args(&spec());
        assert_eq!(
            args,
            vec![
                "systemd-run",
                "--user",
                "--unit=longcar-sunshine-tablet",
                "--collect",
                "--property=PartOf=longcar-daemon.service",
                "--property=After=longcar-daemon.service graphical-session.target",
                "--property=UMask=0077",
                "--setenv=XDG_CONFIG_HOME=/home/user/.config/longcar/instances/tablet",
                "--setenv=SUNSHINE_MIGRATE_CONFIG=0",
                "--setenv=XDG_SESSION_DESKTOP=ubuntu",
                "--setenv=XDG_CURRENT_DESKTOP=GNOME",
                "--setenv=XDG_SESSION_TYPE=wayland",
                "--setenv=WAYLAND_DISPLAY=wayland-0",
                "--setenv=DISPLAY=:0",
                "--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus",
                "--setenv=XDG_RUNTIME_DIR=/run/user/1000",
                "--",
                "/usr/bin/sunshine",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn missing_env_becomes_empty_value() {
        let mut spec = spec();
        spec.env = InheritedEnv::default();
        let args = build_systemd_run_args(&spec);
        assert!(args.contains(&"--setenv=WAYLAND_DISPLAY=".to_string()));
        assert!(args.contains(&"--setenv=DISPLAY=".to_string()));
        assert_eq!(args.last().unwrap(), "/usr/bin/sunshine");
    }

    #[test]
    fn unit_names_are_sanitized_and_bounded() {
        assert_eq!(unit_name("tablet"), "longcar-sunshine-tablet");
        assert_eq!(
            unit_name("Phone Portrait"),
            "longcar-sunshine-phone-portrait"
        );
        assert_eq!(unit_name("a/b:c"), "longcar-sunshine-a-b-c");
        assert_eq!(
            unit_name(&"x".repeat(100)),
            format!("longcar-sunshine-{}", "x".repeat(32))
        );
        assert_eq!(unit_name(""), "longcar-sunshine-screen");
        let name = unit_name("Tab--let!!");
        assert!(name.starts_with(UNIT_PREFIX));
        assert!(
            name.trim_start_matches(UNIT_PREFIX)
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }

    #[test]
    fn stop_does_not_block_the_caller() {
        assert_eq!(
            stop_args("longcar-sunshine-tablet.service"),
            [
                "--user",
                "stop",
                "--no-block",
                "longcar-sunshine-tablet.service"
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn previous_log_path_is_inside_instance() {
        assert_eq!(
            previous_log_path(Path::new("/home/user/.config/longcar/instances/tablet")),
            PathBuf::from(
                "/home/user/.config/longcar/instances/tablet/sunshine/sunshine.log.longcar-prev"
            )
        );
    }
}
