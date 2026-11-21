// SPDX-License-Identifier: GPL-3.0-or-later

//! One Sunshine instance per screen: on-disk layout, credentials, unit lifecycle,
//! API readiness and log tailing.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use longcar_core::config::{ScreenConfig, create_private_dir};
use longcar_core::logparse::{LogEvent, parse_line};
use longcar_core::model::{Stage, StageError};
use longcar_core::paths;
use longcar_core::ports::PortFamily;
use longcar_core::sunshine::{render_apps, render_config};
use longcar_core::sunshine_api::{ApiError, ApiFailure, Credentials, SunshineApi};
use rand::Rng;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use crate::systemd::{self, InheritedEnv, UnitSpec};

/// How long to wait for Sunshine's API to answer at all.
const API_READY_TIMEOUT: Duration = Duration::from_secs(120);
const API_POLL: Duration = Duration::from_millis(250);
/// How long to wait for the Sunshine log file to appear.
const LOG_APPEAR_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    #[error("{0}")]
    Stage(#[from] StageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl InstanceError {
    fn stage(stage: Stage, message: impl Into<String>) -> Self {
        Self::Stage(StageError::new(stage, message))
    }
}

/// Events the orchestrator consumes per instance.
#[derive(Debug, Clone)]
pub enum InstanceEvent {
    /// Parsed Sunshine log line.
    Log(LogEvent),
    /// Raw log line (for `GetLogs`).
    Line(String),
    /// The Sunshine log file did not appear in time.
    LogMissing,
    /// The tail task stopped reading (unit gone).
    LogClosed,
}

/// Sunshine credentials for one screen.
pub fn generate_password() -> String {
    let mut rng = rand::rng();
    (0..32)
        .map(|_| {
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
            ALPHABET[rng.random_range(0..ALPHABET.len())] as char
        })
        .collect()
}

/// Directories and files for one screen instance.
#[derive(Debug, Clone)]
pub struct InstanceLayout {
    pub dir: PathBuf,
}

impl InstanceLayout {
    pub fn new(screen_id: &str) -> Self {
        Self {
            dir: paths::instance_dir(screen_id),
        }
    }

    pub fn sunshine_dir(&self) -> PathBuf {
        self.dir.join("sunshine")
    }

    pub fn config_path(&self) -> PathBuf {
        self.sunshine_dir().join("sunshine.conf")
    }

    pub fn apps_path(&self) -> PathBuf {
        self.sunshine_dir().join("apps.json")
    }

    pub fn log_path(&self) -> PathBuf {
        self.sunshine_dir().join("sunshine.log")
    }

    pub fn credentials_path(&self) -> PathBuf {
        self.dir.join("credentials.json")
    }

    pub fn previous_log_path(&self) -> PathBuf {
        self.sunshine_dir().join("sunshine.log.longcar-prev")
    }

    /// Create `dir` (0700) and `sunshine/` (0700).
    pub fn ensure(&self) -> Result<(), std::io::Error> {
        create_private_dir(&self.dir)?;
        create_private_dir(&self.sunshine_dir())
    }

    /// Move the current Sunshine log out of the way so the new run starts empty.
    pub fn rotate_log(&self) -> Result<(), std::io::Error> {
        let log = self.log_path();
        if log.exists() {
            std::fs::rename(&log, self.previous_log_path())?;
        }
        Ok(())
    }

    /// True when a portal restore token file exists.
    pub fn portal_token_present(&self) -> bool {
        match std::fs::read_dir(self.sunshine_dir()) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with("portal_token")),
            Err(_) => false,
        }
    }

    /// Delete the stored portal restore token(s).
    pub fn remove_portal_tokens(&self) -> Result<(), std::io::Error> {
        let dir = self.sunshine_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(());
        };
        for entry in entries.filter_map(|e| e.ok()) {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("portal_token")
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    pub fn credentials(&self) -> Result<Credentials, InstanceError> {
        let text = std::fs::read_to_string(self.credentials_path())?;
        serde_json::from_str(&text).map_err(|e| {
            InstanceError::stage(
                Stage::Api,
                format!("credentials.json is not valid JSON: {e}"),
            )
        })
    }

    pub fn save_credentials(&self, creds: &Credentials) -> Result<(), InstanceError> {
        let body = serde_json::to_vec_pretty(creds)
            .map_err(|e| InstanceError::stage(Stage::Api, e.to_string()))?;
        write_private(&self.credentials_path(), &body)?;
        Ok(())
    }
}

