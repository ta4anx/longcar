// SPDX-License-Identifier: GPL-3.0-or-later

//! Orchestrator: one task owns every screen, layout change and Sunshine instance.
//!
//! D-Bus handlers send [`Command`]s and await a reply; the orchestrator serialises real
//! work through a queue so portal dialogs never overlap, and publishes progress through
//! a [`StateDoc`] snapshot plus a broadcast channel.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use longcar_core::config::{Config, GeneralConfig, ScreenConfig, create_private_dir};
use longcar_core::layout::plan_layout;
use longcar_core::logparse::LogEvent;
use longcar_core::model::{
    DaemonState, DisplayState, LayoutSnapshot, Rect, ScreenState, Stage, StageError,
};
use longcar_core::paths;
use longcar_core::ports::{PortFamily, check_ports};
use longcar_core::state::{
    DaemonStatus, DeviceEntry, HostEntry, PendingPairing, PhysicalEntry, SessionRecord, StateDoc,
};
use longcar_display::{DisplayConfigClient, DisplaySpec, MutterDisplay};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::error::DaemonError;
use crate::instance::{self, InstanceEvent, InstanceLayout, SunshineInstance};
use crate::notify::Notifier;
use crate::plan::{self, LogHub, VirtualSeed};
use crate::systemd;

/// Portal approval deadline for one screen.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Stats-only state changes are coalesced to this rate.
const STATS_PUBLISH_INTERVAL: Duration = Duration::from_millis(1000);
/// Bitrate samples are appended at most this often.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
const PAIRS_POLL: Duration = Duration::from_secs(2);
const CLIENTS_POLL: Duration = Duration::from_secs(5);
const HEALTH_POLL: Duration = Duration::from_secs(10);
const HOST_POLL: Duration = Duration::from_secs(60);

type Reply = oneshot::Sender<Result<(), DaemonError>>;
type JsonReply = oneshot::Sender<Result<String, DaemonError>>;

/// Every operation the daemon exposes to its D-Bus layer.
pub enum Command {
    StartDefault(Reply),
    StartAll(Reply),
    StopAll(Reply),
    StartScreen(String, Reply),
    StopScreen(String, Reply),
    AddScreen(String, oneshot::Sender<Result<String, DaemonError>>),
    UpdateScreen(String, Reply),
    RemoveScreen {
        id: String,
        purge: bool,
        reply: Reply,
    },
    ApplyPreset(String, Reply),
    SetPresets(String, Reply),
    AddQuickScreen(String, oneshot::Sender<Result<String, DaemonError>>),
    UpdateSettings(String, Reply),
    SetAudioScreen(String, Reply),
    ListPairings(String, JsonReply),
    Pair {
        screen: String,
        pairing_id: String,
        pin: String,
        name: String,
        reply: oneshot::Sender<Result<bool, DaemonError>>,
    },
    CancelPairing {
        screen: String,
        pairing_id: String,
        reply: Reply,
    },
    Unpair {
        screen: String,
        uuid: String,
        reply: Reply,
    },
    SetClientEnabled {
        screen: String,
        uuid: String,
        enabled: bool,
        reply: Reply,
    },
    GetPreviewNode(String, oneshot::Sender<Result<u32, DaemonError>>),
    RestoreLayout(Reply),
    DiscardLayoutSnapshot(Reply),
}

/// Read-only handle used by the D-Bus service, the GUI and the CLI.
#[derive(Clone)]
pub struct Orchestrator {
    cmd_tx: mpsc::Sender<Command>,
    state: Arc<RwLock<StateDoc>>,
    config: Arc<RwLock<Config>>,
    changed: broadcast::Sender<String>,
    logs: LogHub,
}

impl Orchestrator {
    /// Start the actor loop and return the handle.
    pub async fn spawn(config: Config) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (evt_tx, evt_rx) = mpsc::channel(1024);
        let (changed, _) = broadcast::channel(64);
        let state: Arc<RwLock<StateDoc>> = Arc::new(RwLock::new(StateDoc::default()));
        let config_shared = Arc::new(RwLock::new(config.clone()));
        let logs = LogHub::new();
        let handle = Self {
            cmd_tx,
            state: state.clone(),
            config: config_shared.clone(),
            changed: changed.clone(),
            logs: logs.clone(),
        };
        tokio::spawn(async move {
            let mut actor = Actor::new(
                config,
                state,
                config_shared,
                changed,
                logs,
                cmd_rx,
                evt_tx,
                evt_rx,
            );
            actor.run().await;
        });
        handle
    }

    /// Current state document snapshot.
    pub fn state(&self) -> StateDoc {
        self.state
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    /// Current state document as JSON.
    pub fn state_json(&self) -> String {
        self.state().to_json()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.changed.subscribe()
    }

    pub fn config(&self) -> Config {
        self.config
            .read()
            .map(|c| c.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    async fn call(&self, make: impl FnOnce(Reply) -> Command) -> Result<(), DaemonError> {
        let (tx, rx) = oneshot::channel();
        self.send_command(make(tx)).await?;
        rx.await
            .map_err(|_| DaemonError::Unavailable("daemon stopped responding".to_string()))?
    }

    async fn send_command(&self, command: Command) -> Result<(), DaemonError> {
        self.cmd_tx
            .send(command)
            .await
            .map_err(|_| DaemonError::Unavailable("orchestrator is not running".to_string()))
    }

    pub async fn start_default(&self) -> Result<(), DaemonError> {
        self.call(Command::StartDefault).await
    }

    pub async fn start_all(&self) -> Result<(), DaemonError> {
        self.call(Command::StartAll).await
    }

    pub async fn stop_all(&self) -> Result<(), DaemonError> {
        self.call(Command::StopAll).await
    }

    pub async fn start_screen(&self, id: impl Into<String>) -> Result<(), DaemonError> {
        let id = id.into();
        self.call(|reply| Command::StartScreen(id, reply)).await
    }

    pub async fn stop_screen(&self, id: impl Into<String>) -> Result<(), DaemonError> {
        let id = id.into();
        self.call(|reply| Command::StopScreen(id, reply)).await
    }

    pub async fn add_screen(&self, json: &str) -> Result<String, DaemonError> {
        let (tx, rx) = oneshot::channel();
        self.send_command(Command::AddScreen(json.to_string(), tx))
            .await?;
        rx.await
            .map_err(|_| DaemonError::Unavailable("daemon stopped responding".to_string()))?
    }

    pub async fn update_screen(&self, json: &str) -> Result<(), DaemonError> {
        let json = json.to_string();
        self.call(|reply| Command::UpdateScreen(json, reply)).await
    }

    pub async fn remove_screen(&self, id: &str, purge: bool) -> Result<(), DaemonError> {
        let id = id.to_string();
        self.call(|reply| Command::RemoveScreen { id, purge, reply })
            .await
    }

    pub async fn apply_preset(&self, id: &str) -> Result<(), DaemonError> {
        let id = id.to_string();
        self.call(|reply| Command::ApplyPreset(id, reply)).await
    }

    /// Replace the whole preset list (create, edit and delete in one call).
    pub async fn set_presets(&self, json: &str) -> Result<(), DaemonError> {
        let json = json.to_string();
        self.call(|reply| Command::SetPresets(json, reply)).await
    }

    pub async fn add_quick_screen(&self, kind: &str) -> Result<String, DaemonError> {
        let (tx, rx) = oneshot::channel();
        self.send_command(Command::AddQuickScreen(kind.to_string(), tx))
            .await?;
        rx.await
            .map_err(|_| DaemonError::Unavailable("daemon stopped responding".to_string()))?
    }

    pub async fn update_settings(&self, json: &str) -> Result<(), DaemonError> {
        let json = json.to_string();
        self.call(|reply| Command::UpdateSettings(json, reply))
            .await
    }

    pub async fn set_audio_screen(&self, id: &str) -> Result<(), DaemonError> {
        let id = id.to_string();
        self.call(|reply| Command::SetAudioScreen(id, reply)).await
    }

    pub async fn list_pairings(&self, screen: &str) -> Result<String, DaemonError> {
        let (tx, rx) = oneshot::channel();
        self.send_command(Command::ListPairings(screen.to_string(), tx))
            .await?;
        rx.await
            .map_err(|_| DaemonError::Unavailable("daemon stopped responding".to_string()))?
    }

    pub async fn pair(
        &self,
        screen: &str,
        pairing_id: &str,
        pin: &str,
        name: &str,
    ) -> Result<bool, DaemonError> {
        let (tx, rx) = oneshot::channel();
        self.send_command(Command::Pair {
            screen: screen.to_string(),
            pairing_id: pairing_id.to_string(),
            pin: pin.to_string(),
            name: name.to_string(),
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| DaemonError::Unavailable("daemon stopped responding".to_string()))?
    }

    pub async fn cancel_pairing(&self, screen: &str, pairing_id: &str) -> Result<(), DaemonError> {
        let screen = screen.to_string();
        let pairing_id = pairing_id.to_string();
        self.call(|reply| Command::CancelPairing {
            screen,
            pairing_id,
            reply,
        })
        .await
    }

    pub async fn unpair(&self, screen: &str, uuid: &str) -> Result<(), DaemonError> {
        let screen = screen.to_string();
        let uuid = uuid.to_string();
        self.call(|reply| Command::Unpair {
            screen,
            uuid,
            reply,
        })
        .await
    }

    pub async fn set_client_enabled(
        &self,
        screen: &str,
        uuid: &str,
        enabled: bool,
    ) -> Result<(), DaemonError> {
        let screen = screen.to_string();
        let uuid = uuid.to_string();
        self.call(|reply| Command::SetClientEnabled {
            screen,
            uuid,
            enabled,
            reply,
        })
        .await
    }

    pub async fn preview_node(&self, screen: &str) -> Result<u32, DaemonError> {
        let (tx, rx) = oneshot::channel();
        self.send_command(Command::GetPreviewNode(screen.to_string(), tx))
            .await?;
        rx.await
            .map_err(|_| DaemonError::Unavailable("daemon stopped responding".to_string()))?
    }

    pub async fn restore_layout(&self) -> Result<(), DaemonError> {
        self.call(Command::RestoreLayout).await
    }

    pub async fn discard_layout_snapshot(&self) -> Result<(), DaemonError> {
        self.call(Command::DiscardLayoutSnapshot).await
    }

    /// Discover Sunshine hosts on the LAN (mDNS, blocking).
    pub async fn discover_hosts(&self) -> Result<String, DaemonError> {
        let hosts = tokio::task::spawn_blocking(longcar_core::network::discover_hosts)
            .await
            .map_err(|e| DaemonError::Io(e.to_string()))?;
        Ok(serde_json::to_string(&hosts).unwrap_or_else(|_| "[]".to_string()))
    }

    /// Session history, newest first.
    pub fn history(&self, limit: u32) -> Result<String, DaemonError> {
        let limit = limit.clamp(1, 500) as usize;
        let text = std::fs::read_to_string(paths::sessions_path()).unwrap_or_default();
        let records: Vec<serde_json::Value> = text
            .lines()
            .rev()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .take(limit)
            .collect();
        Ok(serde_json::Value::Array(records).to_string())
    }

    /// Daemon log plus per-screen log tails.
    pub fn logs(&self, limit: u32) -> Result<String, DaemonError> {
        let limit = limit.clamp(1, 5000) as usize;
        let state = self.state();
        let screens: Vec<serde_json::Value> = state
            .screens
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "id": entry.id,
                    "lines": self.logs.screen_tail(&entry.id, limit),
                })
            })
            .collect();
        Ok(serde_json::json!({
            "daemon": self.logs.daemon_tail(limit),
            "screens": screens,
        })
        .to_string())
    }

    /// Doctor report.
    pub async fn doctor(&self) -> Result<String, DaemonError> {
        let snapshot_present = paths::layout_snapshot_path().exists();
        let config = self.config();
        let report = match tokio::time::timeout(
            Duration::from_secs(15),
            crate::doctor::collect(&config, snapshot_present),
        )
        .await
        {
            Ok(report) => report,
            Err(_) => {
                eprintln!("longcar-daemon: doctor: collect timed out");
                return Err(DaemonError::Unavailable(
                    "doctor checks timed out".to_string(),
                ));
            }
        };
        eprintln!(
            "longcar-daemon: doctor: collected {} checks",
            report.checks.len()
        );
        Ok(report.to_json())
    }

    /// Run a doctor fix. Orchestrator-owned fixes are handled here, everything else by
    /// the doctor module.
    pub async fn run_doctor_fix(&self, fix_id: &str) -> Result<String, DaemonError> {
        if fix_id == "restore-layout" {
            self.restore_layout().await?;
            return Ok("applied the saved layout".to_string());
        }
        if let Some(screen) = fix_id.strip_prefix("reset-instance:") {
            if !self.screen_exists(screen) {
                return Err(DaemonError::NotFound(format!("screen '{screen}'")));
            }
            instance::reset_instance(screen)
                .await
                .map_err(|e| DaemonError::Io(e.to_string()))?;
            return Ok(format!("reset the Sunshine instance for {screen}"));
        }
        if let Some(screen) = fix_id.strip_prefix("reauthorize:") {
            if !self.screen_exists(screen) {
                return Err(DaemonError::NotFound(format!("screen '{screen}'")));
            }
            InstanceLayout::new(screen)
                .remove_portal_tokens()
                .map_err(|e| DaemonError::Io(e.to_string()))?;
            let _ = self.stop_screen(screen).await;
            let _ = self.start_screen(screen).await;
            return Ok(format!(
                "cleared the portal token for {screen}; approve the picker again"
            ));
        }
        crate::doctor::run_fix(fix_id).await
    }

    /// Display self-test: create a temporary output without Sunshine.
    pub async fn doctor_display_test(&self) -> (bool, String) {
        let existing: HashSet<String> = self
            .state()
            .physical_monitors
            .iter()
            .map(|m| m.connector.clone())
            .collect();
        let mut last = "no virtual display size could be created".to_string();
        for (width, height) in [(1366u32, 768u32), (1280, 720), (1024, 768)] {
            let spec = DisplaySpec {
                width,
                height,
                refresh_hz: 60.0,
                cursor_mode: 1,
            };
            match MutterDisplay::create(spec, &existing).await {
                Ok(display) => {
                    let detail = format!(
                        "ok: {} {}x{}@60 (node {}, mode {})",
                        display.connector(),
                        width,
                        height,
                        display.node_id(),
                        display.mode_id()
                    );
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    match display.stop().await {
                        Ok(()) => return (true, detail),
                        Err(err) => {
                            return (false, format!("created {detail} but cleanup failed: {err}"));
                        }
                    }
                }
                Err(err) => last = format!("{width}x{height}: {err}"),
            }
        }
        (false, last)
    }

    fn screen_exists(&self, id: &str) -> bool {
        self.state().screens.iter().any(|s| s.id == id)
    }
}

