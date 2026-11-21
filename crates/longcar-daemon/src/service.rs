// SPDX-License-Identifier: GPL-3.0-or-later

//! D-Bus service: `io.github.ta4anx.Longcar.Daemon` on the session bus.

use std::process::ExitCode;
use std::time::Duration;

use longcar_core::config::Config;
use longcar_core::model::{BUS_NAME, INTERFACE, OBJECT_PATH};
use longcar_core::paths;
use zbus::object_server::SignalEmitter;

use crate::error::DaemonError;
use crate::orchestrator::Orchestrator;

/// D-Bus error type: `io.github.ta4anx.Longcar.Daemon.Error.<Kind>`.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.ta4anx.Longcar.Daemon.Error")]
enum ServiceError {
    NotFound(String),
    InvalidArgs(String),
    Conflict(String),
    Unavailable(String),
    Sunshine(String),
    Mutter(String),
    Portal(String),
    Io(String),
}

impl From<DaemonError> for ServiceError {
    fn from(err: DaemonError) -> Self {
        match err {
            DaemonError::NotFound(m) => Self::NotFound(m),
            DaemonError::InvalidArgs(m) => Self::InvalidArgs(m),
            DaemonError::Conflict(m) => Self::Conflict(m),
            DaemonError::Unavailable(m) => Self::Unavailable(m),
            DaemonError::Sunshine(m) => Self::Sunshine(m),
            DaemonError::Mutter(m) => Self::Mutter(m),
            DaemonError::Portal(m) => Self::Portal(m),
            DaemonError::Io(m) => Self::Io(m),
        }
    }
}

struct Daemon {
    orchestrator: Orchestrator,
}

#[zbus::interface(name = "io.github.ta4anx.Longcar.Daemon.Control")]
impl Daemon {
    #[zbus(name = "GetState")]
    async fn get_state(&self) -> String {
        self.orchestrator.state_json()
    }

    #[zbus(name = "StartDefault")]
    async fn start_default(&self) -> Result<(), ServiceError> {
        self.orchestrator.start_default().await.map_err(Into::into)
    }

    #[zbus(name = "StartAll")]
    async fn start_all(&self) -> Result<(), ServiceError> {
        self.orchestrator.start_all().await.map_err(Into::into)
    }

    #[zbus(name = "StopAll")]
    async fn stop_all(&self) -> Result<(), ServiceError> {
        self.orchestrator.stop_all().await.map_err(Into::into)
    }

    #[zbus(name = "StartScreen")]
    async fn start_screen(&self, id: String) -> Result<(), ServiceError> {
        self.orchestrator.start_screen(id).await.map_err(Into::into)
    }

    #[zbus(name = "StopScreen")]
    async fn stop_screen(&self, id: String) -> Result<(), ServiceError> {
        self.orchestrator.stop_screen(id).await.map_err(Into::into)
    }

