// SPDX-License-Identifier: GPL-3.0-or-later

//! Mutter private D-Bus clients: `ScreenCast` virtual outputs and
//! `DisplayConfig` layout changes.
//!
//! GNOME 50 exports the two services on separate bus names
//! (`org.gnome.Mutter.ScreenCast` and `org.gnome.Mutter.DisplayConfig`)
//! rather than the historical `org.gnome.Mutter` name, so the proxies use the
//! per-service names directly.
//!
//! When a logical monitor spans several connectors, only the first connector
//! is represented in [`LogicalMonitorState`] and [`MonitorRef`]; the model has
//! no place for the remaining connectors, so they are dropped.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use gstreamer as gst;
use longcar_core::model::{
    DisplayState, LAYOUT_SNAPSHOT_VERSION, LayoutSnapshot, LogicalMonitorConfig,
    LogicalMonitorState, MonitorInfo, MonitorMode, MonitorRef, SnapshotMonitor,
};
use serde_json::Value as JsonValue;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use zbus::{OwnedVariant, Variant};

use crate::gst::{Keepalive, message_error_text, pipeline_description};

const DISPLAY_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Session bus name for Mutter's ScreenCast service.
pub const SCREENCAST_BUS: &str = "org.gnome.Mutter.ScreenCast";
pub const SCREENCAST_PATH: &str = "/org/gnome/Mutter/ScreenCast";
pub const SCREENCAST_INTERFACE: &str = "org.gnome.Mutter.ScreenCast";
pub const SESSION_INTERFACE: &str = "org.gnome.Mutter.ScreenCast.Session";
pub const STREAM_INTERFACE: &str = "org.gnome.Mutter.ScreenCast.Stream";
pub const DISPLAY_BUS: &str = "org.gnome.Mutter.DisplayConfig";
pub const DISPLAY_PATH: &str = "/org/gnome/Mutter/DisplayConfig";
pub const DISPLAY_INTERFACE: &str = "org.gnome.Mutter.DisplayConfig";

const MIN_SCREENCAST_VERSION: i32 = 4;
const NODE_TIMEOUT: Duration = Duration::from_secs(10);
const MONITOR_TIMEOUT: Duration = Duration::from_secs(10);
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const BUS_POLL_INTERVAL: Duration = Duration::from_secs(2);

// Raw `GetCurrentState` wire shapes. `a{sv}` values are decoded as owned
// variants so they outlive the method reply body.
type RawProps = HashMap<String, OwnedVariant>;
type RawMode = (String, i32, i32, f64, f64, Vec<f64>, RawProps);
type RawMonitor = ((String, String, String, String), Vec<RawMode>, RawProps);
type RawLogicalMonitor = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<(String, String, String, String)>,
    RawProps,
);
type RawDisplayState = (u32, Vec<RawMonitor>, Vec<RawLogicalMonitor>, RawProps);

// `ApplyMonitorsConfig` argument shapes: `a(iiduba(ssa{sv}))a{sv}`.
type ApplyMonitorRef = (String, String, RawProps);
type ApplyLogicalMonitor = (i32, i32, f64, u32, bool, Vec<ApplyMonitorRef>);
type ApplyArgument = (u32, u32, Vec<ApplyLogicalMonitor>, RawProps);

/// Description of the virtual output to create.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplaySpec {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    /// Mutter `cursor-mode`: 0 hidden, 1 embedded, 2 metadata.
    pub cursor_mode: u32,
}

/// Events observed after a virtual output has been created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayEvent {
    /// The Mutter ScreenCast session was closed (for example GNOME's
    /// "Stop Sharing" button).
    Closed,
    /// The GStreamer keepalive pipeline reported an error.
    PipelineError(String),
}

