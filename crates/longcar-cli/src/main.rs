// SPDX-License-Identifier: GPL-3.0-or-later

//! `longcarctl` — command-line control for the Longcar daemon.
//!
//! Every command is a thin D-Bus client: it validates its own arguments,
//! calls one `io.github.ta4anx.Longcar.Daemon.Control` method and, for start
//! and stop, waits for the state document to settle. Exit codes: `0` success,
//! `1` operation error, `2` daemon unreachable.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::Duration;

use clap::{Parser, Subcommand};
use longcar_core::model::{EXTENSION_UUID, ScreenState};
use longcar_core::ports::PortFamily;
use longcar_core::state::{HostAddressEntry, StateDoc};
use serde_json::Value;

/// How long `start` waits for screens to become ready.
const START_TIMEOUT: Duration = Duration::from_secs(180);
/// How long `stop` waits for screens to leave their active states.
const STOP_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the state document is re-read while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Client-side limit for one pairing call.
const PAIR_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// D-Bus proxy
// ---------------------------------------------------------------------------

#[zbus::proxy(
    interface = "io.github.ta4anx.Longcar.Daemon.Control",
    default_service = "io.github.ta4anx.Longcar.Daemon",
    default_path = "/io/github/ta4anx/Longcar/Daemon"
)]
trait Daemon {
    fn get_state(&self) -> zbus::Result<String>;
    fn start_default(&self) -> zbus::Result<()>;
    fn stop_all(&self) -> zbus::Result<()>;
    fn start_screen(&self, id: &str) -> zbus::Result<()>;
    fn stop_screen(&self, id: &str) -> zbus::Result<()>;
    fn add_screen(&self, json: &str) -> zbus::Result<String>;
    fn update_screen(&self, json: &str) -> zbus::Result<()>;
    fn remove_screen(&self, id: &str, purge: bool) -> zbus::Result<()>;
    fn apply_preset(&self, preset_id: &str) -> zbus::Result<()>;
    fn set_audio_screen(&self, id: &str) -> zbus::Result<()>;
    fn set_screen_audio(&self, screen_id: &str, enabled: bool) -> zbus::Result<()>;
    fn set_screen_touch(&self, screen_id: &str, enabled: bool) -> zbus::Result<()>;
    fn set_screen_bitrate(&self, screen_id: &str, kbps: u32) -> zbus::Result<()>;
    fn set_screen_layout(
        &self,
        screen_id: &str,
        position: &str,
        relative_to: &str,
        alignment: &str,
    ) -> zbus::Result<()>;
    fn set_screen_input(&self, screen_id: &str, kind: &str, enabled: bool) -> zbus::Result<()>;
    fn set_screen_audio_sink(&self, screen_id: &str, sink: &str) -> zbus::Result<()>;
    fn get_audio_info(&self) -> zbus::Result<String>;
    fn start_speed_test(&self) -> zbus::Result<String>;
    fn stop_speed_test(&self) -> zbus::Result<()>;
    fn restore_host_audio(&self) -> zbus::Result<()>;
    fn export_config(&self) -> zbus::Result<String>;
    fn import_config(&self, toml: &str) -> zbus::Result<()>;
    fn list_pairings(&self, screen_id: &str) -> zbus::Result<String>;
    fn pair(&self, screen_id: &str, pairing_id: &str, pin: &str, name: &str) -> zbus::Result<bool>;
    fn unpair(&self, screen_id: &str, uuid: &str) -> zbus::Result<()>;
    fn set_client_enabled(&self, screen_id: &str, uuid: &str, enabled: bool) -> zbus::Result<()>;
    fn discover_hosts(&self) -> zbus::Result<String>;
    fn get_logs(&self, limit: u32) -> zbus::Result<String>;
    fn get_doctor(&self) -> zbus::Result<String>;
    fn run_doctor_fix(&self, fix_id: &str) -> zbus::Result<String>;
    fn doctor_display_test(&self) -> zbus::Result<(bool, String)>;
    fn restore_layout(&self) -> zbus::Result<()>;
    fn discard_layout_snapshot(&self) -> zbus::Result<()>;
}

// ---------------------------------------------------------------------------
// Errors and process exit codes
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum CliError {
    /// The session bus does not know the daemon; exit code 2.
    Unreachable,
    /// A normal operation failure; the message goes to stderr; exit code 1.
    Failed(String),
    /// A message was already printed; exit with this code.
    Silent(i32),
}

type CliResult<T> = Result<T, CliError>;

fn failed(message: impl Into<String>) -> CliError {
    CliError::Failed(message.into())
}

fn is_unreachable(err: &zbus::Error) -> bool {
    match err {
        zbus::Error::FDO(inner) => matches!(
            inner.as_ref(),
            zbus::fdo::Error::ServiceUnknown(_)
                | zbus::fdo::Error::NameHasNoOwner(_)
                | zbus::fdo::Error::Disconnected(_)
                | zbus::fdo::Error::NoServer(_)
                | zbus::fdo::Error::BadAddress(_)
        ),
        zbus::Error::MethodError(name, _, _) => {
            let name = name.to_string();
            name.ends_with(".ServiceUnknown")
                || name.ends_with(".NameHasNoOwner")
                || name.ends_with(".Disconnected")
                || name.ends_with(".NoServer")
        }
        zbus::Error::InputOutput(_) | zbus::Error::Address(_) | zbus::Error::Handshake(_) => true,
        _ => false,
    }
}

/// The daemon puts the stage and detail into the D-Bus error message, so do
/// not wrap it in zbus' own boilerplate.
fn describe_zbus(err: &zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(_, Some(detail), _) => detail.clone(),
        zbus::Error::FDO(inner) => inner.to_string(),
        other => other.to_string(),
    }
}

fn map_zbus(err: zbus::Error) -> CliError {
    if is_unreachable(&err) {
        CliError::Unreachable
    } else {
        failed(describe_zbus(&err))
    }
}

fn map_fix_zbus(err: zbus::Error) -> CliError {
    if is_unreachable(&err) {
        CliError::Unreachable
    } else {
        failed(format!("fix failed: {}", describe_zbus(&err)))
    }
}

// ---------------------------------------------------------------------------
// CLI surface
// ---------------------------------------------------------------------------