    #[zbus(name = "AddScreen")]
    async fn add_screen(&self, json: String) -> Result<String, ServiceError> {
        self.orchestrator
            .add_screen(&json)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "UpdateScreen")]
    async fn update_screen(&self, json: String) -> Result<(), ServiceError> {
        self.orchestrator
            .update_screen(&json)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "RemoveScreen")]
    async fn remove_screen(&self, id: String, purge: bool) -> Result<(), ServiceError> {
        self.orchestrator
            .remove_screen(&id, purge)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "ApplyPreset")]
    async fn apply_preset(&self, preset_id: String) -> Result<(), ServiceError> {
        self.orchestrator
            .apply_preset(&preset_id)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "SetPresets")]
    async fn set_presets(&self, json: String) -> Result<(), ServiceError> {
        self.orchestrator
            .set_presets(&json)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "AddQuickScreen")]
    async fn add_quick_screen(&self, kind: String) -> Result<String, ServiceError> {
        self.orchestrator
            .add_quick_screen(&kind)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "UpdateSettings")]
    async fn update_settings(&self, json: String) -> Result<(), ServiceError> {
        self.orchestrator
            .update_settings(&json)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "SetAudioScreen")]
    async fn set_audio_screen(&self, id: String) -> Result<(), ServiceError> {
        self.orchestrator
            .set_audio_screen(&id)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "SetScreenAudio")]
    async fn set_screen_audio(
        &self,
        screen_id: String,
        enabled: bool,
    ) -> Result<(), ServiceError> {
        self.orchestrator
            .set_screen_audio(&screen_id, enabled)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "SetScreenTouch")]
    async fn set_screen_touch(
        &self,
        screen_id: String,
        enabled: bool,
    ) -> Result<(), ServiceError> {
        self.orchestrator
            .set_screen_touch(&screen_id, enabled)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "SetScreenBitrate")]
    async fn set_screen_bitrate(&self, screen_id: String, kbps: u32) -> Result<(), ServiceError> {
        self.orchestrator
            .set_screen_bitrate(&screen_id, kbps)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "SetScreenLayout")]
    async fn set_screen_layout(
        &self,
        screen_id: String,
        position: String,
        relative_to: String,
        alignment: String,
    ) -> Result<(), ServiceError> {
        self.orchestrator
            .set_screen_layout(&screen_id, &position, &relative_to, &alignment)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "ListPairings")]
    async fn list_pairings(&self, screen_id: String) -> Result<String, ServiceError> {
        self.orchestrator
            .list_pairings(&screen_id)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "Pair")]
    async fn pair(
        &self,
        screen_id: String,
        pairing_id: String,
        pin: String,
        name: String,
    ) -> Result<bool, ServiceError> {
        self.orchestrator
            .pair(&screen_id, &pairing_id, &pin, &name)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "CancelPairing")]
    async fn cancel_pairing(
        &self,
        screen_id: String,
        pairing_id: String,
    ) -> Result<(), ServiceError> {
        self.orchestrator
            .cancel_pairing(&screen_id, &pairing_id)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "Unpair")]
    async fn unpair(&self, screen_id: String, uuid: String) -> Result<(), ServiceError> {
        self.orchestrator
            .unpair(&screen_id, &uuid)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "SetClientEnabled")]
    async fn set_client_enabled(
        &self,
        screen_id: String,
        uuid: String,
        enabled: bool,
    ) -> Result<(), ServiceError> {
        self.orchestrator
            .set_client_enabled(&screen_id, &uuid, enabled)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "DiscoverHosts")]
    async fn discover_hosts(&self) -> Result<String, ServiceError> {
        self.orchestrator.discover_hosts().await.map_err(Into::into)
    }

    #[zbus(name = "GetHistory")]
    async fn get_history(&self, limit: u32) -> Result<String, ServiceError> {
        self.orchestrator.history(limit).map_err(Into::into)
    }

    #[zbus(name = "GetLogs")]
    async fn get_logs(&self, limit: u32) -> Result<String, ServiceError> {
        self.orchestrator.logs(limit).map_err(Into::into)
    }

    #[zbus(name = "GetDoctor")]
    async fn get_doctor(&self) -> Result<String, ServiceError> {
        self.orchestrator.doctor().await.map_err(Into::into)
    }

    #[zbus(name = "RunDoctorFix")]
    async fn run_doctor_fix(&self, fix_id: String) -> Result<String, ServiceError> {
        self.orchestrator
            .run_doctor_fix(&fix_id)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "DoctorDisplayTest")]
    async fn doctor_display_test(&self) -> (bool, String) {
        self.orchestrator.doctor_display_test().await
    }

    #[zbus(name = "GetPreviewNode")]
    async fn get_preview_node(&self, screen_id: String) -> Result<u32, ServiceError> {
        self.orchestrator
            .preview_node(&screen_id)
            .await
            .map_err(Into::into)
    }

    #[zbus(name = "RestoreLayout")]
    async fn restore_layout(&self) -> Result<(), ServiceError> {
        self.orchestrator.restore_layout().await.map_err(Into::into)
    }

    #[zbus(name = "DiscardLayoutSnapshot")]
    async fn discard_layout_snapshot(&self) -> Result<(), ServiceError> {
        self.orchestrator
            .discard_layout_snapshot()
            .await
            .map_err(Into::into)
    }

    #[zbus(signal, name = "StateChanged")]
    async fn state_changed(emitter: &SignalEmitter<'_>, json: &str) -> zbus::Result<()>;
}