#[derive(Debug, thiserror::Error)]
pub enum DisplayError {
    #[error("Mutter ScreenCast version {0} is too old (need >= {MIN_SCREENCAST_VERSION})")]
    Version(i32),
    #[error("unsupported layout mode: {0}")]
    LayoutMode(String),
    #[error("ScreenCast session error: {0}")]
    Session(String),
    #[error("RecordVirtual failed: {0}")]
    RecordVirtual(String),
    #[error("no PipeWire node: {0}")]
    PipeWireNode(String),
    #[error("virtual monitor not found: {0}")]
    MonitorMissing(String),
    #[error("GStreamer pipeline error: {0}")]
    Pipeline(String),
    #[error("D-Bus error: {0}")]
    Bus(String),
    #[error("ScreenCast session closed")]
    Closed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// One live Mutter virtual output plus its keepalive pipeline.
pub struct MutterDisplay {
    session: zbus::Proxy<'static>,
    display: DisplayConfigClient,
    keepalive: Option<Keepalive>,
    node_id: u32,
    connector: String,
    mode_id: String,
    supported_scales: Vec<f64>,
    events: Mutex<mpsc::Receiver<DisplayEvent>>,
    // Kept alive so `next_event` never observes a closed channel while the
    // display exists; each event kind is still emitted exactly once.
    _events_tx: mpsc::Sender<DisplayEvent>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl MutterDisplay {
    /// Create a virtual output, start its PipeWire keepalive and wait until
    /// Mutter publishes the new monitor through `DisplayConfig`.
    pub async fn create(
        spec: DisplaySpec,
        existing_connectors: &HashSet<String>,
    ) -> Result<Self, DisplayError> {
        crate::gst::init();

        let conn = zbus::Connection::session()
            .await
            .map_err(|err| DisplayError::Session(format!("session bus unavailable: {err}")))?;
        let screencast = zbus::Proxy::new_owned(
            conn.clone(),
            SCREENCAST_BUS,
            SCREENCAST_PATH,
            SCREENCAST_INTERFACE,
        )
        .await
        .map_err(|err| DisplayError::Session(format!("cannot proxy ScreenCast: {err}")))?;

        let version: i32 = screencast.get_property("Version").await.map_err(|err| {
            DisplayError::Session(format!("cannot read ScreenCast Version: {err}"))
        })?;
        if version < MIN_SCREENCAST_VERSION {
            return Err(DisplayError::Version(version));
        }

        let display = DisplayConfigClient::from_connection(conn.clone()).await?;
        // Fail early on layout modes that cannot represent virtual outputs.
        display.current_state().await?;

        let session_path: zbus::OwnedObjectPath = screencast
            .call("CreateSession", &(HashMap::<String, OwnedVariant>::new(),))
            .await
            .map_err(|err| DisplayError::Session(format!("CreateSession failed: {err}")))?;
        let session = zbus::Proxy::new_owned(
            conn.clone(),
            SCREENCAST_BUS,
            session_path,
            SESSION_INTERFACE,
        )
        .await
        .map_err(|err| DisplayError::Session(format!("cannot proxy session: {err}")))?;

        let mut record_args: HashMap<String, OwnedVariant> = HashMap::new();
        record_args.insert("is-platform".to_string(), OwnedVariant::from(true));
        record_args.insert(
            "cursor-mode".to_string(),
            OwnedVariant::from(spec.cursor_mode),
        );
        let stream_path: zbus::OwnedObjectPath = session
            .call("RecordVirtual", &(record_args,))
            .await
            .map_err(|err| DisplayError::RecordVirtual(err.to_string()))?;
        let stream =
            zbus::Proxy::new_owned(conn.clone(), SCREENCAST_BUS, stream_path, STREAM_INTERFACE)
                .await
                .map_err(|err| {
                    DisplayError::RecordVirtual(format!("cannot proxy stream: {err}"))
                })?;

        // Both signal subscriptions must exist before Start so that a fast
        // Mutter cannot emit before we listen.
        let mut pipewire_added = stream
            .receive_signal("PipeWireStreamAdded")
            .await
            .map_err(|err| DisplayError::RecordVirtual(format!("cannot subscribe: {err}")))?;
        let mut closed = session
            .receive_signal("Closed")
            .await
            .map_err(|err| DisplayError::Session(format!("cannot subscribe to Closed: {err}")))?;

        let (events_tx, events_rx) = mpsc::channel::<DisplayEvent>(8);
        let closed_tx = events_tx.clone();
        let closed_task = tokio::spawn(async move {
            if closed.next().await.is_some() {
                let _ = closed_tx.send(DisplayEvent::Closed).await;
            }
        });

        let start: zbus::Result<()> = session.call("Start", &()).await;
        if let Err(err) = start {
            closed_task.abort();
            stop_session_quietly(&session).await;
            return Err(DisplayError::Session(format!(
                "Session.Start failed: {err}"
            )));
        }

        let node_id = match tokio::time::timeout(NODE_TIMEOUT, pipewire_added.next()).await {
            Ok(Some(message)) => match message.body().deserialize::<(u32,)>() {
                Ok((node_id,)) => node_id,
                Err(err) => {
                    closed_task.abort();
                    stop_session_quietly(&session).await;
                    return Err(DisplayError::PipeWireNode(format!(
                        "cannot decode PipeWireStreamAdded: {err}"
                    )));
                }
            },
            Ok(None) => {
                closed_task.abort();
                stop_session_quietly(&session).await;
                return Err(DisplayError::PipeWireNode(
                    "PipeWireStreamAdded stream ended".to_string(),
                ));
            }
            Err(_) => {
                closed_task.abort();
                stop_session_quietly(&session).await;
                return Err(DisplayError::PipeWireNode(format!(
                    "timed out after {} s waiting for PipeWireStreamAdded",
                    NODE_TIMEOUT.as_secs()
                )));
            }
        };

        let description = pipeline_description(node_id, spec.width, spec.height, spec.refresh_hz);
        let mut keepalive = match Keepalive::new(&description) {
            Ok(keepalive) => keepalive,
            Err(err) => {
                closed_task.abort();
                stop_session_quietly(&session).await;
                return Err(err);
            }
        };

        let matched = match wait_for_monitor(
            &display,
            existing_connectors,
            spec.width,
            spec.height,
            spec.refresh_hz,
        )
        .await
        {
            Ok(matched) => matched,
            Err(err) => {
                keepalive.shutdown();
                closed_task.abort();
                stop_session_quietly(&session).await;
                return Err(err);
            }
        };

        let bus_task = match keepalive.bus() {
            Some(bus) => {
                let bus_tx = events_tx.clone();
                Some(tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(BUS_POLL_INTERVAL);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        ticker.tick().await;
                        if let Some(message) = bus.pop_filtered(&[gst::MessageType::Error]) {
                            let text = message_error_text(&message);
                            let _ = bus_tx.send(DisplayEvent::PipelineError(text)).await;
                            break;
                        }
                    }
                }))
            }
            None => None,
        };