#[derive(Debug, Parser)]
#[command(
    name = "longcarctl",
    version,
    about = "Control Longcar virtual displays and Sunshine streams"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show daemon, screen and preset status.
    Status {
        /// Print the state document as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start the default preset, a preset, or a single screen.
    Start {
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        screen: Option<String>,
        /// Return as soon as the daemon accepts the command.
        #[arg(long = "no-wait")]
        no_wait: bool,
    },
    /// Stop one screen, or every screen.
    Stop {
        #[arg(long)]
        screen: Option<String>,
        /// Return as soon as the daemon accepts the command.
        #[arg(long = "no-wait")]
        no_wait: bool,
    },
    /// List and edit configured screens.
    Screens {
        #[command(subcommand)]
        command: ScreensCommand,
    },
    /// List and start presets.
    Presets {
        #[command(subcommand)]
        command: PresetsCommand,
    },
    /// Pair and manage Moonlight clients.
    Devices {
        #[command(subcommand)]
        command: DevicesCommand,
    },
    /// Show host addresses and the port table.
    Network {
        #[command(subcommand)]
        command: NetworkCommand,
    },
    /// Inspect the environment and run the display self-test.
    Doctor {
        #[arg(long)]
        json: bool,
        #[arg(long = "self-test")]
        self_test: bool,
    },
    /// Audio routing: no arguments shows host sinks, `off`/`SCREEN` picks the legacy
    /// single-audio view, `--restore-host` puts the host speakers back.
    Audio {
        target: Option<String>,
        /// Put the host back on a physical sink (undo a stale Sunshine sink).
        #[arg(long = "restore-host")]
        restore_host: bool,
    },
    /// Print the configuration as TOML (for backups or another machine).
    Export {
        /// Write to a file instead of stdout.
        #[arg(short = 'o', long)]
        output: Option<String>,
    },
    /// Replace the configuration from a TOML file.
    Import {
        file: String,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Apply a doctor fix.
    Fix { fix_id: String },
    /// Print daemon and screen logs.
    Logs {
        #[arg(short = 'n', default_value_t = 200)]
        n: u32,
        #[arg(long)]
        json: bool,
    },
    /// Restore the saved physical layout.
    RestoreLayout,
    /// Discard the saved physical layout snapshot.
    DiscardLayout,
    /// Install the GNOME extension and enable the user service.
    Setup {
        #[arg(long = "extension-only")]
        extension_only: bool,
        #[arg(long = "service-only")]
        service_only: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ScreensCommand {
    /// List configured screens.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add a screen from a JSON `ScreenConfig` object.
    Add {
        #[arg(long)]
        json: String,
    },
    /// Replace a screen's configuration from a JSON object.
    Update {
        id: String,
        #[arg(long)]
        json: String,
    },
    /// Stop and remove a screen.
    Remove {
        id: String,
        /// Delete the instance directory (pairings and credentials).
        #[arg(long)]
        purge: bool,
        /// Confirm destructive operations.
        #[arg(long)]
        yes: bool,
    },
    /// Apply a bitrate preset (kbps, `0` removes the cap).
    Bitrate { id: String, kbps: u32 },
    /// Move a screen next to `--relative-to` (default: its current reference).
    Position {
        id: String,
        /// left, right, above or below.
        position: String,
        #[arg(long = "relative-to")]
        relative_to: Option<String>,
        #[arg(long)]
        alignment: Option<String>,
    },
    /// Stream this screen's audio to its client.
    Audio {
        id: String,
        /// on or off.
        state: String,
    },
    /// Send native pen/touch events for this screen.
    Touch {
        id: String,
        /// on or off.
        state: String,
    },
    /// Allow keyboard, mouse or controller input from this screen's client.
    Input {
        id: String,
        /// keyboard, mouse or controller.
        kind: String,
        /// on or off.
        state: String,
    },
    /// Capture this screen's audio from a host sink (`auto` clears the override).
    Sink { id: String, sink: String },
}

#[derive(Debug, Subcommand)]
enum PresetsCommand {
    /// List presets.
    List,
    /// Start every screen of a preset.
    Start {
        id: String,
        #[arg(long = "no-wait")]
        no_wait: bool,
    },
}

#[derive(Debug, Subcommand)]
enum DevicesCommand {
    /// Show pairing requests waiting for a PIN.
    Pending { screen: String },
    /// Show paired clients of one screen.
    List { screen: String },
    /// Approve a pairing request.
    Pair {
        screen: String,
        pairing_id: String,
        pin: String,
        name: Option<String>,
    },
    /// Remove a paired client.
    Unpair { screen: String, uuid: String },
    /// Allow a paired client to connect.
    Enable { screen: String, uuid: String },
    /// Block a paired client.
    Disable { screen: String, uuid: String },
}

#[derive(Debug, Subcommand)]
enum NetworkCommand {
    /// Discover Sunshine hosts on the local network.
    Hosts,
    /// Print the port table and copyable ufw rules.
    Ports {
        #[arg(long)]
        screen: Option<String>,
        #[arg(long)]
        cidr: Option<String>,
    },
    /// Show the host Wi-Fi link (SSID, signal, negotiated rates).
    Link,
    /// Measure LAN throughput with a device: prints a URL to open on it, or shows the
    /// last result with `--status`, or stops the listener with `--stop`.
    Speedtest {
        #[arg(long)]
        stop: bool,
        #[arg(long)]
        status: bool,
    },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(()) => 0,
        Err(CliError::Unreachable) => {
            eprintln!(
                "Longcar daemon is not running (systemctl --user start longcar-daemon.service)"
            );
            2
        }
        Err(CliError::Failed(message)) => {
            eprintln!("{message}");
            1
        }
        Err(CliError::Silent(code)) => code,
    };
    std::process::exit(code);
}

async fn run() -> CliResult<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Status { json } => status(json).await,
        Command::Start {
            preset,
            screen,
            no_wait,
        } => start(preset, screen, no_wait).await,
        Command::Stop { screen, no_wait } => stop(screen, no_wait).await,
        Command::Screens { command } => screens(command).await,
        Command::Presets { command } => presets(command).await,
        Command::Devices { command } => devices(command).await,
        Command::Network { command } => network(command).await,
        Command::Doctor { json, self_test } => doctor(json, self_test).await,
        Command::Fix { fix_id } => fix(&fix_id).await,
        Command::Audio {
            target,
            restore_host,
        } => audio(target, restore_host).await,
        Command::Export { output } => export_config(output).await,
        Command::Import { file, yes } => import_config(&file, yes).await,
        Command::Logs { n, json } => logs(n, json).await,
        Command::RestoreLayout => restore_layout().await,
        Command::DiscardLayout => discard_layout().await,
        Command::Setup {
            extension_only,
            service_only,
        } => setup(extension_only, service_only).await,
    }
}

// ---------------------------------------------------------------------------
// D-Bus helpers
// ---------------------------------------------------------------------------

async fn connect() -> CliResult<DaemonProxy<'static>> {
    let connection = zbus::Connection::session().await.map_err(map_zbus)?;
    DaemonProxy::new(&connection).await.map_err(map_zbus)
}

async fn fetch_state(proxy: &DaemonProxy<'_>) -> CliResult<StateDoc> {
    let raw = proxy.get_state().await.map_err(map_zbus)?;
    StateDoc::from_json(&raw)
        .map_err(|err| failed(format!("invalid state payload from daemon: {err}")))
}

fn print_json(value: &Value) -> CliResult<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|err| failed(format!("cannot render JSON: {err}")))?;
    println!("{text}");
    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

async fn status(json: bool) -> CliResult<()> {
    let proxy = connect().await?;
    let state = fetch_state(&proxy).await?;
    if json {
        let value = serde_json::to_value(&state)
            .map_err(|err| failed(format!("cannot render JSON: {err}")))?;
        return print_json(&value);
    }
    print_status(&state);
    Ok(())
}

fn preferred_address(state: &StateDoc) -> Option<&HostAddressEntry> {
    state
        .host
        .addresses
        .iter()
        .find(|address| address.preferred)
        .or_else(|| state.host.addresses.first())
}

