// SPDX-License-Identifier: GPL-3.0-or-later

//! Longcar daemon: orchestration, Sunshine instances and the D-Bus control plane.

pub mod doctor;
pub mod error;
pub mod instance;
pub mod logbuf;
pub mod notify;
pub mod orchestrator;
pub mod plan;
pub mod service;
pub mod speedtest;
pub mod systemd;

pub use orchestrator::Orchestrator;

/// Entry point used by the `longcar-daemon` binary.
pub async fn run() -> std::process::ExitCode {
    service::run().await
}