/// Write `bytes` to `path` atomically (temp file + rename) with mode 0600.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// A running Sunshine instance for one screen.
pub struct SunshineInstance {
    pub screen_id: String,
    pub unit: String,
    pub layout: InstanceLayout,
    pub credentials: Credentials,
    pub web_port: u16,
    log_tx: mpsc::Sender<InstanceEvent>,
    log_stop: Option<tokio::sync::oneshot::Sender<()>>,
    log_task: Option<tokio::task::JoinHandle<()>>,
}

impl SunshineInstance {
    /// Full start sequence: dirs, config, credentials, unit, API readiness, log tail.
    pub async fn start(
        screen: &ScreenConfig,
        family: &PortFamily,
        events: mpsc::Sender<InstanceEvent>,
    ) -> Result<Self, InstanceError> {
        let layout = InstanceLayout::new(&screen.id);
        layout.ensure()?;
        layout.rotate_log()?;

        let config = render_config(screen, family.base, &layout.dir);
        let apps = render_apps(screen);
        write_private(&layout.config_path(), config.as_bytes())?;
        write_private(&layout.apps_path(), apps.as_bytes())?;

        let generated = Credentials {
            username: "longcar".to_string(),
            password: generate_password(),
        };
        let stored = layout.credentials().ok();
        let credentials = stored.clone().unwrap_or_else(|| generated.clone());
        // Persist before the API handshake so a crash mid-bootstrap can still recover.
        if stored.is_none() {
            layout.save_credentials(&credentials)?;
        }

        let bin = longcar_core::sunshine::detect_sunshine_binary()
            .map_err(|e| InstanceError::stage(Stage::SunshineSpawn, e.to_string()))?;
        let spec = UnitSpec {
            screen_id: screen.id.clone(),
            instance_dir: layout.dir.clone(),
            sunshine_bin: bin,
            env: InheritedEnv::from_process(),
        };
        let unit = systemd::start(&spec)
            .await
            .map_err(|e| InstanceError::stage(Stage::SunshineSpawn, e))?;

        let anon = SunshineApi::new(family.web, None);
        let outcome = wait_for_api(&anon, &layout, &credentials, unit.as_str(), family.web).await;
        let credentials = match outcome {
            Ok(creds) => creds,
            Err(e) => {
                let _ = systemd::stop(&unit).await;
                return Err(e);
            }
        };
        layout.save_credentials(&credentials)?;

        Ok(Self {
            screen_id: screen.id.clone(),
            unit,
            layout,
            credentials,
            web_port: family.web,
            log_tx: events,
            log_stop: None,
            log_task: None,
        })
    }

    /// Authenticated API client.
    pub fn api(&self) -> SunshineApi {
        SunshineApi::new(self.web_port, Some(self.credentials.clone()))
    }

    pub fn log_path(&self) -> PathBuf {
        self.layout.log_path()
    }

    pub fn portal_token_present(&self) -> bool {
        self.layout.portal_token_present()
    }

    pub fn remove_portal_tokens(&self) -> Result<(), InstanceError> {
        self.layout.remove_portal_tokens()?;
        Ok(())
    }

    /// Tail `sunshine.log` for the life of the unit, parsing events.
    pub fn spawn_log_tail(&mut self) {
        if self.log_task.is_some() {
            return;
        }
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let path = self.log_path();
        let tx = self.log_tx.clone();
        let unit = self.unit.clone();
        self.log_stop = Some(stop_tx);
        self.log_task = Some(tokio::spawn(async move {
            tail_log(path, unit, tx, &mut stop_rx).await;
        }));
    }

    /// Stop the unit and the tail task.
    pub async fn stop(mut self) {
        if let Some(stop) = self.log_stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.log_task.take() {
            task.abort();
        }
        let _ = systemd::stop(&self.unit).await;
    }