fn print_status(state: &StateDoc) {
    println!(
        "daemon: {} (since {})",
        state.daemon.state.as_str(),
        state.daemon.since
    );
    if let Some(address) = preferred_address(state) {
        let preferred = if address.preferred { ", preferred" } else { "" };
        println!(
            "host: {} ({}, {}{})",
            address.address, address.interface, address.kind, preferred
        );
    }
    if !state.warnings.is_empty() {
        println!("warnings: {}", state.warnings.join(", "));
    }
    if state.screens.is_empty() {
        println!("screens: none configured");
    } else {
        println!("screens:");
        for screen in &state.screens {
            let mut line = format!(
                "  {:<12} {:<24} {}",
                screen.id,
                screen.name,
                screen.state.as_str()
            );
            if let Some(connector) = &screen.connector {
                let _ = write!(line, " [{connector}]");
            }
            if matches!(screen.state, ScreenState::Ready | ScreenState::Streaming) {
                let _ = write!(
                    line,
                    " {} kbps, {} fps{}",
                    screen.bitrate_kbps,
                    screen.fps,
                    if screen.connected { ", connected" } else { "" }
                );
            }
            println!("{line}");
            if let Some(error) = &screen.error {
                println!("    failed at {:?}: {}", error.stage, error.message);
            }
        }
    }
    if !state.presets.is_empty() {
        println!("presets:");
        for preset in &state.presets {
            let default = if preset.id == state.settings.default_preset {
                " (default)"
            } else {
                ""
            };
            println!(
                "  {} - {}{}: {}",
                preset.id,
                preset.name,
                default,
                preset.screens.join(", ")
            );
        }
    }
}

// ---------------------------------------------------------------------------
// start / stop
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum StartMode {
    Default,
    Preset(String),
    Screen(String),
}

fn start_mode(preset: Option<String>, screen: Option<String>) -> CliResult<StartMode> {
    match (preset, screen) {
        (Some(_), Some(_)) => Err(failed("choose either --preset or --screen")),
        (Some(id), None) => Ok(StartMode::Preset(id)),
        (None, Some(id)) => Ok(StartMode::Screen(id)),
        (None, None) => Ok(StartMode::Default),
    }
}

fn preset_screens<'a>(state: &'a StateDoc, preset_id: &str) -> CliResult<&'a [String]> {
    let preset = state
        .presets
        .iter()
        .find(|preset| preset.id == preset_id)
        .ok_or_else(|| failed(format!("preset '{preset_id}' does not exist")))?;
    if preset.screens.is_empty() {
        return Err(failed(format!("preset '{preset_id}' has no screens")));
    }
    Ok(&preset.screens)
}

fn resolve_start_targets(state: &StateDoc, mode: &StartMode) -> CliResult<Vec<String>> {
    match mode {
        StartMode::Screen(id) => {
            if !state.screens.iter().any(|screen| &screen.id == id) {
                return Err(failed(format!("screen '{id}' is not configured")));
            }
            Ok(vec![id.clone()])
        }
        StartMode::Preset(id) => Ok(preset_screens(state, id)?.to_vec()),
        StartMode::Default => {
            let id = &state.settings.default_preset;
            if id.is_empty() {
                return Err(failed("no default preset is configured"));
            }
            Ok(preset_screens(state, id)?.to_vec())
        }
    }
}

async fn start(preset: Option<String>, screen: Option<String>, no_wait: bool) -> CliResult<()> {
    let mode = start_mode(preset, screen)?;
    let proxy = connect().await?;
    match &mode {
        StartMode::Default => proxy.start_default().await.map_err(map_zbus)?,
        StartMode::Preset(id) => proxy.apply_preset(id).await.map_err(map_zbus)?,
        StartMode::Screen(id) => proxy.start_screen(id).await.map_err(map_zbus)?,
    }
    if no_wait {
        println!("start requested");
        return Ok(());
    }
    let state = fetch_state(&proxy).await?;
    let targets = resolve_start_targets(&state, &mode)?;
    println!("waiting for: {}", targets.join(", "));
    wait_for_ready(&proxy, &targets).await
}