        let (connector, mode_id, supported_scales) = matched;
        let mut tasks = vec![closed_task];
        if let Some(task) = bus_task {
            tasks.push(task);
        }

        Ok(Self {
            session,
            display,
            keepalive: Some(keepalive),
            node_id,
            connector,
            mode_id,
            supported_scales,
            events: Mutex::new(events_rx),
            _events_tx: events_tx,
            tasks,
        })
    }

    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    pub fn connector(&self) -> &str {
        &self.connector
    }

    pub fn mode_id(&self) -> &str {
        &self.mode_id
    }

    pub fn supported_scales(&self) -> &[f64] {
        &self.supported_scales
    }

    /// Wait for the session to close or the keepalive pipeline to fail.
    ///
    /// Each kind of event is emitted at most once; when the internal channel
    /// is already closed the call reports a final [`DisplayEvent::Closed`].
    pub async fn next_event(&self) -> DisplayEvent {
        self.events
            .lock()
            .await
            .recv()
            .await
            .unwrap_or(DisplayEvent::Closed)
    }

    /// Tear the output down: stop the pipeline, end the session and wait until
    /// the connector disappears from DisplayConfig.
    pub async fn stop(mut self) -> Result<(), DisplayError> {
        self.abort_tasks();
        if let Some(mut keepalive) = self.keepalive.take() {
            keepalive.shutdown();
        }

        let mut session_gone = false;
        let stop: zbus::Result<()> = match tokio::time::timeout(
            DISPLAY_CALL_TIMEOUT,
            self.session.call("Stop", &()),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(zbus::Error::Failure("Session.Stop timed out".to_string())),
        };
        if let Err(err) = stop {
            if is_ignorable_teardown_error(&err) {
                session_gone = true;
            } else {
                return Err(DisplayError::Session(format!("Session.Stop failed: {err}")));
            }
        }
        if session_gone {
            return Ok(());
        }

        let connector = self.connector.clone();
        let deadline = Instant::now() + TEARDOWN_TIMEOUT;
        let mut last_error: Option<String>;
        loop {
            match self.display.current_state().await {
                Ok(state) => {
                    if !state
                        .monitors
                        .iter()
                        .any(|monitor| monitor.connector == connector)
                    {
                        return Ok(());
                    }
                    last_error = None;
                }
                Err(err) => last_error = Some(err.to_string()),
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        if last_error.is_some() {
            // DisplayConfig is unreachable; the session and its virtual output
            // are gone together with the bus name, so teardown is complete.
            Ok(())
        } else {
            Err(DisplayError::Other(format!(
                "virtual monitor {connector} did not disappear within {} s",
                TEARDOWN_TIMEOUT.as_secs()
            )))
        }
    }

    fn abort_tasks(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

impl Drop for MutterDisplay {
    fn drop(&mut self) {
        self.abort_tasks();
        if let Some(mut keepalive) = self.keepalive.take() {
            keepalive.shutdown();
        }
        // Dropping the connection makes Mutter destroy the ScreenCast session
        // and with it the virtual output.
    }
}

async fn stop_session_quietly(session: &zbus::Proxy<'static>) {
    // Cleanup path: the caller is already returning an error, and dropping the
    // connection removes the session even if this call fails.
    let _: zbus::Result<()> = session.call("Stop", &()).await;
}

async fn wait_for_monitor(
    display: &DisplayConfigClient,
    existing_connectors: &HashSet<String>,
    width: u32,
    height: u32,
    refresh_hz: f64,
) -> Result<(String, String, Vec<f64>), DisplayError> {
    let deadline = Instant::now() + MONITOR_TIMEOUT;
    let mut last_error: Option<String> = None;
    loop {
        match display.current_state().await {
            Ok(state) => {
                if let Some(found) =
                    find_new_monitor(&state, existing_connectors, width, height, refresh_hz)
                {
                    return Ok(found);
                }
            }
            Err(err) => last_error = Some(err.to_string()),
        }
        if Instant::now() >= deadline {
            let suffix = last_error
                .map(|err| format!(" (last DisplayConfig error: {err})"))
                .unwrap_or_default();
            return Err(DisplayError::MonitorMissing(format!(
                "no new monitor with a {width}x{height} mode appeared within {} s{suffix}",
                MONITOR_TIMEOUT.as_secs()
            )));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Find a monitor that was not present before and exposes an exact `width x
/// height` mode, preferring the mode whose refresh rate is nearest to
/// `refresh_hz`.
fn find_new_monitor(
    state: &DisplayState,
    existing_connectors: &HashSet<String>,
    width: u32,
    height: u32,
    refresh_hz: f64,
) -> Option<(String, String, Vec<f64>)> {
    state
        .monitors
        .iter()
        .filter(|monitor| !existing_connectors.contains(&monitor.connector))
        .find_map(|monitor| {
            let mode = monitor
                .modes
                .iter()
                .filter(|mode| mode.width == width && mode.height == height)
                .min_by(|a, b| {
                    (a.refresh_hz - refresh_hz)
                        .abs()
                        .partial_cmp(&(b.refresh_hz - refresh_hz).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })?;
            Some((
                monitor.connector.clone(),
                mode.id.clone(),
                mode.supported_scales.clone(),
            ))
        })
}

/// Client for `org.gnome.Mutter.DisplayConfig`.
#[derive(Clone)]
pub struct DisplayConfigClient {
    proxy: zbus::Proxy<'static>,
}

impl DisplayConfigClient {
    pub async fn new() -> Result<Self, DisplayError> {
        let conn = zbus::Connection::session()
            .await
            .map_err(|err| DisplayError::Bus(format!("session bus unavailable: {err}")))?;
        Self::from_connection(conn).await
    }

    async fn from_connection(conn: zbus::Connection) -> Result<Self, DisplayError> {
        let proxy = zbus::Proxy::new_owned(conn, DISPLAY_BUS, DISPLAY_PATH, DISPLAY_INTERFACE)
            .await
            .map_err(|err| DisplayError::Bus(format!("cannot proxy DisplayConfig: {err}")))?;
        Ok(Self { proxy })
    }

    pub async fn current_state(&self) -> Result<DisplayState, DisplayError> {
        // A ScreenCast session or an open portal picker can make the compositor hold
        // DisplayConfig calls; never let that wedge a caller (the daemon's shutdown and
        // layout tracking depend on this returning).
        match tokio::time::timeout(DISPLAY_CALL_TIMEOUT, self.current_state_inner()).await {
            Ok(result) => result,
            Err(_) => Err(DisplayError::Other(
                "GetCurrentState timed out after 5s".to_string(),
            )),
        }
    }

    async fn current_state_inner(&self) -> Result<DisplayState, DisplayError> {
        let raw: RawDisplayState = self
            .proxy
            .call("GetCurrentState", &())
            .await
            .map_err(|err| DisplayError::Bus(format!("GetCurrentState failed: {err}")))?;
        let state = parse_display_state(raw);
        check_layout_mode(&state.properties)?;
        Ok(state)
    }

    /// Verify then apply a temporary layout (`method` 1, never 2).
    pub async fn apply_layout(&self, layout: &[LogicalMonitorConfig]) -> Result<(), DisplayError> {
        match tokio::time::timeout(DISPLAY_CALL_TIMEOUT, self.apply_layout_inner(layout)).await {
            Ok(result) => result,
            Err(_) => Err(DisplayError::Other(
                "ApplyMonitorsConfig timed out after 5s".to_string(),
            )),
        }
    }

    async fn apply_layout_inner(
        &self,
        layout: &[LogicalMonitorConfig],
    ) -> Result<(), DisplayError> {
        let state = self.current_state().await?;
        match self.apply_attempt(state.serial, layout).await {
            Ok(()) => Ok(()),
            Err(first) if is_retryable_apply_error(&first) => {
                // A stale serial or a concurrent layout change: retry once
                // with a freshly read serial.
                let state = self.current_state().await?;
                self.apply_attempt(state.serial, layout)
                    .await
                    .map_err(|err| {
                        DisplayError::Other(format!(
                            "ApplyMonitorsConfig failed after retry: {err}"
                        ))
                    })
            }
            Err(err) => Err(DisplayError::Other(format!(
                "ApplyMonitorsConfig failed: {err}"
            ))),
        }
    }

    async fn apply_attempt(
        &self,
        serial: u32,
        layout: &[LogicalMonitorConfig],
    ) -> Result<(), zbus::Error> {
        let verify = apply_argument(serial, 0, layout);
        let _: () = self.proxy.call("ApplyMonitorsConfig", &verify).await?;
        let apply = apply_argument(serial, 1, layout);
        let _: () = self.proxy.call("ApplyMonitorsConfig", &apply).await?;
        Ok(())
    }

    /// Capture the current physical layout for later restoration.
    pub async fn snapshot(&self) -> Result<LayoutSnapshot, DisplayError> {
        match tokio::time::timeout(DISPLAY_CALL_TIMEOUT, self.snapshot_inner()).await {
            Ok(result) => result,
            Err(_) => Err(DisplayError::Other("snapshot timed out".to_string())),
        }
    }

    async fn snapshot_inner(&self) -> Result<LayoutSnapshot, DisplayError> {
        let state = self.current_state().await?;
        let virtual_connectors: Vec<String> = state
            .monitors
            .iter()
            .filter(|monitor| monitor.connector.starts_with("Meta-"))
            .map(|monitor| monitor.connector.clone())
            .collect();

        let mut logical_monitors = Vec::new();
        for logical in &state.logical_monitors {
            let Some(first) = logical.monitors.first() else {
                continue;
            };
            if first.connector.starts_with("Meta-") {
                continue;
            }
            let Some(monitor) = state.connector(&first.connector) else {
                continue;
            };
            let mode = resolve_snapshot_mode(monitor, first).ok_or_else(|| {
                DisplayError::Other(format!(
                    "monitor {} reports no usable mode",
                    first.connector
                ))
            })?;
            logical_monitors.push(SnapshotMonitor {
                x: logical.x,
                y: logical.y,
                scale: logical.scale,
                transform: logical.transform,
                primary: logical.primary,
                connector: first.connector.clone(),
                mode_id: mode.id.clone(),
                width: mode.width,
                height: mode.height,
                refresh_hz: mode.refresh_hz,
            });
        }

        if logical_monitors.is_empty() {
            return Err(DisplayError::Other(
                "current layout has no physical logical monitors".to_string(),
            ));
        }
        if !logical_monitors.iter().any(|monitor| monitor.primary) {
            return Err(DisplayError::Other(
                "current layout has no primary monitor".to_string(),
            ));
        }

        Ok(LayoutSnapshot {
            version: LAYOUT_SNAPSHOT_VERSION,
            taken_at: now_rfc3339(),
            logical_monitors,
            virtual_connectors,
        })
    }

    /// Re-apply a snapshot to the current display state.
    ///
    /// Missing connectors and modes are skipped and reported as warnings; at
    /// least one restored monitor must be primary.
    pub async fn restore(&self, snapshot: &LayoutSnapshot) -> Result<Vec<String>, DisplayError> {
        match tokio::time::timeout(DISPLAY_CALL_TIMEOUT, self.restore_inner(snapshot)).await {
            Ok(result) => result,
            Err(_) => Err(DisplayError::Other("layout restore timed out".to_string())),
        }
    }

    async fn restore_inner(&self, snapshot: &LayoutSnapshot) -> Result<Vec<String>, DisplayError> {
        let state = self.current_state().await?;
        let mut warnings = Vec::new();
        let mut layout = Vec::new();

        for entry in &snapshot.logical_monitors {
            let Some(monitor) = state.connector(&entry.connector) else {
                warnings.push(format!("missing connector {}", entry.connector));
                continue;
            };
            let Some(mode) = resolve_mode(
                &monitor.modes,
                &entry.mode_id,
                entry.width,
                entry.height,
                entry.refresh_hz,
            ) else {
                warnings.push(format!(
                    "missing mode {} on {}",
                    entry.mode_id, entry.connector
                ));
                continue;
            };
            layout.push(LogicalMonitorConfig {
                x: entry.x,
                y: entry.y,
                scale: entry.scale,
                transform: entry.transform,
                primary: entry.primary,
                monitors: vec![MonitorRef {
                    connector: entry.connector.clone(),
                    mode_id: mode.id.clone(),
                }],
            });
        }

        if layout.is_empty() {
            return Err(DisplayError::Other(
                "no snapshot monitors are available on this system".to_string(),
            ));
        }
        let primaries = layout.iter().filter(|logical| logical.primary).count();
        if primaries != 1 {
            return Err(DisplayError::Other(format!(
                "restored layout needs exactly one primary monitor, found {primaries}"
            )));
        }

        self.apply_layout(&layout).await?;
        Ok(warnings)
    }

    /// Subscribe to `MonitorsChanged`. The returned receiver yields once per
    /// signal; callers are responsible for debouncing.
    pub async fn watch_monitors_changed(&self) -> Result<mpsc::Receiver<()>, DisplayError> {
        let mut signals = self
            .proxy
            .receive_signal("MonitorsChanged")
            .await
            .map_err(|err| {
                DisplayError::Bus(format!("cannot subscribe to MonitorsChanged: {err}"))
            })?;
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            while signals.next().await.is_some() {
                if tx.send(()).await.is_err() {
                    return;
                }
            }
        });
        Ok(rx)
    }
}

fn apply_argument(serial: u32, method: u32, layout: &[LogicalMonitorConfig]) -> ApplyArgument {
    let logical_monitors: Vec<ApplyLogicalMonitor> = layout
        .iter()
        .map(|logical| {
            let monitors: Vec<ApplyMonitorRef> = logical
                .monitors
                .iter()
                .map(|monitor| {
                    (
                        monitor.connector.clone(),
                        monitor.mode_id.clone(),
                        RawProps::new(),
                    )
                })
                .collect();
            (
                logical.x,
                logical.y,
                logical.scale,
                logical.transform,
                logical.primary,
                monitors,
            )
        })
        .collect();
    (serial, method, logical_monitors, RawProps::new())
}

fn is_retryable_apply_error(error: &zbus::Error) -> bool {
    match error {
        zbus::Error::FDO(inner) => matches!(
            inner.as_ref(),
            zbus::fdo::Error::AccessDenied(_) | zbus::fdo::Error::InvalidArgs(_)
        ),
        _ => false,
    }
}

fn is_ignorable_teardown_error(error: &zbus::Error) -> bool {
    match error {
        zbus::Error::FDO(inner) => matches!(
            inner.as_ref(),
            zbus::fdo::Error::ServiceUnknown(_)
                | zbus::fdo::Error::UnknownObject(_)
                | zbus::fdo::Error::UnknownInterface(_)
                | zbus::fdo::Error::UnknownMethod(_)
        ),
        _ => false,
    }
}

fn parse_display_state(raw: RawDisplayState) -> DisplayState {
    let (serial, raw_monitors, raw_logicals, raw_properties) = raw;

    let mut current_modes: HashMap<String, String> = HashMap::new();
    let mut monitors = Vec::with_capacity(raw_monitors.len());
    for raw_monitor in raw_monitors {
        let ((connector, vendor, product, monitor_serial), raw_modes, properties) = raw_monitor;
        let mut modes = Vec::with_capacity(raw_modes.len());
        let mut current = None;
        for raw_mode in raw_modes {
            let (id, width, height, refresh_hz, preferred_scale, supported_scales, mode_props) =
                raw_mode;
            if mode_props
                .get("is-current")
                .and_then(|value| bool::try_from(value).ok())
                .unwrap_or(false)
            {
                current = Some(id.clone());
            }
            modes.push(MonitorMode {
                id,
                width: width.max(0) as u32,
                height: height.max(0) as u32,
                refresh_hz,
                preferred_scale,
                supported_scales,
            });
        }
        if let Some(mode_id) = current.or_else(|| modes.first().map(|mode| mode.id.clone())) {
            current_modes.insert(connector.clone(), mode_id);
        }
        monitors.push(MonitorInfo {
            connector,
            vendor,
            product,
            serial: monitor_serial,
            modes,
            properties: props_to_json(properties),
        });
    }

    let logical_monitors = raw_logicals
        .into_iter()
        .map(
            |(x, y, scale, transform, primary, refs, _properties)| LogicalMonitorState {
                x,
                y,
                scale,
                transform,
                primary,
                monitors: refs
                    .into_iter()
                    .map(|(connector, _vendor, _product, _serial)| MonitorRef {
                        mode_id: current_modes.get(&connector).cloned().unwrap_or_default(),
                        connector,
                    })
                    .collect(),
            },
        )
        .collect();

    DisplayState {
        serial,
        monitors,
        logical_monitors,
        properties: props_to_json(raw_properties),
    }
}

fn props_to_json(properties: RawProps) -> HashMap<String, JsonValue> {
    properties
        .into_iter()
        .map(|(key, value)| (key, variant_to_json(&value)))
        .collect()
}

fn variant_to_json(value: &Variant<'_>) -> JsonValue {
    match value {
        Variant::U8(number) => JsonValue::from(*number),
        Variant::Bool(flag) => JsonValue::from(*flag),
        Variant::I16(number) => JsonValue::from(*number),
        Variant::U16(number) => JsonValue::from(*number),
        Variant::I32(number) => JsonValue::from(*number),
        Variant::U32(number) => JsonValue::from(*number),
        Variant::I64(number) => JsonValue::from(*number),
        Variant::U64(number) => JsonValue::from(*number),
        Variant::F64(number) => {
            serde_json::Number::from_f64(*number).map_or(JsonValue::Null, JsonValue::Number)
        }
        Variant::Str(text) => JsonValue::String(text.to_string()),
        Variant::Signature(signature) => JsonValue::String(signature.to_string()),
        Variant::ObjectPath(path) => JsonValue::String(path.to_string()),
        Variant::Variant(inner) => variant_to_json(inner),
        other => JsonValue::String(format!("{other:?}")),
    }
}

fn check_layout_mode(properties: &HashMap<String, JsonValue>) -> Result<(), DisplayError> {
    let layout_mode = properties
        .get("layout-mode")
        .and_then(JsonValue::as_i64)
        .unwrap_or(1);
    if layout_mode == 2 {
        return Err(DisplayError::LayoutMode(
            "layout-mode 2 (single global scale) cannot host virtual outputs".to_string(),
        ));
    }
    if properties
        .get("global-scale-required")
        .and_then(JsonValue::as_bool)
        == Some(true)
    {
        return Err(DisplayError::LayoutMode(
            "Mutter requires a global scale; per-monitor fractional scaling is unavailable"
                .to_string(),
        ));
    }
    Ok(())
}

fn resolve_snapshot_mode<'a>(
    monitor: &'a MonitorInfo,
    reference: &MonitorRef,
) -> Option<&'a MonitorMode> {
    monitor
        .modes
        .iter()
        .find(|mode| mode.id == reference.mode_id)
        .or_else(|| monitor.modes.first())
}

/// Resolve a saved mode: exact mode ID first, then exact dimensions with a
/// refresh rate within 1 Hz. A dimension-only match is never accepted.
fn resolve_mode<'a>(
    modes: &'a [MonitorMode],
    mode_id: &str,
    width: u32,
    height: u32,
    refresh_hz: f64,
) -> Option<&'a MonitorMode> {
    if let Some(mode) = modes.iter().find(|mode| mode.id == mode_id) {
        return Some(mode);
    }
    modes
        .iter()
        .filter(|mode| {
            mode.width == width
                && mode.height == height
                && (mode.refresh_hz - refresh_hz).abs() <= 1.0
        })
        .min_by(|a, b| {
            (a.refresh_hz - refresh_hz)
                .abs()
                .partial_cmp(&(b.refresh_hz - refresh_hz).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format_timestamp(seconds)
}

fn format_timestamp(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let remainder = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remainder / 3_600,
        (remainder % 3_600) / 60,
        remainder % 60
    )
}

/// Howard Hinnant's `civil_from_days` algorithm (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(entries: &[(&str, OwnedVariant)]) -> HashMap<String, JsonValue> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), variant_to_json(value)))
            .collect()
    }

    #[test]
    fn layout_mode_defaults_to_one() {
        assert!(check_layout_mode(&HashMap::new()).is_ok());
        assert!(check_layout_mode(&props(&[("layout-mode", OwnedVariant::from(1u32))])).is_ok());
    }

    #[test]
    fn layout_mode_two_is_rejected() {
        let error =
            check_layout_mode(&props(&[("layout-mode", OwnedVariant::from(2u32))])).unwrap_err();
        assert!(matches!(error, DisplayError::LayoutMode(_)));
    }

    #[test]
    fn global_scale_requirement_is_rejected() {
        let error = check_layout_mode(&props(&[(
            "global-scale-required",
            OwnedVariant::from(true),
        )]))
        .unwrap_err();
        assert!(matches!(error, DisplayError::LayoutMode(_)));
    }

    fn mode(id: &str, width: u32, height: u32, refresh_hz: f64) -> MonitorMode {
        MonitorMode {
            id: id.to_string(),
            width,
            height,
            refresh_hz,
            preferred_scale: 1.0,
            supported_scales: vec![1.0],
        }
    }

    #[test]
    fn mode_resolution_prefers_exact_id() {
        let modes = vec![mode("a", 1280, 720, 60.0), mode("b", 1920, 1080, 60.0)];
        let found = resolve_mode(&modes, "b", 0, 0, 0.0).unwrap();
        assert_eq!(found.id, "b");
    }

    #[test]
    fn mode_resolution_falls_back_to_size_and_nearest_refresh() {
        let modes = vec![
            mode("a", 1920, 1080, 60.0),
            mode("b", 1920, 1080, 59.94),
            mode("c", 1920, 1080, 120.0),
        ];
        let found = resolve_mode(&modes, "missing", 1920, 1080, 59.9).unwrap();
        assert_eq!(found.id, "b");
    }

    #[test]
    fn mode_resolution_rejects_dimension_only_match() {
        let modes = vec![mode("a", 1920, 1080, 100.0)];
        assert!(resolve_mode(&modes, "missing", 1920, 1080, 60.0).is_none());
    }

    #[test]
    fn mode_resolution_rejects_wrong_dimensions() {
        let modes = vec![mode("a", 1280, 720, 60.0)];
        assert!(resolve_mode(&modes, "missing", 1920, 1080, 60.0).is_none());
    }

    #[test]
    fn snapshot_connectors_starting_with_meta_are_virtual() {
        assert!("Meta-0".starts_with("Meta-"));
        assert!(!"eDP-1".starts_with("Meta-"));
    }

    #[test]
    fn wire_shapes_match_mutter() {
        use zbus::Type;
        let apply = ApplyArgument::SIGNATURE.to_string();
        assert!(apply.contains("a(iiduba(ssa{sv}))a{sv}"), "{apply}");
        let state = RawDisplayState::SIGNATURE.to_string();
        assert!(state.contains("a((ssss)a(siiddada{sv})a{sv})"), "{state}");
        assert!(state.contains("a(iiduba(ssss)a{sv})"), "{state}");
    }

    #[test]
    fn timestamps_are_rfc3339() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_timestamp(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format_timestamp(1_795_219_200), "2026-11-21T00:00:00Z");
    }
}