/// A queued start or stop, executed one at a time.
#[derive(Debug, Clone)]
enum Action {
    Start(String),
    Stop(String),
}

/// Why a stop was requested, which decides how the layout is restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopCause {
    User,
    Rollback,
}

/// Runtime for one active screen.
struct ScreenRuntime {
    display: MutterDisplay,
    instance: SunshineInstance,
    connector: String,
    mode_id: String,
    logical: Rect,
    connected: bool,
    bitrate_kbps: u32,
    fps: u32,
    samples: Vec<u32>,
    encoder: Option<String>,
    size: Option<(u32, u32)>,
    portal_deadline: Option<Instant>,
    last_sample: Instant,
}

/// Result of the background start sequence.
struct StartedScreen {
    display: MutterDisplay,
    instance: SunshineInstance,
    connector: String,
    mode_id: String,
    logical: Rect,
    warnings: Vec<String>,
    snapshot_written: bool,
}

/// Events produced by background tasks.
enum ActorEvent {
    Started {
        screen: String,
        result: Box<Result<StartedScreen, StageError>>,
    },
    Progress {
        screen: String,
        state: ScreenState,
    },
    Stopped {
        screen: String,
        cause: StopCause,
        error: Option<StageError>,
        result: Result<(), String>,
    },
    /// Sunshine restarted after the portal token appeared.
    Restarted {
        screen: String,
        runtime: Box<ScreenRuntime>,
    },
    Instance {
        screen: String,
        event: InstanceEvent,
    },
    Health {
        screen: String,
        state: String,
    },
    MonitorsChanged,
    Pairs {
        screen: String,
        pairings: Vec<PendingPairing>,
    },
    Clients {
        screen: String,
        devices: Vec<DeviceEntry>,
    },
}