async fn wait_for_ready(proxy: &DaemonProxy<'_>, targets: &[String]) -> CliResult<()> {
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        let state = fetch_state(proxy).await?;
        let mut pending = Vec::new();
        for id in targets {
            let screen = state
                .screens
                .iter()
                .find(|screen| &screen.id == id)
                .ok_or_else(|| {
                    failed(format!("screen '{id}' disappeared from the daemon state"))
                })?;
            if let Some(error) = &screen.error {
                return Err(failed(format!(
                    "screen {} failed at {:?}: {}",
                    screen.id, error.stage, error.message
                )));
            }
            match screen.state {
                ScreenState::Ready | ScreenState::Streaming => {}
                other => pending.push(format!("{}={}", screen.id, other.as_str())),
            }
        }
        if pending.is_empty() {
            println!("ready: {}", targets.join(", "));
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(failed(format!(
                "timed out after 180 s waiting for: {}",
                pending.join(", ")
            )));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn is_operating(state: ScreenState) -> bool {
    state.is_active() || state == ScreenState::Stopping
}

async fn stop(screen: Option<String>, no_wait: bool) -> CliResult<()> {
    let proxy = connect().await?;
    match &screen {
        Some(id) => proxy.stop_screen(id).await.map_err(map_zbus)?,
        None => proxy.stop_all().await.map_err(map_zbus)?,
    }
    if no_wait {
        println!("stop requested");
        return Ok(());
    }
    let state = fetch_state(&proxy).await?;
    let targets: Vec<String> = match &screen {
        Some(id) => {
            if !state.screens.iter().any(|screen| &screen.id == id) {
                return Err(failed(format!("screen '{id}' is not configured")));
            }
            vec![id.clone()]
        }
        None => state
            .screens
            .iter()
            .map(|screen| screen.id.clone())
            .collect(),
    };
    wait_for_stopped(&proxy, &targets).await
}

async fn wait_for_stopped(proxy: &DaemonProxy<'_>, targets: &[String]) -> CliResult<()> {
    if targets.is_empty() {
        println!("no configured screens to stop");
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    loop {
        let state = fetch_state(proxy).await?;
        let mut pending = Vec::new();
        for id in targets {
            let Some(screen) = state.screens.iter().find(|screen| &screen.id == id) else {
                continue;
            };
            if is_operating(screen.state) {
                pending.push(format!("{}={}", screen.id, screen.state.as_str()));
            }
        }
        if pending.is_empty() {
            println!("stopped: {}", targets.join(", "));
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(failed(format!(
                "timed out after 60 s waiting for: {}",
                pending.join(", ")
            )));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// screens
// ---------------------------------------------------------------------------

async fn screens(command: ScreensCommand) -> CliResult<()> {
    match command {
        ScreensCommand::List { json } => screens_list(json).await,
        ScreensCommand::Add { json } => screens_add(&json).await,
        ScreensCommand::Update { id, json } => screens_update(&id, &json).await,
        ScreensCommand::Remove { id, purge, yes } => screens_remove(&id, purge, yes).await,
        ScreensCommand::Bitrate { id, kbps } => {
            let proxy = connect().await?;
            proxy
                .set_screen_bitrate(&id, kbps)
                .await
                .map_err(map_zbus)?;
            println!(
                "{}",
                if kbps == 0 {
                    format!("{id}: bitrate cap removed")
                } else {
                    format!("{id}: {kbps} kbps cap")
                }
            );
            Ok(())
        }
        ScreensCommand::Position {
            id,
            position,
            relative_to,
            alignment,
        } => {
            let proxy = connect().await?;
            let state = fetch_state(&proxy).await?;
            let screen = state
                .screens
                .iter()
                .find(|screen| screen.id == id)
                .ok_or_else(|| failed(format!("screen '{id}' is not configured")))?;
            let relative = relative_to.unwrap_or_else(|| screen.relative_to.clone());
            let align = alignment.unwrap_or_else(|| alignment_name(screen.alignment));
            proxy
                .set_screen_layout(&id, &position, &relative, &align)
                .await
                .map_err(map_zbus)?;
            println!("{id}: placed {position} of {relative} ({align})");
            Ok(())
        }
        ScreensCommand::Audio { id, state } => {
            let proxy = connect().await?;
            proxy
                .set_screen_audio(&id, parse_on_off(&state)?)
                .await
                .map_err(map_zbus)?;
            println!("{id}: audio {state}");
            Ok(())
        }
        ScreensCommand::Touch { id, state } => {
            let proxy = connect().await?;
            proxy
                .set_screen_touch(&id, parse_on_off(&state)?)
                .await
                .map_err(map_zbus)?;
            println!("{id}: native touch {state}");
            Ok(())
        }
        ScreensCommand::Input { id, kind, state } => {
            let proxy = connect().await?;
            proxy
                .set_screen_input(&id, &kind, parse_on_off(&state)?)
                .await
                .map_err(map_zbus)?;
            println!("{id}: {kind} input {state}");
            Ok(())
        }
        ScreensCommand::Sink { id, sink } => {
            let proxy = connect().await?;
            let sink = if sink == "auto" { String::new() } else { sink };
            proxy
                .set_screen_audio_sink(&id, &sink)
                .await
                .map_err(map_zbus)?;
            if sink.is_empty() {
                println!("{id}: audio sink follows the host default");
            } else {
                println!("{id}: capturing {sink}");
            }
            Ok(())
        }
    }
}

async fn screens_list(json: bool) -> CliResult<()> {
    let proxy = connect().await?;
    let state = fetch_state(&proxy).await?;
    if json {
        let value = serde_json::to_value(&state.screens)
            .map_err(|err| failed(format!("cannot render JSON: {err}")))?;
        return print_json(&value);
    }
    if state.screens.is_empty() {
        println!("no screens configured");
        return Ok(());
    }
    for screen in &state.screens {
        println!(
            "{:<12} {:<24} {}x{}@{:.0} scale {:.2} slot {} {}/{}/{} {} {}/{}",
            screen.id,
            screen.name,
            screen.width,
            screen.height,
            screen.refresh_hz,
            screen.scale,
            screen.slot,
            screen.position_str(),
            screen.relative_to,
            screen.alignment_str(),
            screen.state.as_str(),
            screen.base_port,
            screen.web_port,
        );
    }
    Ok(())
}

trait ScreenDisplay {
    fn position_str(&self) -> &'static str;
    fn alignment_str(&self) -> &'static str;
}

impl ScreenDisplay for longcar_core::state::ScreenEntry {
    fn position_str(&self) -> &'static str {
        use longcar_core::model::Position;
        match self.position {
            Position::Left => "left",
            Position::Right => "right",
            Position::Above => "above",
            Position::Below => "below",
        }
    }

    fn alignment_str(&self) -> &'static str {
        use longcar_core::model::Alignment;
        match self.alignment {
            Alignment::Start => "start",
            Alignment::Center => "center",
            Alignment::End => "end",
        }
    }
}

fn parse_screen_object(raw: &str) -> CliResult<serde_json::Map<String, Value>> {
    let value: Value =
        serde_json::from_str(raw).map_err(|err| failed(format!("invalid screen JSON: {err}")))?;
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(failed("invalid screen JSON: expected an object")),
    }
}

/// Ensure the payload carries the immutable screen id before sending it.
fn update_payload(id: &str, raw: &str) -> CliResult<(serde_json::Map<String, Value>, String)> {
    let mut object = parse_screen_object(raw)?;
    match object.get("id").and_then(Value::as_str) {
        Some(existing) if existing != id => {
            return Err(failed(format!(
                "screen JSON id '{existing}' does not match '{id}'"
            )));
        }
        Some(_) => {}
        None => {
            object.insert("id".to_string(), Value::String(id.to_string()));
        }
    }
    let payload = Value::Object(object.clone()).to_string();
    Ok((object, payload))
}

async fn screens_add(json: &str) -> CliResult<()> {
    parse_screen_object(json)?;
    let proxy = connect().await?;
    let id = proxy.add_screen(json).await.map_err(map_zbus)?;
    println!("{id}");
    Ok(())
}

async fn screens_update(id: &str, json: &str) -> CliResult<()> {
    let (_, payload) = update_payload(id, json)?;
    let proxy = connect().await?;
    proxy.update_screen(&payload).await.map_err(map_zbus)?;
    println!("updated {id}");
    Ok(())
}

async fn screens_remove(id: &str, purge: bool, yes: bool) -> CliResult<()> {
    if purge && !yes {
        return Err(failed(
            "removing with --purge deletes the instance and pairings; pass --yes to confirm",
        ));
    }
    let proxy = connect().await?;
    proxy.remove_screen(id, purge).await.map_err(map_zbus)?;
    if purge {
        println!("removed {id} (instance data and pairings deleted)");
    } else {
        println!("removed {id} (pairing data kept)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// presets
// ---------------------------------------------------------------------------

async fn presets(command: PresetsCommand) -> CliResult<()> {
    match command {
        PresetsCommand::List => presets_list().await,
        PresetsCommand::Start { id, no_wait } => presets_start(&id, no_wait).await,
    }
}

async fn presets_list() -> CliResult<()> {
    let proxy = connect().await?;
    let state = fetch_state(&proxy).await?;
    if state.presets.is_empty() {
        println!("no presets configured");
        return Ok(());
    }
    for preset in &state.presets {
        let default = if preset.id == state.settings.default_preset {
            " (default)"
        } else {
            ""
        };
        println!(
            "{:<12} {}{}: {}",
            preset.id,
            preset.name,
            default,
            preset.screens.join(", ")
        );
    }
    Ok(())
}

async fn presets_start(id: &str, no_wait: bool) -> CliResult<()> {
    let proxy = connect().await?;
    proxy.apply_preset(id).await.map_err(map_zbus)?;
    if no_wait {
        println!("start requested");
        return Ok(());
    }
    let state = fetch_state(&proxy).await?;
    let targets = preset_screens(&state, id)?.to_vec();
    println!("waiting for: {}", targets.join(", "));
    wait_for_ready(&proxy, &targets).await
}

// ---------------------------------------------------------------------------
// devices
// ---------------------------------------------------------------------------

async fn devices(command: DevicesCommand) -> CliResult<()> {
    match command {
        DevicesCommand::Pending { screen } => devices_pending(&screen).await,
        DevicesCommand::List { screen } => devices_list(&screen).await,
        DevicesCommand::Pair {
            screen,
            pairing_id,
            pin,
            name,
        } => devices_pair(&screen, &pairing_id, &pin, name).await,
        DevicesCommand::Unpair { screen, uuid } => {
            devices_set(&screen, &uuid, DeviceOp::Unpair).await
        }
        DevicesCommand::Enable { screen, uuid } => {
            devices_set(&screen, &uuid, DeviceOp::Enable).await
        }
        DevicesCommand::Disable { screen, uuid } => {
            devices_set(&screen, &uuid, DeviceOp::Disable).await
        }
    }
}

enum DeviceOp {
    Unpair,
    Enable,
    Disable,
}

fn pairings_from_payload(raw: &str) -> CliResult<Vec<Value>> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|err| failed(format!("invalid pairing payload from daemon: {err}")))?;
    match value {
        Value::Array(items) => Ok(items),
        Value::Object(mut object) => match object.remove("pairings") {
            Some(Value::Array(items)) => Ok(items),
            _ => Err(failed(
                "invalid pairing payload from daemon: missing 'pairings' array",
            )),
        },
        _ => Err(failed(
            "invalid pairing payload from daemon: expected an array",
        )),
    }
}

fn json_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

async fn devices_pending(screen: &str) -> CliResult<()> {
    let proxy = connect().await?;
    let raw = proxy.list_pairings(screen).await.map_err(map_zbus)?;
    let pairings = pairings_from_payload(&raw)?;
    if pairings.is_empty() {
        println!("no pending pairing requests for {screen}");
        return Ok(());
    }
    for pairing in &pairings {
        println!(
            "{}\t{}\t{}",
            json_str(pairing, "id"),
            json_str(pairing, "name"),
            json_str(pairing, "address")
        );
    }
    Ok(())
}

async fn devices_list(screen: &str) -> CliResult<()> {
    let proxy = connect().await?;
    let state = fetch_state(&proxy).await?;
    let devices: Vec<_> = state
        .devices
        .iter()
        .filter(|device| device.screen_id == screen)
        .collect();
    if devices.is_empty() {
        println!("no paired clients for {screen}");
        return Ok(());
    }
    for device in devices {
        let status = if device.enabled {
            "enabled"
        } else {
            "disabled"
        };
        println!("{}\t{}\t{}", device.uuid, device.name, status);
    }
    Ok(())
}

fn pin_is_valid(pin: &str) -> bool {
    pin.len() == 4 && pin.bytes().all(|byte| byte.is_ascii_digit())
}

async fn devices_pair(
    screen: &str,
    pairing_id: &str,
    pin: &str,
    name: Option<String>,
) -> CliResult<()> {
    if !pin_is_valid(pin) {
        return Err(failed("PIN must be exactly 4 digits"));
    }
    let proxy = connect().await?;
    let name = match name {
        Some(name) if !name.trim().is_empty() => name,
        _ => {
            let raw = proxy.list_pairings(screen).await.map_err(map_zbus)?;
            let pairings = pairings_from_payload(&raw)?;
            let pending = pairings
                .iter()
                .find(|pairing| json_str(pairing, "id") == pairing_id)
                .ok_or_else(|| {
                    failed(format!(
                        "pairing request '{pairing_id}' not found; pass a NAME argument"
                    ))
                })?;
            let name = json_str(pending, "name").to_string();
            if name.is_empty() {
                return Err(failed("pairing request has no name; pass a NAME argument"));
            }
            name
        }
    };
    if name.len() > 128 {
        return Err(failed("client name must be 1..128 bytes"));
    }
    let result =
        tokio::time::timeout(PAIR_TIMEOUT, proxy.pair(screen, pairing_id, pin, &name)).await;
    match result {
        Err(_) => Err(failed("pairing timed out after 30 s")),
        Ok(Err(err)) => Err(map_zbus(err)),
        Ok(Ok(true)) => {
            println!("paired");
            Ok(())
        }
        Ok(Ok(false)) => Err(failed("pairing was rejected by Sunshine")),
    }
}

async fn devices_set(screen: &str, uuid: &str, operation: DeviceOp) -> CliResult<()> {
    let proxy = connect().await?;
    match operation {
        DeviceOp::Unpair => {
            proxy.unpair(screen, uuid).await.map_err(map_zbus)?;
            println!("unpaired {uuid}");
        }
        DeviceOp::Enable => {
            proxy
                .set_client_enabled(screen, uuid, true)
                .await
                .map_err(map_zbus)?;
            println!("enabled {uuid}");
        }
        DeviceOp::Disable => {
            proxy
                .set_client_enabled(screen, uuid, false)
                .await
                .map_err(map_zbus)?;
            println!("disabled {uuid}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// network
// ---------------------------------------------------------------------------

async fn network(command: NetworkCommand) -> CliResult<()> {
    match command {
        NetworkCommand::Hosts => network_hosts().await,
        NetworkCommand::Ports { screen, cidr } => network_ports(screen, cidr).await,
        NetworkCommand::Link => network_link().await,
        NetworkCommand::Speedtest { stop, status } => network_speedtest(stop, status).await,
    }
}

async fn network_link() -> CliResult<()> {
    let proxy = connect().await?;
    let state = fetch_state(&proxy).await?;
    match state.host.wifi.as_ref() {
        Some(link) => {
            println!("interface: {}", link.interface);
            println!(
                "ssid:      {}",
                if link.ssid.is_empty() {
                    "?"
                } else {
                    &link.ssid
                }
            );
            println!(
                "signal:    {}",
                link.signal_dbm
                    .map(|dbm| format!("{dbm} dBm"))
                    .unwrap_or_else(|| "?".to_string())
            );
            println!(
                "rate:      rx {} / tx {}",
                link.rx_bitrate_mbps
                    .map(|v| format!("{v:.0} Mbps"))
                    .unwrap_or_else(|| "?".to_string()),
                link.tx_bitrate_mbps
                    .map(|v| format!("{v:.0} Mbps"))
                    .unwrap_or_else(|| "?".to_string())
            );
            println!(
                "channel:   {} ({} MHz, {} MHz wide)",
                link.channel
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".to_string()),
                link.frequency_mhz
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".to_string()),
                link.width_mhz
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".to_string())
            );
            println!(
                "note: this is the host's own link; run `longcarctl network speedtest` to measure the device"
            );
        }
        None => println!("the preferred address is not wireless (no Wi-Fi link to report)"),
    }
    Ok(())
}

async fn network_speedtest(stop: bool, status: bool) -> CliResult<()> {
    let proxy = connect().await?;
    if stop {
        proxy.stop_speed_test().await.map_err(map_zbus)?;
        println!("speed test listener stopped");
        return Ok(());
    }
    if status {
        let state = fetch_state(&proxy).await?;
        let test = &state.speed_test;
        if test.running {
            println!("running: open {} on the device", test.url);
            if let Some(expires) = &test.expires_at {
                println!("expires: {expires}");
            }
        } else {
            println!("not running");
        }
        if let (Some(down), Some(up)) = (test.download_mbps, test.upload_mbps) {
            println!(
                "last result: {down:.1} Mbps down / {up:.1} Mbps up{}",
                test.measured_at
                    .as_ref()
                    .map(|at| format!(" ({at})"))
                    .unwrap_or_default()
            );
        }
        return Ok(());
    }
    let raw = proxy.start_speed_test().await.map_err(map_zbus)?;
    let test: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|err| failed(format!("invalid speed test payload: {err}")))?;
    let url = test["url"].as_str().unwrap_or_default();
    println!("open this on your tablet or phone:");
    println!("  {url}");
    println!(
        "it downloads {} MiB, uploads 16 MiB and reports the rates back",
        test["payloadBytes"].as_u64().unwrap_or(0) / (1024 * 1024)
    );
    println!("poll with `longcarctl network speedtest --status`, stop with `--stop`");
    Ok(())
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn network_hosts() -> CliResult<()> {
    let proxy = connect().await?;
    let raw = proxy.discover_hosts().await.map_err(map_zbus)?;
    let hosts: Value = serde_json::from_str(&raw)
        .map_err(|err| failed(format!("invalid discovery payload from daemon: {err}")))?;
    let hosts = hosts
        .as_array()
        .ok_or_else(|| failed("invalid discovery payload from daemon: expected an array"))?;
    if hosts.is_empty() {
        println!("no Sunshine hosts found");
        return Ok(());
    }
    for host in hosts {
        let self_marker = if host.get("isSelf").and_then(Value::as_bool) == Some(true) {
            " (this host)"
        } else {
            ""
        };
        println!(
            "{} ({}:{}){}",
            json_str(host, "name"),
            json_str(host, "host"),
            host.get("port").and_then(Value::as_u64).unwrap_or(0),
            self_marker
        );
        for address in string_array(host, "addresses") {
            println!("  {address}");
        }
    }
    Ok(())
}

fn cidr_for(address: &str) -> Option<String> {
    longcar_core::network::iface_cidr(address)
}

fn port_rows(family: &PortFamily) -> [(&'static str, &'static str, u16); 7] {
    [
        ("Moonlight/control (HTTP)", "tcp", family.base),
        ("Web UI (HTTPS)", "tcp", family.web),
        ("Control (HTTPS)", "tcp", family.https),
        ("RTSP setup", "tcp", family.rtsp),
        ("Video", "udp", family.video_udp),
        ("Control", "udp", family.control_udp),
        ("Audio", "udp", family.audio_udp),
    ]
}

fn firewall_lines(cidr: &str, family: &PortFamily) -> Vec<String> {
    longcar_core::network::firewall_commands(cidr, family)
}

async fn network_ports(screen: Option<String>, cidr: Option<String>) -> CliResult<()> {
    let mut note = String::new();
    let (label, slot, preferred) = match connect().await {
        Ok(proxy) => match fetch_state(&proxy).await {
            Ok(state) => {
                let entry = match &screen {
                    Some(id) => state
                        .screens
                        .iter()
                        .find(|entry| &entry.id == id)
                        .ok_or_else(|| failed(format!("screen '{id}' is not configured")))?,
                    None => match state.screens.first() {
                        Some(entry) => entry,
                        None => {
                            note = String::from("no screens configured; showing slot 0");
                            let label = String::from("default (slot 0)");
                            return print_ports(&label, 0, cidr.as_deref(), None, &note);
                        }
                    },
                };
                (
                    format!("{} ({})", entry.id, entry.name),
                    entry.slot,
                    preferred_address(&state).map(|address| address.address.clone()),
                )
            }
            Err(CliError::Unreachable) => {
                if screen.is_some() {
                    return Err(CliError::Unreachable);
                }
                note = String::from("daemon not running; showing slot 0");
                (String::from("default (slot 0)"), 0, None)
            }
            Err(err) => return Err(err),
        },
        Err(CliError::Unreachable) => {
            if screen.is_some() {
                return Err(CliError::Unreachable);
            }
            note = String::from("daemon not running; showing slot 0");
            (String::from("default (slot 0)"), 0, None)
        }
        Err(err) => return Err(err),
    };
    print_ports(&label, slot, cidr.as_deref(), preferred.as_deref(), &note)
}

fn print_ports(
    label: &str,
    slot: u8,
    cidr: Option<&str>,
    preferred: Option<&str>,
    note: &str,
) -> CliResult<()> {
    let family = PortFamily::for_slot(slot)
        .map_err(|err| failed(format!("cannot derive ports for slot {slot}: {err}")))?;
    let interface: Option<String> = None;
    let cidr = match cidr {
        Some(cidr) => cidr.to_string(),
        None => match preferred.and_then(cidr_for) {
            Some(cidr) => cidr,
            None => {
                if note.is_empty() {
                    // The CIDR is printed in the rules, so explain the placeholder.
                    println!(
                        "note: could not derive the local subnet; use --cidr <subnet> to print copyable rules"
                    );
                }
                String::from("<your-subnet>")
            }
        },
    };
    println!("screen: {label} — slot {slot}");
    for (name, proto, port) in port_rows(&family) {
        println!("  {name:<26} {port}/{proto}");
    }
    if let Some(interface) = interface {
        println!("interface: {interface}");
    }
    println!("ufw rules for {cidr}:");
    for rule in firewall_lines(&cidr, &family) {
        println!("  {rule}");
    }
    if !note.is_empty() {
        println!("note: {note}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

async fn doctor(json: bool, self_test: bool) -> CliResult<()> {
    let proxy = connect().await?;
    if self_test {
        let (ok, detail) = proxy.doctor_display_test().await.map_err(map_zbus)?;
        if json {
            // Keep the documented key order: ok first, then detail.
            let detail = serde_json::to_string(&detail)
                .map_err(|err| failed(format!("cannot render JSON: {err}")))?;
            println!("{{\"ok\":{ok},\"detail\":{detail}}}");
        } else {
            println!("{detail}");
        }
        return if ok { Ok(()) } else { Err(CliError::Silent(1)) };
    }
    let raw = proxy.get_doctor().await.map_err(map_zbus)?;
    if json {
        println!("{}", raw.trim_end());
        return Ok(());
    }
    print_doctor(&raw)
}

fn print_doctor(raw: &str) -> CliResult<()> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|err| failed(format!("invalid doctor payload from daemon: {err}")))?;
    let checks = value
        .get("checks")
        .and_then(Value::as_array)
        .ok_or_else(|| failed("invalid doctor payload from daemon: missing 'checks' array"))?;
    if checks.is_empty() {
        println!("no checks reported");
        return Ok(());
    }
    for check in checks {
        let status = check
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("error");
        let title = json_str(check, "title");
        let detail = json_str(check, "detail");
        let mut line = format!("[{status}] {title}: {detail}");
        if let Some(fix) = check.get("fix").and_then(Value::as_str)
            && !fix.is_empty()
        {
            let _ = write!(line, " fix: {fix}");
        }
        println!("{line}");
    }
    Ok(())
}

async fn audio(target: Option<String>, restore_host: bool) -> CliResult<()> {
    let proxy = connect().await?;
    if restore_host {
        proxy.restore_host_audio().await.map_err(map_zbus)?;
        println!("host audio restored to the physical sink");
        return Ok(());
    }
    let Some(target) = target else {
        let raw = proxy.get_audio_info().await.map_err(map_zbus)?;
        let info: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|err| failed(format!("invalid audio payload: {err}")))?;
        let default = info
            .get("defaultSink")
            .and_then(|sink| sink.get("name"))
            .and_then(|name| name.as_str())
            .unwrap_or("<none>");
        println!("host default sink: {default}");
        if info
            .get("staleDefault")
            .and_then(|stale| stale.as_bool())
            .unwrap_or(false)
        {
            println!(
                "  warning: Sunshine's virtual sink is still default; run `longcarctl audio --restore-host`"
            );
        }
        println!("sinks:");
        for sink in info["sinks"].as_array().into_iter().flatten() {
            let volume = (sink["volume"].as_f64().unwrap_or(1.0) * 100.0).round() as i64;
            println!(
                "  {} {:<4} {:<32} {:>3}%{:<7} {}",
                if sink["isDefault"].as_bool().unwrap_or(false) {
                    "*"
                } else {
                    " "
                },
                sink["id"].as_u64().unwrap_or(0),
                sink["name"].as_str().unwrap_or(""),
                volume,
                if sink["muted"].as_bool().unwrap_or(false) {
                    " MUTED"
                } else {
                    ""
                },
                sink["description"].as_str().unwrap_or("")
            );
        }
        println!("screens:");
        for screen in info["screens"].as_array().into_iter().flatten() {
            println!(
                "  {:<12} audio={:<5} sink={}",
                screen["id"].as_str().unwrap_or(""),
                screen["audio"].as_bool().unwrap_or(false),
                screen["effectiveSink"].as_str().unwrap_or("<none>")
            );
        }
        return Ok(());
    };
    let id = if matches!(target.as_str(), "off" | "none" | "local") {
        String::new()
    } else {
        target
    };
    proxy.set_audio_screen(&id).await.map_err(map_zbus)?;
    if id.is_empty() {
        println!("audio disabled on every screen (devices use their own speakers)");
    } else {
        println!("audio routed to {id}");
    }
    Ok(())
}

fn parse_on_off(value: &str) -> CliResult<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "enabled" => Ok(true),
        "off" | "false" | "no" | "disabled" => Ok(false),
        other => Err(failed(format!("expected on or off, got '{other}'"))),
    }
}

fn alignment_name(alignment: longcar_core::model::Alignment) -> String {
    match alignment {
        longcar_core::model::Alignment::Start => "start",
        longcar_core::model::Alignment::Center => "center",
        longcar_core::model::Alignment::End => "end",
    }
    .to_string()
}

async fn export_config(output: Option<String>) -> CliResult<()> {
    let proxy = connect().await?;
    let toml_text = proxy.export_config().await.map_err(map_zbus)?;
    match output {
        Some(path) => {
            std::fs::write(&path, &toml_text)
                .map_err(|err| failed(format!("cannot write {path}: {err}")))?;
            println!("wrote {path}");
        }
        None => print!("{toml_text}"),
    }
    Ok(())
}

async fn import_config(path: &str, yes: bool) -> CliResult<()> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| failed(format!("cannot read {path}: {err}")))?;
    if !yes {
        return Err(failed(
            "importing replaces every screen and preset; re-run with --yes to confirm",
        ));
    }
    let proxy = connect().await?;
    proxy.import_config(&text).await.map_err(map_zbus)?;
    println!("configuration imported from {path}");
    Ok(())
}

async fn fix(fix_id: &str) -> CliResult<()> {
    let proxy = connect().await?;
    let detail = proxy.run_doctor_fix(fix_id).await.map_err(map_fix_zbus)?;
    println!("{detail}");
    Ok(())
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

async fn logs(limit: u32, json: bool) -> CliResult<()> {
    let proxy = connect().await?;
    let raw = proxy.get_logs(limit).await.map_err(map_zbus)?;
    if json {
        println!("{}", raw.trim_end());
        return Ok(());
    }
    let value: Value = serde_json::from_str(&raw)
        .map_err(|err| failed(format!("invalid log payload from daemon: {err}")))?;
    println!("== daemon ==");
    for line in string_array(&value, "daemon") {
        println!("{line}");
    }
    if let Some(screens) = value.get("screens").and_then(Value::as_array) {
        for screen in screens {
            println!("== screen {} ==", json_str(screen, "id"));
            for line in string_array(screen, "lines") {
                println!("{line}");
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// layout
// ---------------------------------------------------------------------------

async fn restore_layout() -> CliResult<()> {
    let proxy = connect().await?;
    proxy.restore_layout().await.map_err(map_zbus)?;
    println!("layout restored");
    Ok(())
}

async fn discard_layout() -> CliResult<()> {
    let proxy = connect().await?;
    proxy.discard_layout_snapshot().await.map_err(map_zbus)?;
    println!("layout snapshot discarded");
    Ok(())
}

// ---------------------------------------------------------------------------
// setup
// ---------------------------------------------------------------------------

async fn setup(extension_only: bool, service_only: bool) -> CliResult<()> {
    if extension_only && service_only {
        return Err(failed("choose either --extension-only or --service-only"));
    }
    let should_install_extension = !service_only;
    let enable_service = !extension_only;
    if should_install_extension {
        install_extension_zip()?;
    }
    if enable_service {
        enable_user_service()?;
    }
    if should_install_extension {
        println!("Log out and back in once on Wayland for the extension to load.");
    }
    Ok(())
}

fn find_extension_zip() -> CliResult<PathBuf> {
    if let Some(value) = std::env::var_os("LONGCAR_EXTENSION_ZIP") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(failed(format!(
            "LONGCAR_EXTENSION_ZIP points to a missing file: {}",
            path.display()
        )));
    }
    let system = PathBuf::from(format!(
        "/usr/share/longcar/{EXTENSION_UUID}.shell-extension.zip"
    ));
    if system.is_file() {
        return Ok(system);
    }
    let relative = PathBuf::from(format!("build/{EXTENSION_UUID}.shell-extension.zip"));
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut current = exe.parent().map(Path::to_path_buf);
        while let Some(directory) = current {
            roots.push(directory.clone());
            current = directory.parent().map(Path::to_path_buf);
        }
    }
    for root in roots {
        let candidate = root.join(&relative);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(failed(format!(
        "extension zip not found; looked at $LONGCAR_EXTENSION_ZIP, {}, and {} in the working tree",
        system.display(),
        relative.display()
    )))
}

fn run_tool(program: &str, args: &[&str]) -> CliResult<()> {
    let output = ProcessCommand::new(program)
        .args(args)
        .output()
        .map_err(|err| failed(format!("failed to run {program}: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Err(failed(format!(
        "{program} {} failed: {detail}",
        args.join(" ")
    )))
}

fn install_extension_zip() -> CliResult<()> {
    let zip = find_extension_zip()?;
    let zip_arg = zip.to_string_lossy().into_owned();
    run_tool("gnome-extensions", &["install", "--force", &zip_arg])?;
    println!(
        "installed extension {EXTENSION_UUID} from {}",
        zip.display()
    );
    match run_tool("gnome-extensions", &["enable", EXTENSION_UUID]) {
        Ok(()) => println!("enabled extension {EXTENSION_UUID}"),
        Err(CliError::Failed(message)) => {
            // The running Shell cannot enable an extension it has not loaded yet; write
            // the GSettings list so the next login picks it up.
            println!("{message}");
            enable_extension_in_settings()?;
            println!("added {EXTENSION_UUID} to enabled-extensions; it loads on the next login");
        }
        Err(CliError::Unreachable) | Err(CliError::Silent(_)) => {
            return Err(failed(format!(
                "cannot enable {EXTENSION_UUID} in the running Shell; log out and back in once"
            )));
        }
    }
    Ok(())
}

/// Append the extension UUID to `org.gnome.shell enabled-extensions` with `gsettings`.
fn enable_extension_in_settings() -> CliResult<()> {
    let current = ProcessCommand::new("gsettings")
        .args(["get", "org.gnome.shell", "enabled-extensions"])
        .output()
        .map_err(|err| failed(format!("failed to run gsettings: {err}")))?;
    if !current.status.success() {
        return Err(failed(format!(
            "gsettings get enabled-extensions failed: {}",
            String::from_utf8_lossy(&current.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&current.stdout).trim().to_string();
    if text.contains(EXTENSION_UUID) {
        return Ok(());
    }
    let mut entries: Vec<String> = text
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|entry| {
            entry
                .trim()
                .trim_matches('\'')
                .trim_matches('"')
                .to_string()
        })
        .filter(|entry| !entry.is_empty())
        .collect();
    entries.push(EXTENSION_UUID.to_string());
    let rendered = format!(
        "[{}]",
        entries
            .iter()
            .map(|entry| format!("'{entry}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let output = ProcessCommand::new("gsettings")
        .args(["set", "org.gnome.shell", "enabled-extensions", &rendered])
        .output()
        .map_err(|err| failed(format!("failed to run gsettings: {err}")))?;
    if !output.status.success() {
        return Err(failed(format!(
            "gsettings set enabled-extensions failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn enable_user_service() -> CliResult<()> {
    // A missing user manager is not fatal: log in again and the unit loads.
    let _ = ProcessCommand::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    let unit_known = ProcessCommand::new("systemctl")
        .args(["--user", "cat", "longcar-daemon.service"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !unit_known {
        println!("longcar-daemon.service is not installed; skipped enabling the user service");
        return Ok(());
    }
    run_tool(
        "systemctl",
        &["--user", "enable", "--now", "longcar-daemon.service"],
    )?;
    println!("enabled longcar-daemon.service");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests for the pure helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_mode_selects_exactly_one_source() {
        assert_eq!(start_mode(None, None).unwrap(), StartMode::Default);
        assert_eq!(
            start_mode(Some("tablet".into()), None).unwrap(),
            StartMode::Preset("tablet".into())
        );
        assert_eq!(
            start_mode(None, Some("phone".into())).unwrap(),
            StartMode::Screen("phone".into())
        );
        let error = start_mode(Some("tablet".into()), Some("phone".into())).unwrap_err();
        assert!(
            matches!(error, CliError::Failed(message) if message == "choose either --preset or --screen")
        );
    }

    #[test]
    fn pin_requires_four_ascii_digits() {
        assert!(pin_is_valid("0000"));
        assert!(pin_is_valid("4821"));
        assert!(!pin_is_valid("123"));
        assert!(!pin_is_valid("12345"));
        assert!(!pin_is_valid("12a4"));
        assert!(!pin_is_valid("１２３４"));
        assert!(!pin_is_valid(""));
    }

    #[test]
    fn firewall_rules_cover_every_port_family_member() {
        let family = PortFamily::for_slot(0).unwrap();
        let rules = firewall_lines("192.168.2.0/24", &family);
        assert_eq!(rules.len(), 7);
        assert_eq!(
            rules[0],
            "ufw allow from 192.168.2.0/24 to any port 48049 proto tcp"
        );
        assert_eq!(
            rules[1],
            "ufw allow from 192.168.2.0/24 to any port 48050 proto tcp"
        );
        assert_eq!(
            rules[2],
            "ufw allow from 192.168.2.0/24 to any port 48044 proto tcp"
        );
        assert_eq!(
            rules[3],
            "ufw allow from 192.168.2.0/24 to any port 48070 proto tcp"
        );
        assert_eq!(
            rules[4],
            "ufw allow from 192.168.2.0/24 to any port 48058 proto udp"
        );
        assert_eq!(
            rules[6],
            "ufw allow from 192.168.2.0/24 to any port 48060 proto udp"
        );
    }

    #[test]
    fn port_rows_match_the_documented_table() {
        let family = PortFamily::for_slot(1).unwrap();
        let rows = port_rows(&family);
        assert_eq!(rows[0], ("Moonlight/control (HTTP)", "tcp", 48109));
        assert_eq!(rows[3], ("RTSP setup", "tcp", 48130));
        assert_eq!(rows[6], ("Audio", "udp", 48120));
    }

    #[test]
    fn update_payload_injects_the_route_id() {
        let (object, payload) =
            update_payload("tablet", r#"{"name":"Tablet","width":2560}"#).unwrap();
        assert_eq!(object.get("id").and_then(Value::as_str), Some("tablet"));
        let round_trip: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(round_trip["id"], "tablet");
    }

    #[test]
    fn update_payload_rejects_a_mismatched_id() {
        let error = update_payload("tablet", r#"{"id":"phone"}"#).unwrap_err();
        assert!(matches!(error, CliError::Failed(message) if message.contains("does not match")));
    }

    #[test]
    fn update_payload_rejects_non_objects() {
        assert!(update_payload("tablet", "[1, 2]").is_err());
        assert!(update_payload("tablet", "not json").is_err());
    }

    fn sample_state() -> StateDoc {
        StateDoc::from_json(
            r#"{
                "schemaVersion": 1,
                "daemon": {"state": "idle", "since": "2026-11-21T12:00:00Z"},
                "screens": [{
                    "id": "tablet", "name": "Tablet", "state": "off",
                    "width": 2560, "height": 1600, "refreshHz": 120.0, "scale": 1.5,
                    "slot": 0, "position": "right", "relativeTo": "primary",
                    "alignment": "center", "encoder": "auto", "hevc": "auto",
                    "av1": "off", "maxBitrateKbps": 30000,
                    "maxResolution": "2560x1600", "cursor": "embedded",
                    "basePort": 48049, "webPort": 48050
                }],
                "presets": [{"id": "tablet", "name": "Tablet", "screens": ["tablet"]}],
                "settings": {
                    "autostartScreens": false, "notifications": true,
                    "stopWhenIdleMinutes": 0, "defaultPreset": "tablet",
                    "audioScreen": "tablet", "restoreLayoutOnStop": true,
                    "onboardingComplete": false
                },
                "host": {"addresses": []}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn target_selection_resolves_each_start_mode() {
        let mut state = sample_state();
        assert_eq!(
            resolve_start_targets(&state, &StartMode::Default).unwrap(),
            vec!["tablet".to_string()]
        );
        assert_eq!(
            resolve_start_targets(&state, &StartMode::Preset("tablet".into())).unwrap(),
            vec!["tablet".to_string()]
        );
        assert_eq!(
            resolve_start_targets(&state, &StartMode::Screen("tablet".into())).unwrap(),
            vec!["tablet".to_string()]
        );
        assert!(resolve_start_targets(&state, &StartMode::Screen("phone".into())).is_err());
        assert!(resolve_start_targets(&state, &StartMode::Preset("missing".into())).is_err());

        state.settings.default_preset = String::new();
        assert!(resolve_start_targets(&state, &StartMode::Default).is_err());

        state.settings.default_preset = "tablet".to_string();
        state.presets[0].screens.clear();
        assert!(resolve_start_targets(&state, &StartMode::Default).is_err());
    }

    #[test]
    fn pairing_payload_accepts_both_shapes() {
        let array = pairings_from_payload(r#"[{"id":"a"}]"#).unwrap();
        assert_eq!(array.len(), 1);
        let wrapped = pairings_from_payload(r#"{"pairings":[{"id":"a"},{"id":"b"}]}"#).unwrap();
        assert_eq!(wrapped.len(), 2);
        assert!(pairings_from_payload(r#"{"nope":true}"#).is_err());
        assert!(pairings_from_payload("nonsense").is_err());
    }
}