    /// Stop the unit without consuming the handle (used during rollback).
    pub async fn stop_unit(&self) {
        let _ = systemd::stop(&self.unit).await;
    }
}

/// Poll the API until it answers, then resolve credentials.
async fn wait_for_api(
    anon: &SunshineApi,
    layout: &InstanceLayout,
    credentials: &Credentials,
    unit: &str,
    web_port: u16,
) -> Result<Credentials, InstanceError> {
    let deadline = tokio::time::Instant::now() + API_READY_TIMEOUT;
    loop {
        match anon.config_locale().await {
            Ok(_) => break,
            Err(ApiFailure::Transport(err)) => {
                if tokio::time::Instant::now() >= deadline {
                    let journal = systemd::journal_tail(unit, 100).await;
                    return Err(InstanceError::stage(
                        Stage::SunshineApi,
                        format!("Sunshine API never became ready: {err}\n{journal}"),
                    ));
                }
                tokio::time::sleep(API_POLL).await;
            }
            Err(err) => {
                return Err(InstanceError::stage(
                    Stage::SunshineApi,
                    format!("Sunshine API is unreachable: {err}"),
                ));
            }
        }
    }

    // Decide from the protected endpoint: Sunshine redirects to /welcome until an
    // administrator exists, answers 401 to an unauthenticated caller once it does, and
    // 200 only for the right credentials.
    match anon.config().await {
        Err(ApiFailure::WelcomeRequired) => {
            anon.set_password(&credentials.username, &credentials.password)
                .await
                .map_err(|e| {
                    InstanceError::stage(
                        Stage::SunshineApi,
                        format!("cannot set API credentials: {e}"),
                    )
                })?;
            let authed = SunshineApi::new(web_port, Some(credentials.clone()));
            authed.config().await.map_err(|e| {
                InstanceError::stage(
                    Stage::SunshineApi,
                    format!("new API credentials were rejected: {e}"),
                )
            })?;
            Ok(credentials.clone())
        }
        Ok(_) => {
            // Credentials already work (or this build does not gate /api/config).
            Ok(credentials.clone())
        }
        Err(ApiFailure::CredentialsLost) | Err(ApiFailure::Http(ApiError { status: 401, .. })) => {
            if !layout.credentials_path().exists() {
                return Err(InstanceError::stage(
                    Stage::SunshineApi,
                    "Sunshine already has API credentials that Longcar does not know; reset the instance to recover",
                ));
            }
            let authed = SunshineApi::new(web_port, Some(credentials.clone()));
            match authed.config().await {
                Ok(_) => Ok(credentials.clone()),
                Err(_) => Err(InstanceError::stage(
                    Stage::SunshineApi,
                    "Sunshine rejected the stored API credentials; reset the instance to recover",
                )),
            }
        }
        Err(err) => Err(InstanceError::stage(
            Stage::SunshineApi,
            format!("Sunshine config check failed: {err}"),
        )),
    }
}