struct Actor {
    config: Config,
    config_shared: Arc<RwLock<Config>>,
    state: Arc<RwLock<StateDoc>>,
    changed: broadcast::Sender<String>,
    logs: LogHub,
    state_doc: StateDoc,
    cmd_rx: mpsc::Receiver<Command>,
    evt_rx: mpsc::Receiver<ActorEvent>,
    evt_tx: mpsc::Sender<ActorEvent>,
    runtimes: HashMap<String, ScreenRuntime>,
    screen_states: HashMap<String, ScreenState>,
    errors: HashMap<String, StageError>,
    queue: VecDeque<Action>,
    in_flight: Option<Action>,
    stopping_all: bool,
    autostart_at: Option<Instant>,
    display_state: Option<DisplayState>,
    snapshot_present: bool,
    layout_user_modified: bool,
    applied_physical: Vec<PhysicalEntry>,
    pending_pairings: Vec<PendingPairing>,
    devices: Vec<DeviceEntry>,
    host: HostEntry,
    host_refreshed: Instant,
    warnings: BTreeSet<String>,
    daemon: DaemonStatus,
    sessions: Vec<SessionRecord>,
    idle_deadline: Option<Instant>,
    notifier: Notifier,
    suppress_until: Option<Instant>,
    last_stats_publish: Instant,
    last_pairs_poll: Instant,
    last_clients_poll: Instant,
    last_health_poll: Instant,
    pairs_inflight: HashSet<String>,
    clients_inflight: HashSet<String>,
    last_connector_poll: Instant,
    session_started: HashMap<String, String>,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: Config,
        state: Arc<RwLock<StateDoc>>,
        config_shared: Arc<RwLock<Config>>,
        changed: broadcast::Sender<String>,
        logs: LogHub,
        cmd_rx: mpsc::Receiver<Command>,
        evt_tx: mpsc::Sender<ActorEvent>,
        evt_rx: mpsc::Receiver<ActorEvent>,
    ) -> Self {
        let snapshot_present = paths::layout_snapshot_path().exists();
        let notifier = Notifier::new(config.general.notifications);
        let now = Instant::now();
        Self {
            config,
            config_shared,
            state,
            changed,
            logs,
            state_doc: StateDoc::default(),
            cmd_rx,
            evt_rx,
            evt_tx,
            runtimes: HashMap::new(),
            screen_states: HashMap::new(),
            errors: HashMap::new(),
            queue: VecDeque::new(),
            in_flight: None,
            stopping_all: false,
            autostart_at: None,
            display_state: None,
            snapshot_present,
            layout_user_modified: false,
            applied_physical: Vec::new(),
            pending_pairings: Vec::new(),
            devices: Vec::new(),
            host: HostEntry::default(),
            host_refreshed: now,
            warnings: BTreeSet::new(),
            daemon: DaemonStatus::now(DaemonState::Idle),
            sessions: Vec::new(),
            idle_deadline: None,
            notifier,
            suppress_until: None,
            last_stats_publish: now - STATS_PUBLISH_INTERVAL,
            last_pairs_poll: now,
            last_clients_poll: now,
            last_health_poll: now,
            pairs_inflight: HashSet::new(),
            clients_inflight: HashSet::new(),
            last_connector_poll: now,
            session_started: HashMap::new(),
        }
    }

    async fn run(&mut self) {
        self.logs.daemon_line("longcar-daemon started");
        self.refresh_host();
        // Seed the physical monitor list so the GUI can draw the layout preview before
        // anything is started and layout tracking has a baseline.
        if let Ok(client) = DisplayConfigClient::new().await
            && let Ok(state) = client.current_state().await
        {
            self.applied_physical = plan::physical_entries(&state);
            self.display_state = Some(state);
        }
        if self.snapshot_present {
            self.warnings
                .insert(longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE.to_string());
        }
        if self.config.general.autostart_screens {
            self.autostart_at = Some(Instant::now() + Duration::from_secs(3));
        }
        self.spawn_monitors_watcher();
        self.publish(false);

        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                command = self.cmd_rx.recv() => {
                    match command {
                        Some(command) => self.handle_command(command).await,
                        None => break,
                    }
                }
                event = self.evt_rx.recv() => {
                    if let Some(event) = event {
                        self.handle_event(event).await;
                    }
                }
                _ = ticker.tick() => self.on_tick().await,
            }
        }
        self.logs.daemon_line("orchestrator stopped");
    }

    fn spawn_monitors_watcher(&mut self) {
        let tx = self.evt_tx.clone();
        tokio::spawn(async move {
            let Ok(client) = DisplayConfigClient::new().await else {
                return;
            };
            let Ok(mut rx) = client.watch_monitors_changed().await else {
                return;
            };
            while rx.recv().await.is_some() {
                // Debounce bursty layout signals.
                tokio::time::sleep(Duration::from_millis(300)).await;
                while rx.try_recv().is_ok() {}
                if tx.send(ActorEvent::MonitorsChanged).await.is_err() {
                    return;
                }
            }
        });
    }

    fn screen_state(&self, id: &str) -> ScreenState {
        self.screen_states
            .get(id)
            .copied()
            .unwrap_or(ScreenState::Off)
    }

    fn set_state(&mut self, id: &str, state: ScreenState) {
        self.screen_states.insert(id.to_string(), state);
    }

    fn set_error(&mut self, id: &str, error: StageError) {
        self.logs
            .screen_line(id, format!("error at {:?}: {}", error.stage, error.message));
        self.errors.insert(id.to_string(), error);
        self.set_state(id, ScreenState::Error);
    }

    fn clear_error(&mut self, id: &str) {
        self.errors.remove(id);
    }

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::StartDefault(reply) => {
                let preset = self.config.general.default_preset.clone();
                let result = if preset.is_empty() {
                    Err(DaemonError::invalid("no default preset configured"))
                } else {
                    self.apply_preset(&preset)
                };
                let _ = reply.send(result);
            }
            Command::StartAll(reply) => {
                let result = if self.config.screens.is_empty() {
                    Err(DaemonError::invalid("No screens configured"))
                } else {
                    for screen in self.config.screens.clone() {
                        self.enqueue_start(&screen.id);
                    }
                    Ok(())
                };
                let _ = reply.send(result);
            }
            Command::StopAll(reply) => {
                let result = self.stop_all();
                let _ = reply.send(result);
            }
            Command::StartScreen(id, reply) => {
                let result = self.start_screen_command(&id);
                let _ = reply.send(result);
            }
            Command::StopScreen(id, reply) => {
                let result = self.stop_screen_command(&id);
                let _ = reply.send(result);
            }
            Command::AddScreen(json, reply) => {
                let result = self.add_screen_command(&json);
                let _ = reply.send(result);
            }
            Command::UpdateScreen(json, reply) => {
                let result = self.update_screen_command(&json);
                let _ = reply.send(result);
            }
            Command::RemoveScreen { id, purge, reply } => {
                let result = self.remove_screen_command(&id, purge);
                let _ = reply.send(result);
            }
            Command::ApplyPreset(id, reply) => {
                let result = self.apply_preset(&id);
                let _ = reply.send(result);
            }
            Command::SetPresets(json, reply) => {
                let result = self.set_presets_command(&json);
                let _ = reply.send(result);
            }
            Command::AddQuickScreen(kind, reply) => {
                let result = self.add_quick_screen_command(&kind);
                let _ = reply.send(result);
            }
            Command::UpdateSettings(json, reply) => {
                let result = self.update_settings_command(&json);
                let _ = reply.send(result);
            }
            Command::SetAudioScreen(id, reply) => {
                let result = self.set_audio_screen_command(&id);
                let _ = reply.send(result);
            }
            Command::ListPairings(screen, reply) => {
                let result = self.list_pairings_command(&screen).await;
                let _ = reply.send(result);
            }
            Command::Pair {
                screen,
                pairing_id,
                pin,
                name,
                reply,
            } => {
                let result = self.pair_command(&screen, &pairing_id, &pin, &name).await;
                let _ = reply.send(result);
            }
            Command::CancelPairing {
                screen,
                pairing_id,
                reply,
            } => {
                let result = self.cancel_pairing_command(&screen, &pairing_id).await;
                let _ = reply.send(result);
            }
            Command::Unpair {
                screen,
                uuid,
                reply,
            } => {
                let result = self.unpair_command(&screen, &uuid).await;
                let _ = reply.send(result);
            }
            Command::SetClientEnabled {
                screen,
                uuid,
                enabled,
                reply,
            } => {
                let result = self
                    .set_client_enabled_command(&screen, &uuid, enabled)
                    .await;
                let _ = reply.send(result);
            }
            Command::GetPreviewNode(screen, reply) => {
                let result = self
                    .runtimes
                    .get(&screen)
                    .map(|runtime| runtime.display.node_id())
                    .ok_or_else(|| {
                        DaemonError::Unavailable(format!("screen '{screen}' is not active"))
                    });
                let _ = reply.send(result);
            }
            Command::RestoreLayout(reply) => {
                let result = self.restore_layout_command().await;
                let _ = reply.send(result);
            }
            Command::DiscardLayoutSnapshot(reply) => {
                let result = self.discard_snapshot_command();
                let _ = reply.send(result);
            }
        }
        self.process_queue().await;
        self.publish(false);
    }

    // ----- commands -------------------------------------------------------------

    fn validate_start(&self, id: &str) -> Result<ScreenConfig, DaemonError> {
        let screen = self
            .config
            .screen(id)
            .cloned()
            .ok_or_else(|| DaemonError::not_found(format!("screen '{id}'")))?;
        if self.runtimes.contains_key(id) || occupies_screen(self.screen_state(id)) {
            return Err(DaemonError::conflict(format!(
                "screen '{id}' is already active"
            )));
        }
        self.config
            .validate()
            .map_err(|e| DaemonError::invalid(e.to_string()))?;
        for other_id in self.runtimes.keys() {
            if let Some(other) = self.config.screen(other_id)
                && other.width == screen.width
                && other.height == screen.height
            {
                return Err(DaemonError::conflict(format!(
                    "resolution {}x{} is already used by active screen '{other_id}'",
                    screen.width, screen.height
                )));
            }
        }
        let family =
            PortFamily::for_slot(screen.slot).map_err(|e| DaemonError::invalid(e.to_string()))?;
        check_ports(&family).map_err(|e| DaemonError::conflict(e.to_string()))?;
        Ok(screen)
    }

    fn enqueue_start(&mut self, id: &str) {
        if self.screen_state(id).is_active()
            || self
                .queue
                .iter()
                .any(|action| matches!(action, Action::Start(queued) if queued == id))
        {
            return;
        }
        self.queue.push_back(Action::Start(id.to_string()));
    }

    fn start_screen_command(&mut self, id: &str) -> Result<(), DaemonError> {
        self.validate_start(id)?;
        if self
            .queue
            .iter()
            .any(|action| matches!(action, Action::Start(queued) if queued == id))
        {
            return Ok(());
        }
        self.set_state(id, ScreenState::Preparing);
        self.clear_error(id);
        self.queue.push_back(Action::Start(id.to_string()));
        Ok(())
    }

    fn stop_screen_command(&mut self, id: &str) -> Result<(), DaemonError> {
        if self.config.screen(id).is_none() && !self.runtimes.contains_key(id) {
            return Err(DaemonError::not_found(format!("screen '{id}'")));
        }
        if !self.runtimes.contains_key(id) && !self.screen_state(id).is_active() {
            self.set_state(id, ScreenState::Off);
            self.clear_error(id);
            self.publish(false);
            return Ok(());
        }
        self.set_state(id, ScreenState::Stopping);
        self.queue.push_back(Action::Stop(id.to_string()));
        Ok(())
    }

    fn stop_all(&mut self) -> Result<(), DaemonError> {
        let ids: Vec<String> = self.runtimes.keys().cloned().collect();
        if ids.is_empty() {
            self.stopping_all = false;
            if self.queue.is_empty() {
                return Ok(());
            }
        }
        self.stopping_all = true;
        self.queue
            .retain(|action| !matches!(action, Action::Start(_)));
        for id in ids {
            self.set_state(&id, ScreenState::Stopping);
            self.queue.push_back(Action::Stop(id));
        }
        Ok(())
    }

    fn apply_preset(&mut self, preset_id: &str) -> Result<(), DaemonError> {
        let preset = self
            .config
            .preset(preset_id)
            .cloned()
            .ok_or_else(|| DaemonError::not_found(format!("preset '{preset_id}'")))?;
        let active: Vec<String> = self.runtimes.keys().cloned().collect();
        for id in active {
            if !preset.screens.contains(&id) {
                self.set_state(&id, ScreenState::Stopping);
                self.queue.push_back(Action::Stop(id));
            }
        }
        for id in &preset.screens {
            if self.config.screen(id).is_none() {
                continue;
            }
            if !self.runtimes.contains_key(id) {
                match self.validate_start(id) {
                    Ok(_) => self.enqueue_start(id),
                    Err(err) => self.set_error(id, stage_error_for(err)),
                }
            }
        }
        Ok(())
    }

    fn set_presets_command(&mut self, json: &str) -> Result<(), DaemonError> {
        let presets: Vec<longcar_core::config::Preset> = serde_json::from_str(json)
            .map_err(|e| DaemonError::invalid(format!("presets are not valid: {e}")))?;
        let backup = self.config.clone();
        self.config.presets = presets;
        if let Err(err) = self.config.validate() {
            self.config = backup;
            return Err(DaemonError::invalid(err.to_string()));
        }
        if let Err(err) = self.save_config() {
            self.config = backup;
            return Err(err);
        }
        self.publish(false);
        Ok(())
    }

    fn add_screen_command(&mut self, json: &str) -> Result<String, DaemonError> {
        let mut value: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| DaemonError::invalid(format!("screen is not valid JSON: {e}")))?;
        if value.get("id").is_none() {
            value["id"] = serde_json::Value::String(String::new());
        }
        let mut screen: ScreenConfig = serde_json::from_value(value)
            .map_err(|e| DaemonError::invalid(format!("screen is not valid: {e}")))?;
        if screen.id.trim().is_empty() {
            screen.id = plan::unique_id(&self.config, &screen.name);
        }
        if self.config.screen(&screen.id).is_some() {
            return Err(DaemonError::conflict(format!(
                "screen '{}' already exists",
                screen.id
            )));
        }
        let backup = self.config.clone();
        let first = self.config.screens.is_empty();
        let id = screen.id.clone();
        self.config.screens.push(screen.clone());
        if first {
            let preset_id = id.clone();
            self.config.presets.push(longcar_core::config::Preset {
                id: preset_id.clone(),
                name: screen.name.clone(),
                screens: vec![id.clone()],
            });
            if self.config.general.default_preset.trim().is_empty() {
                self.config.general.default_preset = preset_id;
            }
            if self.config.general.audio_screen.trim().is_empty() {
                self.config.general.audio_screen = id.clone();
            }
        }
        if let Err(err) = self.config.validate() {
            self.config = backup;
            return Err(DaemonError::invalid(err.to_string()));
        }
        if let Err(err) = self.save_config() {
            self.config = backup;
            return Err(err);
        }
        self.set_state(&id, ScreenState::Off);
        self.publish(false);
        Ok(id)
    }

    fn update_screen_command(&mut self, json: &str) -> Result<(), DaemonError> {
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| DaemonError::invalid(format!("screen is not valid JSON: {e}")))?;
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DaemonError::invalid("screen payload needs an id"))?
            .to_string();
        let old = self
            .config
            .screen(&id)
            .cloned()
            .ok_or_else(|| DaemonError::not_found(format!("screen '{id}'")))?;
        let new: ScreenConfig = serde_json::from_value(value)
            .map_err(|e| DaemonError::invalid(format!("screen is not valid: {e}")))?;
        if new.id != id {
            return Err(DaemonError::invalid("a screen id is immutable"));
        }
        let diff = plan::diff_screens(&old, &new);
        if diff.is_empty() {
            return Ok(());
        }
        let state = self.screen_state(&id);
        let active = state.is_active();
        if active && state != ScreenState::Ready && diff.needs_restart() {
            return Err(DaemonError::conflict(format!(
                "screen '{id}' is {state}; stop it before changing geometry or stream settings",
                state = state.as_str()
            )));
        }
        if active && state == ScreenState::Ready && diff.geometry {
            // Allowed: geometry changes restart the screen (token is re-created).
            self.logs.screen_line(
                &id,
                "geometry change while ready: restarting, portal approval required",
            );
        }
        if diff.slot && state != ScreenState::Off && state != ScreenState::Error {
            return Err(DaemonError::conflict(format!(
                "screen '{id}' must be off before changing its port slot"
            )));
        }
        let backup = self.config.clone();
        if let Some(entry) = self.config.screen_mut(&id) {
            *entry = new.clone();
        }
        if let Err(err) = self.config.validate() {
            self.config = backup;
            return Err(DaemonError::invalid(err.to_string()));
        }
        if let Err(err) = self.save_config() {
            self.config = backup;
            return Err(err);
        }

        if self.runtimes.contains_key(&id) && diff.needs_restart() {
            if diff.geometry {
                let layout = InstanceLayout::new(&id);
                let _ = layout.remove_portal_tokens();
            }
            self.set_state(&id, ScreenState::Stopping);
            self.queue.push_back(Action::Stop(id.clone()));
            self.queue.push_back(Action::Start(id.clone()));
        } else if self.runtimes.contains_key(&id) && diff.layout {
            self.spawn_replan();
        }
        self.publish(false);
        Ok(())
    }

    fn remove_screen_command(&mut self, id: &str, purge: bool) -> Result<(), DaemonError> {
        if self.config.screen(id).is_none() {
            return Err(DaemonError::not_found(format!("screen '{id}'")));
        }
        let backup = self.config.clone();
        self.config.screens.retain(|screen| screen.id != id);
        for preset in &mut self.config.presets {
            preset.screens.retain(|screen| screen != id);
        }
        self.config
            .presets
            .retain(|preset| !preset.screens.is_empty());
        if self.config.general.audio_screen == id {
            self.config.general.audio_screen = self
                .config
                .screens
                .first()
                .map(|screen| screen.id.clone())
                .unwrap_or_default();
        }
        if !self
            .config
            .presets
            .iter()
            .any(|preset| preset.id == self.config.general.default_preset)
        {
            self.config.general.default_preset = self
                .config
                .presets
                .first()
                .map(|preset| preset.id.clone())
                .unwrap_or_default();
        }
        if let Err(err) = self.config.validate() {
            self.config = backup;
            return Err(DaemonError::conflict(format!(
                "cannot remove screen '{id}': {err}"
            )));
        }
        if let Err(err) = self.save_config() {
            self.config = backup;
            return Err(err);
        }
        if self.runtimes.contains_key(id) {
            self.set_state(id, ScreenState::Stopping);
            self.queue.push_back(Action::Stop(id.to_string()));
        }
        if purge {
            let dir = paths::instance_dir(id);
            let active = self.runtimes.contains_key(id);
            tokio::spawn(async move {
                if active {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                let _ = std::fs::remove_dir_all(&dir);
            });
        }
        self.screen_states.remove(id);
        self.errors.remove(id);
        self.publish(false);
        Ok(())
    }

    fn add_quick_screen_command(&mut self, kind: &str) -> Result<String, DaemonError> {
        let (screen, preset, reuse_id) = match kind {
            "phone_portrait" => {
                if let Some(existing) = self.config.screen("phone").cloned() {
                    let has_tablet = self.config.screen("tablet").is_some();
                    let mut screens = Vec::new();
                    if has_tablet {
                        screens.push("tablet".to_string());
                    }
                    screens.push("phone".to_string());
                    let preset = longcar_core::config::Preset {
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
                    (existing, preset, true)
                } else {
                    let (screen, preset) = plan::quick_phone_screen(&self.config);
                    (screen, preset, false)
                }
            }
            "tablet_landscape" => {
                if let Some(existing) = self.config.screen("tablet").cloned() {
                    let mut screens = self
                        .config
                        .preset("tablet")
                        .map(|preset| preset.screens.clone())
                        .unwrap_or_default();
                    if !screens.contains(&"tablet".to_string()) {
                        screens.push("tablet".to_string());
                    }
                    let preset = longcar_core::config::Preset {
                        id: "tablet".to_string(),
                        name: "Tablet".to_string(),
                        screens,
                    };
                    (existing, preset, true)
                } else {
                    let (screen, preset) = plan::quick_tablet_screen(&self.config);
                    (screen, preset, false)
                }
            }
            other => {
                return Err(DaemonError::invalid(format!(
                    "unknown quick screen kind '{other}'"
                )));
            }
        };

        let id = screen.id.clone();
        let backup = self.config.clone();
        if !reuse_id {
            self.config.screens.push(screen.clone());
            self.set_state(&id, ScreenState::Off);
        }
        // Ensure the preset exists and contains the screen.
        match self
            .config
            .presets
            .iter_mut()
            .find(|entry| entry.id == preset.id)
        {
            Some(entry) => {
                for screen_id in &preset.screens {
                    if !entry.screens.contains(screen_id) {
                        entry.screens.push(screen_id.clone());
                    }
                }
            }
            None => self.config.presets.push(preset.clone()),
        }
        let preset_id = preset.id.clone();
        if kind == "phone_portrait"
            && (self.config.general.default_preset == "tablet"
                || self.config.general.default_preset.trim().is_empty())
        {
            self.config.general.default_preset = preset_id.clone();
        }
        if kind == "tablet_landscape" && self.config.general.audio_screen.trim().is_empty() {
            self.config.general.audio_screen = id.clone();
        }
        if let Err(err) = self.config.validate() {
            self.config = backup;
            return Err(DaemonError::invalid(err.to_string()));
        }
        if let Err(err) = self.save_config() {
            self.config = backup;
            return Err(err);
        }
        if !self.runtimes.contains_key(&id) {
            match self.validate_start(&id) {
                Ok(_) => {
                    self.set_state(&id, ScreenState::Preparing);
                    self.enqueue_start(&id);
                }
                Err(err) => {
                    self.publish(false);
                    return Err(err);
                }
            }
        }
        self.publish(false);
        Ok(id)
    }

    fn update_settings_command(&mut self, json: &str) -> Result<(), DaemonError> {
        let backup = self.config.clone();
        let mut general = self.config.general.clone();
        plan::apply_settings_json(&mut general, json)?;
        let audio_changed = general.audio_screen != self.config.general.audio_screen;
        if audio_changed
            && self
                .screen_states
                .values()
                .any(|state| *state == ScreenState::Streaming)
        {
            return Err(DaemonError::conflict(
                "stop streaming before changing the audio screen",
            ));
        }
        self.config.general = general;
        if let Err(err) = self.config.validate() {
            self.config = backup;
            return Err(DaemonError::invalid(err.to_string()));
        }
        if let Err(err) = self.save_config() {
            self.config = backup;
            return Err(err);
        }
        self.notifier.set_enabled(self.config.general.notifications);
        if audio_changed {
            self.restart_ready_screens();
        }
        self.idle_deadline = None;
        self.publish(false);
        Ok(())
    }

    fn set_audio_screen_command(&mut self, id: &str) -> Result<(), DaemonError> {
        if !id.trim().is_empty() && self.config.screen(id).is_none() {
            return Err(DaemonError::not_found(format!("screen '{id}'")));
        }
        if self
            .screen_states
            .values()
            .any(|state| *state == ScreenState::Streaming)
        {
            return Err(DaemonError::conflict(
                "stop streaming before changing the audio screen",
            ));
        }
        self.config.general.audio_screen = id.to_string();
        self.save_config()?;
        self.restart_ready_screens();
        self.publish(false);
        Ok(())
    }

    /// Restart units whose config depends on `general` (audio assignment).
    fn restart_ready_screens(&mut self) {
        let ready: Vec<String> = self
            .runtimes
            .iter()
            .filter(|(id, _)| {
                matches!(
                    self.screen_state(id),
                    ScreenState::Ready | ScreenState::Streaming
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ready {
            self.set_state(&id, ScreenState::Stopping);
            self.queue.push_back(Action::Stop(id.clone()));
            self.queue.push_back(Action::Start(id));
        }
    }

    fn runtime_mut(&mut self, screen: &str) -> Result<&mut ScreenRuntime, DaemonError> {
        self.runtimes
            .get_mut(screen)
            .ok_or_else(|| DaemonError::Unavailable(format!("screen '{screen}' is not active")))
    }

    async fn list_pairings_command(&mut self, screen: &str) -> Result<String, DaemonError> {
        if self.config.screen(screen).is_none() {
            return Err(DaemonError::not_found(format!("screen '{screen}'")));
        }
        let state = self.screen_state(screen);
        if !matches!(state, ScreenState::Ready | ScreenState::Streaming) {
            return Err(DaemonError::Unavailable(format!(
                "screen '{screen}' is not ready"
            )));
        }
        let runtime = self.runtime_mut(screen)?;
        let requests = runtime
            .instance
            .api()
            .pin_pairings()
            .await
            .map_err(|e| DaemonError::Sunshine(format!("cannot list pairings: {e}")))?;
        let pending = plan::pending_for_screen(screen, requests);
        self.pending_pairings
            .retain(|entry| entry.screen_id != screen);
        self.pending_pairings.extend(pending.clone());
        self.publish(false);
        Ok(serde_json::to_string(&pending).unwrap_or_else(|_| "[]".to_string()))
    }

    async fn pair_command(
        &mut self,
        screen: &str,
        pairing_id: &str,
        pin: &str,
        name: &str,
    ) -> Result<bool, DaemonError> {
        if pin.len() != 4 || !pin.chars().all(|c| c.is_ascii_digit()) {
            return Err(DaemonError::invalid("the PIN is exactly 4 digits"));
        }
        if name.trim().is_empty() || name.len() > 128 {
            return Err(DaemonError::invalid("the client name is 1..128 bytes"));
        }
        let runtime = self.runtime_mut(screen)?;
        let api = runtime.instance.api();
        let paired = api
            .submit_pin(pairing_id, pin, name)
            .await
            .map_err(|e| DaemonError::Sunshine(format!("pairing failed: {e}")))?;
        if !paired {
            return Err(DaemonError::Sunshine(
                "Sunshine rejected the PIN".to_string(),
            ));
        }
        self.pending_pairings
            .retain(|entry| !(entry.screen_id == screen && entry.id == pairing_id));
        if let Ok(clients) = api.clients().await {
            let devices = plan::devices_for_screen(screen, clients);
            self.devices.retain(|entry| entry.screen_id != screen);
            self.devices.extend(devices);
        }
        self.publish(false);
        Ok(true)
    }

    async fn cancel_pairing_command(
        &mut self,
        screen: &str,
        pairing_id: &str,
    ) -> Result<(), DaemonError> {
        let runtime = self.runtime_mut(screen)?;
        runtime
            .instance
            .api()
            .cancel_pin(pairing_id)
            .await
            .map_err(|e| DaemonError::Sunshine(format!("cannot cancel pairing: {e}")))?;
        self.pending_pairings
            .retain(|entry| !(entry.screen_id == screen && entry.id == pairing_id));
        self.publish(false);
        Ok(())
    }

    async fn unpair_command(&mut self, screen: &str, uuid: &str) -> Result<(), DaemonError> {
        let runtime = self.runtime_mut(screen)?;
        runtime
            .instance
            .api()
            .unpair(uuid)
            .await
            .map_err(|e| DaemonError::Sunshine(format!("cannot unpair: {e}")))?;
        self.devices
            .retain(|entry| !(entry.screen_id == screen && entry.uuid == uuid));
        self.publish(false);
        Ok(())
    }

    async fn set_client_enabled_command(
        &mut self,
        screen: &str,
        uuid: &str,
        enabled: bool,
    ) -> Result<(), DaemonError> {
        let runtime = self.runtime_mut(screen)?;
        runtime
            .instance
            .api()
            .set_client_enabled(uuid, enabled)
            .await
            .map_err(|e| DaemonError::Sunshine(format!("cannot update client: {e}")))?;
        for entry in &mut self.devices {
            if entry.screen_id == screen && entry.uuid == uuid {
                entry.enabled = enabled;
            }
        }
        self.publish(false);
        Ok(())
    }

    async fn restore_layout_command(&mut self) -> Result<(), DaemonError> {
        if !self.runtimes.is_empty() {
            return Err(DaemonError::conflict(
                "Stop screens before restoring layout",
            ));
        }
        let path = paths::layout_snapshot_path();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| DaemonError::not_found(format!("no layout snapshot: {e}")))?;
        let snapshot: LayoutSnapshot = serde_json::from_str(&text)
            .map_err(|e| DaemonError::Io(format!("layout snapshot is invalid: {e}")))?;
        let client = DisplayConfigClient::new()
            .await
            .map_err(|e| DaemonError::Mutter(e.to_string()))?;
        let warnings = client
            .restore(&snapshot)
            .await
            .map_err(|e| DaemonError::Mutter(e.to_string()))?;
        for warning in warnings {
            self.logs.daemon_line(format!("restore warning: {warning}"));
        }
        let _ = std::fs::remove_file(&path);
        self.snapshot_present = false;
        self.layout_user_modified = false;
        self.warnings
            .remove(longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE);
        if let Ok(state) = client.current_state().await {
            self.applied_physical = plan::physical_entries(&state);
            self.display_state = Some(state);
        }
        self.publish(false);
        Ok(())
    }

    fn discard_snapshot_command(&mut self) -> Result<(), DaemonError> {
        let path = paths::layout_snapshot_path();
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| DaemonError::Io(e.to_string()))?;
        }
        self.snapshot_present = false;
        self.layout_user_modified = false;
        self.warnings
            .remove(longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE);
        self.publish(false);
        Ok(())
    }

    fn save_config(&mut self) -> Result<(), DaemonError> {
        let path = paths::config_path();
        self.config
            .save(&path)
            .map_err(|e| DaemonError::Io(format!("cannot save configuration: {e}")))?;
        if let Ok(mut shared) = self.config_shared.write() {
            *shared = self.config.clone();
        }
        Ok(())
    }

    async fn process_queue(&mut self) {
        while self.in_flight.is_none() {
            let Some(action) = self.queue.pop_front() else {
                break;
            };
            match action.clone() {
                Action::Start(id) => {
                    if self.runtimes.contains_key(&id) || occupies_screen(self.screen_state(&id)) {
                        continue;
                    }
                    if let Err(err) = longcar_core::sunshine::detect_sunshine_binary() {
                        self.warnings
                            .insert(longcar_core::model::warnings::SUNSHINE_MISSING.to_string());
                        self.set_error(&id, StageError::new(Stage::SunshineSpawn, err.to_string()));
                        self.publish(false);
                        continue;
                    }
                    if let Err(err) = self.validate_start(&id) {
                        self.set_error(&id, stage_error_for(err));
                        self.publish(false);
                        continue;
                    }
                    self.set_state(&id, ScreenState::Preparing);
                    self.clear_error(&id);
                    self.spawn_start(&id);
                    self.in_flight = Some(action);
                }
                Action::Stop(id) => {
                    if self.spawn_stop(&id, StopCause::User) {
                        self.in_flight = Some(action);
                    }
                }
            }
        }
        if self.in_flight.is_none() && self.queue.is_empty() {
            self.stopping_all = false;
        }
    }

    /// Spawn the start sequence; returns false when the screen has no runtime to stop.
    fn spawn_stop(&mut self, id: &str, cause: StopCause) -> bool {
        let Some(runtime) = self.runtimes.remove(id) else {
            self.set_state(id, ScreenState::Off);
            return false;
        };
        let error = self.errors.get(id).cloned();
        let tx = self.evt_tx.clone();
        let screen = id.to_string();
        self.set_state(id, ScreenState::Stopping);
        tokio::spawn(async move {
            runtime.instance.stop().await;
            let result = runtime.display.stop().await.map_err(|e| e.to_string());
            let _ = tx
                .send(ActorEvent::Stopped {
                    screen,
                    cause,
                    error,
                    result,
                })
                .await;
        });
        true
    }

    fn spawn_replan(&mut self) {
        let mut seeds = Vec::new();
        for (id, runtime) in &self.runtimes {
            if let Some(screen) = self.config.screen(id) {
                seeds.push(VirtualSeed::from_screen(
                    screen,
                    &runtime.connector,
                    &runtime.mode_id,
                ));
            }
        }
        if seeds.is_empty() {
            return;
        }
        let tx = self.evt_tx.clone();
        let logs = self.logs.clone();
        self.suppress_until = Some(Instant::now() + Duration::from_secs(2));
        tokio::spawn(async move {
            if let Err(err) = replan_task(seeds, &logs).await {
                logs.daemon_line(format!("layout replan failed: {err}"));
            }
            let _ = tx.send(ActorEvent::MonitorsChanged).await;
        });
    }

    /// Restart a screen's Sunshine unit so it picks up a freshly saved portal token.
    fn spawn_token_restart(&mut self, screen: &str) {
        let Some(runtime) = self.runtimes.remove(screen) else {
            return;
        };
        let Some(config) = self.config.screen(screen).cloned() else {
            self.runtimes.insert(screen.to_string(), runtime);
            return;
        };
        let Ok(family) = PortFamily::for_slot(config.slot) else {
            self.runtimes.insert(screen.to_string(), runtime);
            return;
        };
        let general = self.config.general.clone();
        self.set_state(screen, ScreenState::Starting);
        self.suppress_until = Some(Instant::now() + Duration::from_secs(2));
        let tx = self.evt_tx.clone();
        let id = screen.to_string();
        tokio::spawn(async move {
            let ScreenRuntime {
                display,
                instance,
                connector,
                mode_id,
                logical,
                ..
            } = runtime;
            instance.stop().await;
            let (instance_tx, mut instance_rx) = mpsc::channel(512);
            let forward = tx.clone();
            let forward_screen = id.clone();
            tokio::spawn(async move {
                while let Some(event) = instance_rx.recv().await {
                    if forward
                        .send(ActorEvent::Instance {
                            screen: forward_screen.clone(),
                            event,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
            match SunshineInstance::start(&config, &general, &family, instance_tx).await {
                Ok(mut instance) => {
                    instance.spawn_log_tail();
                    let _ = tx
                        .send(ActorEvent::Restarted {
                            screen: id,
                            runtime: Box::new(ScreenRuntime {
                                display,
                                instance,
                                connector,
                                mode_id,
                                logical,
                                connected: false,
                                bitrate_kbps: 0,
                                fps: 0,
                                samples: Vec::new(),
                                encoder: None,
                                size: None,
                                portal_deadline: None,
                                last_sample: Instant::now(),
                            }),
                        })
                        .await;
                }
                Err(err) => {
                    let stage = match err {
                        crate::instance::InstanceError::Stage(stage) => stage,
                        crate::instance::InstanceError::Io(io) => {
                            StageError::new(Stage::Io, io.to_string())
                        }
                    };
                    let _ = display.stop().await;
                    let _ = tx
                        .send(ActorEvent::Stopped {
                            screen: id,
                            cause: StopCause::Rollback,
                            error: Some(stage),
                            result: Ok(()),
                        })
                        .await;
                }
            }
        });
    }

    /// Spawn the full start sequence for one screen.
    fn spawn_start(&mut self, id: &str) {
        let Some(screen) = self.config.screen(id).cloned() else {
            self.set_state(id, ScreenState::Off);
            return;
        };
        let general = self.config.general.clone();
        let seeds: Vec<VirtualSeed> = self
            .runtimes
            .iter()
            .filter(|(key, _)| key.as_str() != id)
            .filter_map(|(key, runtime)| {
                self.config.screen(key).map(|other| {
                    VirtualSeed::from_screen(other, &runtime.connector, &runtime.mode_id)
                })
            })
            .collect();
        let need_snapshot = !self.snapshot_present;
        self.suppress_until = Some(Instant::now() + Duration::from_secs(5));
        let tx = self.evt_tx.clone();
        let logs = self.logs.clone();
        let screen_id = id.to_string();
        tokio::spawn(async move {
            let result = start_task(&screen, &general, seeds, need_snapshot, &logs, &tx).await;
            let _ = tx
                .send(ActorEvent::Started {
                    screen: screen_id,
                    result: Box::new(result),
                })
                .await;
        });
    }

    async fn handle_event(&mut self, event: ActorEvent) {
        match event {
            ActorEvent::Started { screen, result } => {
                self.in_flight = None;
                match *result {
                    Ok(started) => {
                        if started.snapshot_written {
                            self.snapshot_present = true;
                        }
                        for warning in started.warnings {
                            self.warnings.insert(warning);
                        }
                        let runtime = ScreenRuntime {
                            connector: started.connector.clone(),
                            mode_id: started.mode_id.clone(),
                            logical: started.logical,
                            connected: false,
                            bitrate_kbps: 0,
                            fps: 0,
                            samples: Vec::new(),
                            encoder: None,
                            size: None,
                            portal_deadline: None,
                            last_sample: Instant::now(),
                            display: started.display,
                            instance: started.instance,
                        };
                        let has_token = runtime.instance.portal_token_present();
                        let node = runtime.display.node_id();
                        self.runtimes.insert(screen.clone(), runtime);
                        self.clear_error(&screen);
                        self.warnings
                            .remove(longcar_core::model::warnings::SUNSHINE_MISSING);
                        self.set_state(&screen, ScreenState::Starting);
                        if has_token {
                            self.set_state(&screen, ScreenState::Ready);
                        } else {
                            self.set_state(&screen, ScreenState::WaitingPortal);
                            self.arm_portal_deadline(&screen);
                            self.notify_portal(&screen);
                        }
                        self.logs.screen_line(
                            &screen,
                            format!(
                                "display {} (node {node}) ready, portal token present: {has_token}",
                                started.connector
                            ),
                        );
                        if self.idle_deadline.is_none() && !self.any_streaming() {
                            self.arm_idle_deadline();
                        }
                        // Record where the physical monitors ended up after this apply so
                        // the layout tracker does not mistake our own change for a user
                        // edit.
                        if let Ok(client) = DisplayConfigClient::new().await
                            && let Ok(state) = client.current_state().await
                        {
                            self.applied_physical = plan::physical_entries(&state);
                            self.display_state = Some(state);
                        }
                        self.suppress_until = Some(Instant::now() + Duration::from_secs(2));
                        if self.stopping_all {
                            // A StartAll/StartScreen that was in flight when StopAll
                            // arrived must not leave the screen running.
                            self.set_state(&screen, ScreenState::Stopping);
                            self.queue.push_back(Action::Stop(screen.clone()));
                        }
                    }
                    Err(error) => {
                        self.set_error(&screen, error);
                        if self.runtimes.is_empty() {
                            self.after_last_stop().await;
                        }
                    }
                }
                self.publish(false);
                self.process_queue().await;
            }
            ActorEvent::Restarted { screen, runtime } => {
                if self.config.screen(&screen).is_none()
                    || matches!(
                        self.screen_state(&screen),
                        ScreenState::Stopping | ScreenState::Off
                    )
                {
                    // The user stopped or removed the screen while we restarted Sunshine.
                    let runtime = *runtime;
                    tokio::spawn(async move {
                        runtime.instance.stop().await;
                        let _ = runtime.display.stop().await;
                    });
                    self.publish(false);
                    return;
                }
                self.runtimes.insert(screen.clone(), *runtime);
                self.set_state(&screen, ScreenState::Ready);
                self.clear_error(&screen);
                self.warnings
                    .remove(longcar_core::model::warnings::PORTAL_TIMEOUT);
                self.warnings
                    .remove(longcar_core::model::warnings::SESSION_CLOSED);
                self.logs
                    .screen_line(&screen, "Sunshine restarted with the saved portal token");
                self.publish(false);
            }
            ActorEvent::Progress { screen, state } => {
                if !matches!(self.screen_state(&screen), ScreenState::Error) {
                    self.set_state(&screen, state);
                    self.publish(false);
                }
            }
            ActorEvent::Stopped {
                screen,
                cause,
                error,
                result,
            } => {
                self.in_flight = None;
                if let Err(message) = result {
                    self.logs
                        .screen_line(&screen, format!("stop reported: {message}"));
                }
                match error {
                    Some(error) => {
                        self.set_error(&screen, error);
                    }
                    None => {
                        if cause == StopCause::User {
                            self.clear_error(&screen);
                            self.set_state(&screen, ScreenState::Off);
                        }
                    }
                }
                if self.runtimes.is_empty() {
                    self.after_last_stop().await;
                }
                self.publish(false);
                self.process_queue().await;
            }
            ActorEvent::Instance { screen, event } => {
                self.handle_instance_event(&screen, event).await;
            }
            ActorEvent::Health { screen, state } => {
                if (state.contains("failed") || state.contains("inactive"))
                    && self.runtimes.contains_key(&screen)
                    && self.screen_state(&screen) != ScreenState::Stopping
                {
                    let error = StageError::new(
                        Stage::SunshineSpawn,
                        format!("Sunshine unit for '{screen}' is {state}"),
                    );
                    self.set_error(&screen, error);
                    self.notify(
                        "sunshine-stopped",
                        "Sunshine stopped",
                        &format!("The stream for '{screen}' stopped unexpectedly"),
                    );
                    if self.spawn_stop(&screen, StopCause::Rollback) {
                        self.in_flight = Some(Action::Stop(screen.clone()));
                    }
                    self.publish(false);
                }
            }
            ActorEvent::MonitorsChanged => {
                self.on_monitors_changed().await;
            }
            ActorEvent::Pairs { screen, pairings } => {
                self.pairs_inflight.remove(&screen);
                self.pending_pairings
                    .retain(|entry| entry.screen_id != screen);
                self.pending_pairings.extend(pairings);
                self.publish(true);
            }
            ActorEvent::Clients { screen, devices } => {
                self.clients_inflight.remove(&screen);
                self.devices.retain(|entry| entry.screen_id != screen);
                self.devices.extend(devices);
                self.publish(true);
            }
        }
    }
}

/// States in which a screen is being driven or already running, so a new start is a
/// conflict. `Preparing` only means "queued", so it is not included.
fn occupies_screen(state: ScreenState) -> bool {
    matches!(
        state,
        ScreenState::Starting
            | ScreenState::WaitingPortal
            | ScreenState::Ready
            | ScreenState::Streaming
            | ScreenState::Stopping
    )
}

/// Convert a command-level error into the screen error record used in state.
fn stage_error_for(err: DaemonError) -> StageError {
    let stage = match &err {
        DaemonError::NotFound(_) | DaemonError::InvalidArgs(_) => Stage::Validate,
        DaemonError::Conflict(_) | DaemonError::Unavailable(_) => Stage::Validate,
        DaemonError::Sunshine(_) => Stage::SunshineApi,
        DaemonError::Mutter(_) => Stage::Layout,
        DaemonError::Portal(_) => Stage::PortalAuth,
        DaemonError::Io(_) => Stage::Io,
    };
    StageError::new(stage, err.message())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Actor {
    fn any_streaming(&self) -> bool {
        self.screen_states
            .values()
            .any(|state| *state == ScreenState::Streaming)
    }

    fn notify(&self, key: &str, summary: &str, body: &str) {
        if self.config.general.notifications {
            self.notifier.notify(key, summary, body);
        }
    }

    fn notify_portal(&mut self, screen: &str) {
        let Some(config) = self.config.screen(screen).cloned() else {
            return;
        };
        let body = format!(
            "Approve {} ({}x{}) in GNOME's screen picker",
            config.name, config.width, config.height
        );
        self.notify(&format!("portal:{screen}"), "Portal approval needed", &body);
    }

    fn arm_portal_deadline(&mut self, screen: &str) {
        if let Some(runtime) = self.runtimes.get_mut(screen) {
            runtime.portal_deadline = Some(Instant::now() + PORTAL_TIMEOUT);
        }
    }

    fn arm_idle_deadline(&mut self) {
        let minutes = self.config.general.stop_when_idle_minutes;
        if minutes == 0 {
            self.idle_deadline = None;
        } else {
            self.idle_deadline =
                Some(Instant::now() + Duration::from_secs(u64::from(minutes) * 60));
        }
    }

    fn refresh_host(&mut self) {
        self.host = plan::host_entry();
        self.host_refreshed = Instant::now();
    }

    async fn handle_instance_event(&mut self, screen: &str, event: InstanceEvent) {
        match event {
            InstanceEvent::Line(line) => {
                self.logs.screen_line(screen, line);
            }
            InstanceEvent::LogMissing => {
                self.logs
                    .screen_line(screen, "Sunshine log file never appeared");
            }
            InstanceEvent::LogClosed => {
                if self.screen_state(screen) == ScreenState::Stopping {
                    return;
                }
                let unit = systemd::unit_name(screen);
                let state = systemd::unit_state(&unit).await;
                if state.contains("failed") || state.contains("inactive") {
                    let error = StageError::new(
                        Stage::SunshineSpawn,
                        format!("Sunshine unit {unit} stopped ({state})"),
                    );
                    self.set_error(screen, error);
                    self.notify(
                        "sunshine-stopped",
                        "Sunshine stopped",
                        &format!("The stream for '{screen}' stopped"),
                    );
                    if self.spawn_stop(screen, StopCause::Rollback) {
                        self.in_flight = Some(Action::Stop(screen.to_string()));
                    }
                    self.publish(false);
                }
            }
            InstanceEvent::Log(LogEvent::Connected) => {
                if let Some(runtime) = self.runtimes.get_mut(screen) {
                    runtime.connected = true;
                }
                self.set_state(screen, ScreenState::Streaming);
                self.idle_deadline = None;
                self.session_started
                    .insert(screen.to_string(), now_rfc3339());
                if let Some(runtime) = self.runtimes.get_mut(screen) {
                    runtime.samples.clear();
                    runtime.last_sample = Instant::now();
                }
                let name = self
                    .config
                    .screen(screen)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| screen.to_string());
                self.notify(
                    &format!("connected:{screen}"),
                    &format!("{name} connected"),
                    "Streaming started",
                );
                self.publish(false);
            }
            InstanceEvent::Log(LogEvent::Disconnected) => {
                if let Some(runtime) = self.runtimes.get_mut(screen) {
                    runtime.connected = false;
                }
                if self.screen_state(screen) == ScreenState::Streaming {
                    self.set_state(screen, ScreenState::Ready);
                }
                self.finish_session(screen, "client disconnected");
                if self.idle_deadline.is_none() {
                    self.arm_idle_deadline();
                }
                self.publish(false);
            }
            InstanceEvent::Log(LogEvent::Bitrate(bps)) => {
                if let Some(runtime) = self.runtimes.get_mut(screen) {
                    runtime.bitrate_kbps = (bps / 1000) as u32;
                    if runtime.last_sample.elapsed() >= SAMPLE_INTERVAL {
                        runtime.samples.push(runtime.bitrate_kbps);
                        if runtime.samples.len() > 600 {
                            runtime.samples.remove(0);
                        }
                        runtime.last_sample = Instant::now();
                    }
                }
                self.publish(true);
            }
            InstanceEvent::Log(LogEvent::RequestedFps(fps)) => {
                if let Some(runtime) = self.runtimes.get_mut(screen) {
                    runtime.fps = fps;
                }
                self.publish(true);
            }
            InstanceEvent::Log(LogEvent::Encoder(encoder)) => {
                if let Some(runtime) = self.runtimes.get_mut(screen) {
                    runtime.encoder = Some(encoder);
                }
            }
            InstanceEvent::Log(LogEvent::Size(width, height)) => {
                if let Some(runtime) = self.runtimes.get_mut(screen) {
                    runtime.size = Some((width, height));
                }
            }
            InstanceEvent::Log(LogEvent::SessionStarted | LogEvent::SessionEnded) => {}
            InstanceEvent::Log(LogEvent::PortalTokenSaved) => {
                if self.screen_state(screen) == ScreenState::WaitingPortal {
                    self.logs
                        .screen_line(screen, "portal token saved, restarting Sunshine");
                    self.spawn_token_restart(screen);
                }
            }
            InstanceEvent::Log(LogEvent::PortalTokenStale) => {
                self.enter_waiting_portal(screen, "portal token became stale");
            }
            InstanceEvent::Log(LogEvent::PortalError(message)) => {
                self.enter_waiting_portal(screen, &message);
            }
            InstanceEvent::Log(LogEvent::Fatal(message)) => {
                if self.screen_state(screen) == ScreenState::WaitingPortal {
                    // Sunshine gives up on pause: its capture probe runs before the
                    // picker can be answered. The portal request is still open and the
                    // token restarts it, so keep waiting for approval instead.
                    self.logs.screen_line(
                        screen,
                        format!("Sunshine startup probe failed while awaiting approval: {message}"),
                    );
                    self.arm_portal_deadline(screen);
                    self.notify_portal(screen);
                    self.publish(false);
                    return;
                }
                let error = StageError::new(Stage::SunshineApi, message);
                self.set_error(screen, error);
                self.notify(
                    "sunshine-stopped",
                    "Sunshine stopped",
                    &format!("The stream for '{screen}' failed"),
                );
                if self.spawn_stop(screen, StopCause::Rollback) {
                    self.in_flight = Some(Action::Stop(screen.to_string()));
                }
                self.publish(false);
            }
        }
    }

    fn enter_waiting_portal(&mut self, screen: &str, reason: &str) {
        if !matches!(
            self.screen_state(screen),
            ScreenState::Ready | ScreenState::Starting | ScreenState::WaitingPortal
        ) {
            return;
        }
        self.logs
            .screen_line(screen, format!("portal needs approval: {reason}"));
        self.set_state(screen, ScreenState::WaitingPortal);
        self.arm_portal_deadline(screen);
        self.notify_portal(screen);
        self.publish(false);
    }

    fn poll_pairs(&mut self) {
        let targets: Vec<String> = self
            .runtimes
            .iter()
            .filter(|(id, _)| {
                matches!(
                    self.screen_state(id),
                    ScreenState::Ready | ScreenState::Streaming
                ) && !self.pairs_inflight.contains(*id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in targets {
            let Some(runtime) = self.runtimes.get(&id) else {
                continue;
            };
            let api = runtime.instance.api();
            self.pairs_inflight.insert(id.clone());
            let tx = self.evt_tx.clone();
            let screen = id.clone();
            tokio::spawn(async move {
                let pairings = match api.pin_pairings().await {
                    Ok(requests) => plan::pending_for_screen(&screen, requests),
                    Err(_) => Vec::new(),
                };
                let _ = tx.send(ActorEvent::Pairs { screen, pairings }).await;
            });
        }
    }

    fn poll_clients(&mut self) {
        let targets: Vec<String> = self
            .runtimes
            .iter()
            .filter(|(id, _)| {
                matches!(
                    self.screen_state(id),
                    ScreenState::Ready | ScreenState::Streaming
                ) && !self.clients_inflight.contains(*id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in targets {
            let Some(runtime) = self.runtimes.get(&id) else {
                continue;
            };
            let api = runtime.instance.api();
            self.clients_inflight.insert(id.clone());
            let tx = self.evt_tx.clone();
            let screen = id.clone();
            tokio::spawn(async move {
                let devices = match api.clients().await {
                    Ok(clients) => plan::devices_for_screen(&screen, clients),
                    Err(_) => Vec::new(),
                };
                let _ = tx.send(ActorEvent::Clients { screen, devices }).await;
            });
        }
    }

    fn poll_health(&mut self) {
        for id in self.runtimes.keys().cloned().collect::<Vec<_>>() {
            let unit = systemd::unit_name(&id);
            let tx = self.evt_tx.clone();
            tokio::spawn(async move {
                let state = systemd::unit_state(&unit).await;
                let _ = tx.send(ActorEvent::Health { screen: id, state }).await;
            });
        }
    }

    /// Detect a virtual output that vanished without a stop request.
    async fn check_connectors(&mut self) {
        self.last_connector_poll = Instant::now();
        if self.runtimes.is_empty() {
            return;
        }
        let Ok(client) = DisplayConfigClient::new().await else {
            return;
        };
        let Ok(state) = client.current_state().await else {
            return;
        };
        let present: HashSet<String> = state.connectors().into_iter().collect();
        let vanished: Vec<String> = self
            .runtimes
            .iter()
            .filter(|(_, runtime)| !present.contains(&runtime.connector))
            .map(|(id, _)| id.clone())
            .collect();
        self.display_state = Some(state);
        for id in vanished {
            if self.screen_state(&id) == ScreenState::Stopping {
                continue;
            }
            self.logs
                .screen_line(&id, "virtual output disappeared from DisplayConfig");
            let error = StageError::new(
                Stage::MutterSession,
                "Mutter closed the virtual output's ScreenCast session (for example GNOME's Stop Sharing)",
            );
            self.set_error(&id, error);
            self.warnings
                .insert(longcar_core::model::warnings::SESSION_CLOSED.to_string());
            self.notify(
                "session-closed",
                "Session closed",
                &format!("The virtual output for '{id}' was closed by GNOME"),
            );
            if self.spawn_stop(&id, StopCause::Rollback) {
                self.in_flight = Some(Action::Stop(id));
            }
        }
        self.publish(false);
    }

    async fn on_tick(&mut self) {
        if let Some(at) = self.autostart_at
            && Instant::now() >= at
        {
            {
                self.autostart_at = None;
                let preset = self.config.general.default_preset.clone();
                if !preset.is_empty() {
                    let _ = self.apply_preset(&preset);
                } else if !self.config.screens.is_empty() {
                    for screen in self.config.screens.clone() {
                        self.enqueue_start(&screen.id);
                    }
                }
                self.process_queue().await;
            }
        }

        let waiting: Vec<String> = self
            .runtimes
            .iter()
            .filter(|(id, _)| self.screen_state(id) == ScreenState::WaitingPortal)
            .map(|(id, _)| id.clone())
            .collect();
        for id in waiting {
            let has_token = self
                .runtimes
                .get(&id)
                .map(|runtime| runtime.instance.portal_token_present())
                .unwrap_or(false);
            if has_token {
                if let Some(runtime) = self.runtimes.get_mut(&id) {
                    runtime.portal_deadline = None;
                }
                self.logs
                    .screen_line(&id, "portal token file appeared, restarting Sunshine");
                self.spawn_token_restart(&id);
                continue;
            }
            let expired = self
                .runtimes
                .get(&id)
                .and_then(|runtime| runtime.portal_deadline)
                .map(|deadline| Instant::now() >= deadline)
                .unwrap_or(false);
            if expired {
                let error = StageError::new(
                    Stage::PortalAuth,
                    "the screen was not approved in GNOME's picker within 120 seconds",
                );
                self.set_error(&id, error);
                self.warnings
                    .insert(longcar_core::model::warnings::PORTAL_TIMEOUT.to_string());
                if self.spawn_stop(&id, StopCause::Rollback) {
                    self.in_flight = Some(Action::Stop(id));
                }
                self.publish(false);
            }
        }

        if let Some(deadline) = self.idle_deadline
            && Instant::now() >= deadline
            && !self.any_streaming()
            && !self.runtimes.is_empty()
        {
            {
                self.logs
                    .daemon_line("idle timeout reached, stopping screens");
                let _ = self.stop_all();
                self.process_queue().await;
            }
        }

        if self.last_pairs_poll.elapsed() >= PAIRS_POLL {
            self.last_pairs_poll = Instant::now();
            self.poll_pairs();
        }
        if self.last_clients_poll.elapsed() >= CLIENTS_POLL {
            self.last_clients_poll = Instant::now();
            self.poll_clients();
        }
        if self.last_health_poll.elapsed() >= HEALTH_POLL {
            self.last_health_poll = Instant::now();
            self.poll_health();
        }
        if self.last_connector_poll.elapsed() >= Duration::from_secs(2) {
            self.check_connectors().await;
        }
        if self.host_refreshed.elapsed() >= HOST_POLL {
            self.refresh_host();
        }
        self.publish(true);
    }

    async fn on_monitors_changed(&mut self) {
        if let Some(until) = self.suppress_until
            && Instant::now() < until
        {
            return;
        }
        let Ok(client) = DisplayConfigClient::new().await else {
            return;
        };
        let Ok(state) = client.current_state().await else {
            return;
        };
        let physical = plan::physical_entries(&state);
        self.display_state = Some(state);
        if self.runtimes.is_empty() {
            if !self.applied_physical.is_empty() && physical != self.applied_physical {
                // Physical layout changed while nothing was active: keep the snapshot
                // valid by re-recording it when it exists? Leave it; only warn when a
                // screen is running.
            }
            self.applied_physical = physical;
        } else if physical != self.applied_physical {
            self.layout_user_modified = true;
            self.warnings
                .insert(longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE.to_string());
            self.logs
                .daemon_line("physical monitor layout changed while a screen is active");
            self.applied_physical = physical;
        }
        self.publish(false);
    }

    async fn after_last_stop(&mut self) {
        let path = paths::layout_snapshot_path();
        if !path.exists() {
            self.snapshot_present = false;
            self.warnings
                .remove(longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE);
        } else if self.config.general.restore_layout_on_stop && !self.layout_user_modified {
            match self.apply_snapshot_file(&path).await {
                Ok(()) => {
                    let _ = std::fs::remove_file(&path);
                    self.snapshot_present = false;
                    self.layout_user_modified = false;
                    self.warnings
                        .remove(longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE);
                    self.logs.daemon_line("previous monitor layout restored");
                }
                Err(err) => {
                    self.warnings.insert(
                        longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE.to_string(),
                    );
                    self.logs
                        .daemon_line(format!("layout restore failed: {err}"));
                    self.notify(
                        "layout-restore",
                        "Layout restore available",
                        "Your previous monitor layout could not be restored automatically",
                    );
                }
            }
        } else if !self.layout_user_modified {
            let _ = std::fs::remove_file(&path);
            self.snapshot_present = false;
            self.warnings
                .remove(longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE);
        } else {
            self.warnings
                .insert(longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE.to_string());
            self.notify(
                "layout-restore",
                "Layout restore available",
                "Your monitor layout was changed while streaming; restore the saved layout from Longcar",
            );
        }

        if let Ok(client) = DisplayConfigClient::new().await
            && let Ok(state) = client.current_state().await
        {
            self.applied_physical = plan::physical_entries(&state);
            self.display_state = Some(state);
        }
        self.publish(false);
    }

    async fn apply_snapshot_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let snapshot: LayoutSnapshot = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let client = DisplayConfigClient::new()
            .await
            .map_err(|e| e.to_string())?;
        let warnings = client.restore(&snapshot).await.map_err(|e| e.to_string())?;
        for warning in warnings {
            self.logs
                .daemon_line(format!("layout restore warning: {warning}"));
        }
        Ok(())
    }

    fn finish_session(&mut self, screen: &str, reason: &str) {
        let Some(runtime) = self.runtimes.get(screen) else {
            return;
        };
        let started_at = self
            .session_started
            .remove(screen)
            .unwrap_or_else(now_rfc3339);
        let mut record = SessionRecord {
            id: uuid::Uuid::new_v4().to_string(),
            screen_id: screen.to_string(),
            started_at,
            ended_at: None,
            encoder: runtime.encoder.clone(),
            width: runtime.size.map(|size| size.0),
            height: runtime.size.map(|size| size.1),
            fps: Some(runtime.fps),
            bitrate_samples: runtime.samples.clone(),
            avg_bitrate_kbps: None,
            max_bitrate_kbps: None,
            reason: None,
        };
        record.finalize(now_rfc3339(), reason);
        if let Err(err) = plan::append_session(&record) {
            self.logs
                .daemon_line(format!("cannot append session history: {err}"));
        }
        self.sessions.push(record);
        if self.sessions.len() > 200 {
            self.sessions.remove(0);
        }
    }

    fn publish(&mut self, stats_only: bool) {
        let mut states = Vec::new();
        let mut screens = Vec::new();
        for screen in self.config.screens.clone() {
            let state = self.screen_state(&screen.id);
            states.push(state);
            let runtime = self.runtimes.get(&screen.id);
            let entry = plan::screen_entry(
                &screen,
                state,
                runtime.map(|r| r.logical),
                runtime.map(|r| r.connector.clone()),
                runtime.map(|r| r.display.node_id()),
                runtime.map(|r| r.connected).unwrap_or(false),
                runtime.map(|r| r.bitrate_kbps).unwrap_or(0),
                runtime.map(|r| r.fps).unwrap_or(0),
                self.errors.get(&screen.id).cloned(),
            );
            screens.push(entry);
        }
        if states.contains(&ScreenState::Error) {
            self.warnings
                .insert(longcar_core::model::warnings::DAEMON_DEGRADED.to_string());
        } else {
            self.warnings
                .remove(longcar_core::model::warnings::DAEMON_DEGRADED);
        }
        let layout_restore = self
            .warnings
            .contains(longcar_core::model::warnings::LAYOUT_RESTORE_AVAILABLE);
        let work_in_flight = self.in_flight.is_some() || !self.queue.is_empty();
        let derived =
            plan::derive_daemon_state(&states, layout_restore, self.stopping_all, work_in_flight);
        if derived != self.daemon.state {
            self.daemon = DaemonStatus::now(derived);
        }
        let doc = StateDoc {
            schema_version: longcar_core::state::SCHEMA_VERSION,
            daemon: self.daemon.clone(),
            screens,
            physical_monitors: self.applied_physical.clone(),
            presets: plan::preset_entries(&self.config),
            settings: plan::settings_entry(&self.config),
            pending_pairings: self.pending_pairings.clone(),
            devices: self.devices.clone(),
            host: self.host.clone(),
            warnings: plan::warnings_vec(&self.warnings),
        };
        self.state_doc = doc.clone();
        if let Ok(mut shared) = self.state.write() {
            *shared = doc.clone();
        }
        if !stats_only || self.last_stats_publish.elapsed() >= STATS_PUBLISH_INTERVAL {
            let _ = self.changed.send(doc.to_json());
            self.last_stats_publish = Instant::now();
        }
    }
}

/// Write the pre-start layout snapshot atomically (0600).
fn write_snapshot(snapshot: &LayoutSnapshot) -> Result<(), StageError> {
    let path = paths::layout_snapshot_path();
    if let Some(parent) = path.parent() {
        create_private_dir(parent).map_err(|e| {
            StageError::new(Stage::Layout, format!("cannot create {parent:?}: {e}"))
        })?;
    }
    let body = serde_json::to_vec_pretty(snapshot)
        .map_err(|e| StageError::new(Stage::Layout, e.to_string()))?;
    crate::instance::write_private(&path, &body)
        .map_err(|e| StageError::new(Stage::Layout, format!("cannot write {path:?}: {e}")))?;
    Ok(())
}

fn display_stage(error: &longcar_display::DisplayError) -> Stage {
    use longcar_display::DisplayError as E;
    match error {
        E::Version(_) | E::Session(_) => Stage::MutterSession,
        E::RecordVirtual(_) | E::MonitorMissing(_) => Stage::RecordVirtual,
        E::PipeWireNode(_) => Stage::PipeWireNode,
        E::Pipeline(_) => Stage::PipeWireNode,
        E::LayoutMode(_) => Stage::Layout,
        E::Bus(_) | E::Closed | E::Io(_) | E::Other(_) => Stage::MutterSession,
    }
}

/// Full start sequence: snapshot, virtual output, layout, Sunshine instance.
async fn start_task(
    screen: &ScreenConfig,
    general: &GeneralConfig,
    seeds: Vec<VirtualSeed>,
    need_snapshot: bool,
    logs: &LogHub,
    evt: &mpsc::Sender<ActorEvent>,
) -> Result<StartedScreen, StageError> {
    let client = DisplayConfigClient::new()
        .await
        .map_err(|e| StageError::new(Stage::MutterSession, e.to_string()))?;

    let mut snapshot_written = false;
    if need_snapshot {
        let snapshot = client.snapshot().await.map_err(|e| {
            StageError::new(
                Stage::Layout,
                format!("cannot snapshot the current layout: {e}"),
            )
        })?;
        write_snapshot(&snapshot)?;
        snapshot_written = true;
        logs.daemon_line(format!(
            "saved layout snapshot with {} logical monitors",
            snapshot.logical_monitors.len()
        ));
    }

    let state = client
        .current_state()
        .await
        .map_err(|e| StageError::new(Stage::MutterSession, e.to_string()))?;
    let existing: HashSet<String> = state.connectors().into_iter().collect();

    let spec = DisplaySpec {
        width: screen.width,
        height: screen.height,
        refresh_hz: screen.refresh_hz,
        cursor_mode: screen.cursor.mutter_value(),
    };
    let display = MutterDisplay::create(spec, &existing)
        .await
        .map_err(|e| StageError::new(display_stage(&e), e.to_string()))?;

    match finish_start(&client, display, screen, general, seeds, logs, evt).await {
        Ok(mut started) => {
            started.snapshot_written = snapshot_written;
            Ok(started)
        }
        Err((err, display)) => {
            let _ = display.stop().await;
            Err(err)
        }
    }
}

/// Layout application, verification and Sunshine startup for an existing output.
///
/// Returns the display back on failure so the caller can tear it down.
#[allow(clippy::result_large_err)]
async fn finish_start(
    client: &DisplayConfigClient,
    display: MutterDisplay,
    screen: &ScreenConfig,
    general: &GeneralConfig,
    mut seeds: Vec<VirtualSeed>,
    logs: &LogHub,
    evt: &mpsc::Sender<ActorEvent>,
) -> Result<StartedScreen, (StageError, MutterDisplay)> {
    let fail = |err: StageError, display: MutterDisplay| (err, display);
    // Mutter only offers this output's own scale list (for example 1.0, 1.25, 1.667,
    // 2.0 — never 1.5 for a 2560x1600 output). Fall back to the closest supported scale
    // instead of failing the start, and say so in the screen log.
    let scales = display.supported_scales().to_vec();
    let scale = match longcar_core::layout::choose_scale(&scales, screen.scale) {
        Ok(scale) => scale,
        Err(err) => match scales.iter().copied().min_by(|a, b| {
            (a - screen.scale)
                .abs()
                .total_cmp(&(b - screen.scale).abs())
        }) {
            Some(fallback) => {
                logs.screen_line(
                    &screen.id,
                    format!(
                        "{err}; using the closest supported scale {fallback} (offered: {scales:?})"
                    ),
                );
                fallback
            }
            None => screen.scale,
        },
    };

    let connector = display.connector().to_string();
    let mode_id = display.mode_id().to_string();
    let mut seed = VirtualSeed::from_screen(screen, &connector, &mode_id);
    seed.scale = scale;
    seed.supported_scales = scales;
    seeds.push(seed);

    let state = match client.current_state().await {
        Ok(state) => state,
        Err(e) => {
            return Err(fail(
                StageError::new(Stage::MutterSession, e.to_string()),
                display,
            ));
        }
    };
    let physical = plan::physical_monitors(&state);
    let virtuals = plan::virtual_monitors(&seeds);
    let plan = match plan_layout(&physical, &virtuals) {
        Ok(plan) => plan,
        Err(e) => return Err(fail(StageError::new(Stage::Layout, e.to_string()), display)),
    };
    if let Err(e) = client.apply_layout(&plan.monitors).await {
        return Err(fail(StageError::new(Stage::Layout, e.to_string()), display));
    }

    let after = match client.current_state().await {
        Ok(state) => state,
        Err(e) => return Err(fail(StageError::new(Stage::Layout, e.to_string()), display)),
    };
    if let Err(err) = verify_virtuals(&after, &plan, &connector, &screen.id) {
        return Err(fail(err, display));
    }
    let logical = plan
        .virtual_rects
        .iter()
        .find(|(id, _)| id == &screen.id)
        .map(|(_, rect)| *rect)
        .unwrap_or_default();
    let plan_warnings = plan.warnings.clone();

    let _ = evt
        .send(ActorEvent::Progress {
            screen: screen.id.clone(),
            state: ScreenState::Starting,
        })
        .await;

    let family = match PortFamily::for_slot(screen.slot) {
        Ok(family) => family,
        Err(e) => {
            return Err(fail(
                StageError::new(Stage::Validate, e.to_string()),
                display,
            ));
        }
    };
    let (instance_tx, mut instance_rx) = mpsc::channel(512);
    let forward = evt.clone();
    let forward_screen = screen.id.clone();
    tokio::spawn(async move {
        while let Some(event) = instance_rx.recv().await {
            if forward
                .send(ActorEvent::Instance {
                    screen: forward_screen.clone(),
                    event,
                })
                .await
                .is_err()
            {
                return;
            }
        }
    });

    let mut instance = match SunshineInstance::start(screen, general, &family, instance_tx).await {
        Ok(instance) => instance,
        Err(e) => {
            let stage = match e {
                crate::instance::InstanceError::Stage(stage) => stage,
                crate::instance::InstanceError::Io(io) => {
                    StageError::new(Stage::Io, io.to_string())
                }
            };
            return Err(fail(stage, display));
        }
    };
    instance.spawn_log_tail();
    logs.screen_line(
        &screen.id,
        format!(
            "started unit {} on port {}",
            instance.unit, instance.web_port
        ),
    );

    Ok(StartedScreen {
        display,
        instance,
        connector,
        mode_id,
        logical,
        warnings: plan_warnings,
        snapshot_written: false,
    })
}

/// Apply the layout for the current set of virtual outputs (position changes).
async fn replan_task(seeds: Vec<VirtualSeed>, logs: &LogHub) -> Result<(), StageError> {
    let client = DisplayConfigClient::new()
        .await
        .map_err(|e| StageError::new(Stage::MutterSession, e.to_string()))?;
    let state = client
        .current_state()
        .await
        .map_err(|e| StageError::new(Stage::MutterSession, e.to_string()))?;
    let physical = plan::physical_monitors(&state);
    let virtuals = plan::virtual_monitors(&seeds);
    let plan = plan_layout(&physical, &virtuals)
        .map_err(|e| StageError::new(Stage::Layout, e.to_string()))?;
    client
        .apply_layout(&plan.monitors)
        .await
        .map_err(|e| StageError::new(Stage::Layout, e.to_string()))?;
    logs.daemon_line("layout re-applied");
    Ok(())
}

/// Assert the new output landed at the planned position and every virtual monitor is
/// present exactly once.
fn verify_virtuals(
    state: &DisplayState,
    plan: &longcar_core::layout::LayoutPlan,
    connector: &str,
    screen_id: &str,
) -> Result<(), StageError> {
    let expected = plan
        .virtual_rects
        .iter()
        .find(|(id, _)| id == screen_id)
        .map(|(_, rect)| *rect);
    let Some(expected) = expected else {
        return Err(StageError::new(
            Stage::Layout,
            format!("the layout plan has no rect for '{screen_id}'"),
        ));
    };
    let actual = state
        .logical_monitors
        .iter()
        .find(|logical| {
            logical
                .monitors
                .iter()
                .any(|reference| reference.connector == connector)
        })
        .ok_or_else(|| {
            StageError::new(
                Stage::Layout,
                format!("'{connector}' is missing from the applied layout"),
            )
        })?;
    let expected_scale = plan
        .monitors
        .iter()
        .find(|monitor| {
            monitor
                .monitors
                .iter()
                .any(|reference| reference.connector == connector)
        })
        .map(|monitor| monitor.scale)
        .unwrap_or(actual.scale);
    if (actual.scale - expected_scale).abs() > 0.01 {
        return Err(StageError::new(
            Stage::Layout,
            format!(
                "'{connector}' was applied at scale {} instead of {expected_scale}",
                actual.scale
            ),
        ));
    }
    if (actual.x - expected.x).abs() > 2 || (actual.y - expected.y).abs() > 2 {
        return Err(StageError::new(
            Stage::Layout,
            format!(
                "'{connector}' landed at ({}, {}) instead of ({}, {})",
                actual.x, actual.y, expected.x, expected.y
            ),
        ));
    }
    let virtual_count = state
        .logical_monitors
        .iter()
        .filter(|logical| {
            logical
                .monitors
                .iter()
                .any(|reference| reference.connector.starts_with("Meta-"))
        })
        .count();
    if virtual_count < plan.virtual_rects.len() {
        return Err(StageError::new(
            Stage::Layout,
            format!(
                "expected {} virtual monitors, saw {virtual_count}",
                plan.virtual_rects.len()
            ),
        ));
    }
    Ok(())
}