/// Run the daemon until SIGTERM/SIGINT.
pub async fn run() -> ExitCode {
    // GStreamer must be initialised before any pipeline is built.
    let _ = gstreamer::init();
    longcar_display::gst::init();

    let config_path = paths::config_path();
    let config = match Config::load_or_create(&config_path) {
        Ok((config, created)) => {
            if created {
                eprintln!(
                    "longcar-daemon: wrote default configuration to {}",
                    config_path.display()
                );
            }
            config
        }
        Err(err) => {
            eprintln!("longcar-daemon: cannot load configuration: {err}");
            return ExitCode::FAILURE;
        }
    };
    let _ = longcar_core::config::create_private_dir(&paths::config_dir());
    let _ = longcar_core::config::create_private_dir(&paths::state_dir());

    let orchestrator = Orchestrator::spawn(config).await;

    let connection = match zbus::Connection::session().await {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("longcar-daemon: cannot connect to the session bus: {err}");
            return ExitCode::FAILURE;
        }
    };

    let daemon = Daemon {
        orchestrator: orchestrator.clone(),
    };
    if let Err(err) = connection.object_server().at(OBJECT_PATH, daemon).await {
        eprintln!("longcar-daemon: cannot export {OBJECT_PATH}: {err}");
        return ExitCode::FAILURE;
    }

    let flags = zbus::fdo::RequestNameFlags::DO_NOT_QUEUE;
    match connection.request_name_with_flags(BUS_NAME, flags).await {
        Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {}
        Ok(_) => {
            eprintln!("longcar-daemon: {BUS_NAME} is already owned by another process");
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            eprintln!("longcar-daemon: cannot request {BUS_NAME}: {err}");
            return ExitCode::FAILURE;
        }
    }

    // Forward orchestrator state to the D-Bus signal.
    let mut receiver = orchestrator.subscribe();
    let emitter = match SignalEmitter::new(&connection, OBJECT_PATH) {
        Ok(emitter) => emitter,
        Err(err) => {
            eprintln!("longcar-daemon: cannot create signal emitter: {err}");
            return ExitCode::FAILURE;
        }
    };
    tokio::spawn(async move {
        while let Ok(json) = receiver.recv().await {
            if let Err(err) = Daemon::state_changed(&emitter, &json).await {
                eprintln!("longcar-daemon: cannot emit StateChanged: {err}");
            }
        }
    });

    eprintln!("longcar-daemon: serving {INTERFACE} on {OBJECT_PATH}");

    wait_for_shutdown().await;

    eprintln!("longcar-daemon: stopping screens");
    // Shutdown must be prompt: systemd kills us after 90 s, and a compositor or Sunshine
    // unit that refuses to answer must not turn a restart into a SIGKILL.
    let _ = tokio::time::timeout(Duration::from_secs(6), orchestrator.stop_all()).await;
    // Wait until the virtual outputs are really gone: exiting while a ScreenCast session
    // is still being torn down leaves the connector to be reaped later, which can remove
    // a freshly created output of the next daemon run.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let state = orchestrator.state();
        let busy = state.screens.iter().any(|screen| {
            screen.state.is_active() || screen.connector.is_some() || screen.node_id.is_some()
        });
        if !busy {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    eprintln!("longcar-daemon: shutdown complete");
    // Leave immediately: dropping the runtime waits for stray blocking tasks (mDNS
    // discovery, notifications) and can outlive the unit's stop timeout.
    std::process::exit(0);
}

async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        let mut int = match signal(SignalKind::interrupt()) {
            Ok(stream) => stream,
            Err(_) => {
                term.recv().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