/// Tail the Sunshine log, reopening on truncation and reporting a missing file.
async fn tail_log(
    path: PathBuf,
    unit: String,
    tx: mpsc::Sender<InstanceEvent>,
    stop: &mut tokio::sync::oneshot::Receiver<()>,
) {
    let deadline = tokio::time::Instant::now() + LOG_APPEAR_TIMEOUT;
    loop {
        if path.exists() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = tx.send(InstanceEvent::LogMissing).await;
            let journal = systemd::journal_tail(&unit, 100).await;
            if !journal.is_empty() {
                for line in journal.lines() {
                    let _ = tx.send(InstanceEvent::Line(line.to_string())).await;
                }
            }
            return;
        }
        tokio::select! {
            _ = &mut *stop => return,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }

    let mut offset: u64 = 0;
    loop {
        let file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
        if len < offset {
            offset = 0; // truncated or rotated
        }
        let mut reader = tokio::io::BufReader::new(file);
        if offset > 0 {
            use tokio::io::AsyncSeekExt;
            let mut reader_file = reader.into_inner();
            if reader_file
                .seek(std::io::SeekFrom::Start(offset))
                .await
                .is_err()
            {
                offset = 0;
                continue;
            }
            reader = tokio::io::BufReader::new(reader_file);
        }
        let mut line = String::new();
        loop {
            line.clear();
            tokio::select! {
                _ = &mut *stop => return,
                result = reader.read_line(&mut line) => {
                    match result {
                        Ok(0) => break,
                        Ok(n) => {
                            offset += n as u64;
                            let trimmed = line.trim_end_matches(['\n', '\r']);
                            if trimmed.is_empty() {
                                continue;
                            }
                            if let Some(event) = parse_line(trimmed) {
                                let _ = tx.send(InstanceEvent::Log(event)).await;
                            }
                            let _ = tx.send(InstanceEvent::Line(trimmed.to_string())).await;
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        let _ = tx.send(InstanceEvent::LogClosed).await;
        tokio::select! {
            _ = &mut *stop => return,
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

/// Remove a screen's instance directory and recreate the empty layout.
pub async fn reset_instance(screen_id: &str) -> Result<(), InstanceError> {
    let unit = systemd::unit_name(screen_id);
    let _ = systemd::stop(&unit).await;
    let layout = InstanceLayout::new(screen_id);
    if layout.dir.exists() {
        std::fs::remove_dir_all(&layout.dir)?;
    }
    layout.ensure()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_is_32_alphanumeric() {
        let pw = generate_password();
        assert_eq!(pw.len(), 32);
        assert!(pw.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(pw, generate_password());
    }

    #[test]
    fn layout_paths_are_under_the_instance() {
        let layout = InstanceLayout::new("tablet");
        let dir = paths::instance_dir("tablet");
        assert_eq!(layout.dir, dir);
        assert_eq!(layout.config_path(), dir.join("sunshine/sunshine.conf"));
        assert_eq!(layout.apps_path(), dir.join("sunshine/apps.json"));
        assert_eq!(layout.log_path(), dir.join("sunshine/sunshine.log"));
        assert_eq!(layout.credentials_path(), dir.join("credentials.json"));
        assert_eq!(
            layout.previous_log_path(),
            dir.join("sunshine/sunshine.log.longcar-prev")
        );
    }

    fn temp_layout(tag: &str) -> InstanceLayout {
        let dir = std::env::temp_dir().join(format!(
            "longcar-instance-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        InstanceLayout { dir }
    }

    #[test]
    fn ensure_creates_private_dirs_and_rotate_moves_log() {
        let layout = temp_layout("rotate");
        layout.ensure().unwrap();
        let mode = std::fs::metadata(&layout.dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let mode = std::fs::metadata(layout.sunshine_dir())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);

        std::fs::write(layout.log_path(), "old run\n").unwrap();
        layout.rotate_log().unwrap();
        assert!(!layout.log_path().exists());
        assert_eq!(
            std::fs::read_to_string(layout.previous_log_path()).unwrap(),
            "old run\n"
        );

        // Rotating again with no current log keeps the previous one intact.
        layout.rotate_log().unwrap();
        assert_eq!(
            std::fs::read_to_string(layout.previous_log_path()).unwrap(),
            "old run\n"
        );
        let _ = std::fs::remove_dir_all(&layout.dir);
    }

    #[test]
    fn credentials_round_trip_private() {
        let layout = temp_layout("creds");
        layout.ensure().unwrap();
        let creds = Credentials {
            username: "longcar".to_string(),
            password: generate_password(),
        };
        layout.save_credentials(&creds).unwrap();
        let mode = std::fs::metadata(layout.credentials_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(layout.credentials().unwrap(), creds);
        let _ = std::fs::remove_dir_all(&layout.dir);
    }

    #[test]
    fn portal_token_detection_and_removal() {
        let layout = temp_layout("token");
        layout.ensure().unwrap();
        assert!(!layout.portal_token_present());
        std::fs::write(layout.sunshine_dir().join("portal_token_1"), "x").unwrap();
        assert!(layout.portal_token_present());
        layout.remove_portal_tokens().unwrap();
        assert!(!layout.portal_token_present());
        let _ = std::fs::remove_dir_all(&layout.dir);
    }
}
