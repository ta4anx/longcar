// SPDX-License-Identifier: GPL-3.0-or-later

//! Errors surfaced over D-Bus, mapped to the contract's error names.

use longcar_core::model::StageError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DaemonError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    InvalidArgs(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Sunshine(String),
    #[error("{0}")]
    Mutter(String),
    #[error("{0}")]
    Portal(String),
    #[error("{0}")]
    Io(String),
}

impl DaemonError {
    /// D-Bus error name, per `io.github.ta4anx.Longcar.Daemon.Error.<Kind>`.
    pub fn error_name(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "io.github.ta4anx.Longcar.Daemon.Error.NotFound",
            Self::InvalidArgs(_) => "io.github.ta4anx.Longcar.Daemon.Error.InvalidArgs",
            Self::Conflict(_) => "io.github.ta4anx.Longcar.Daemon.Error.Conflict",
            Self::Unavailable(_) => "io.github.ta4anx.Longcar.Daemon.Error.Unavailable",
            Self::Sunshine(_) => "io.github.ta4anx.Longcar.Daemon.Error.Sunshine",
            Self::Mutter(_) => "io.github.ta4anx.Longcar.Daemon.Error.Mutter",
            Self::Portal(_) => "io.github.ta4anx.Longcar.Daemon.Error.Portal",
            Self::Io(_) => "io.github.ta4anx.Longcar.Daemon.Error.Io",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NotFound(m)
            | Self::InvalidArgs(m)
            | Self::Conflict(m)
            | Self::Unavailable(m)
            | Self::Sunshine(m)
            | Self::Mutter(m)
            | Self::Portal(m)
            | Self::Io(m) => m.clone(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidArgs(message.into())
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }
}

impl From<StageError> for DaemonError {
    fn from(err: StageError) -> Self {
        use longcar_core::model::Stage;
        let message = format!("{}: {}", stage_label(err.stage), err.message);
        match err.stage {
            Stage::Validate => Self::InvalidArgs(message),
            Stage::MutterSession | Stage::RecordVirtual | Stage::PipeWireNode | Stage::Layout => {
                Self::Mutter(message)
            }
            Stage::PortalAuth => Self::Portal(message),
            Stage::SunshineSpawn | Stage::SunshineApi => Self::Sunshine(message),
            Stage::Pairing | Stage::Api => Self::Sunshine(message),
            Stage::Io => Self::Io(message),
        }
    }
}

fn stage_label(stage: longcar_core::model::Stage) -> &'static str {
    use longcar_core::model::Stage;
    match stage {
        Stage::Validate => "validation",
        Stage::MutterSession => "mutter session",
        Stage::RecordVirtual => "virtual output",
        Stage::PipeWireNode => "pipewire node",
        Stage::Layout => "layout",
        Stage::SunshineSpawn => "sunshine start",
        Stage::SunshineApi => "sunshine api",
        Stage::PortalAuth => "portal",
        Stage::Pairing => "pairing",
        Stage::Api => "api",
        Stage::Io => "io",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use longcar_core::model::Stage;

    #[test]
    fn every_error_has_a_contract_name() {
        for (err, expected) in [
            (
                DaemonError::NotFound("x".into()),
                "io.github.ta4anx.Longcar.Daemon.Error.NotFound",
            ),
            (
                DaemonError::InvalidArgs("x".into()),
                "io.github.ta4anx.Longcar.Daemon.Error.InvalidArgs",
            ),
            (
                DaemonError::Conflict("x".into()),
                "io.github.ta4anx.Longcar.Daemon.Error.Conflict",
            ),
            (
                DaemonError::Unavailable("x".into()),
                "io.github.ta4anx.Longcar.Daemon.Error.Unavailable",
            ),
            (
                DaemonError::Sunshine("x".into()),
                "io.github.ta4anx.Longcar.Daemon.Error.Sunshine",
            ),
            (
                DaemonError::Mutter("x".into()),
                "io.github.ta4anx.Longcar.Daemon.Error.Mutter",
            ),
            (
                DaemonError::Portal("x".into()),
                "io.github.ta4anx.Longcar.Daemon.Error.Portal",
            ),
            (
                DaemonError::Io("x".into()),
                "io.github.ta4anx.Longcar.Daemon.Error.Io",
            ),
        ] {
            assert_eq!(err.error_name(), expected);
            assert_eq!(err.message(), "x");
        }
    }

    #[test]
    fn stage_errors_map_to_categories() {
        let err: DaemonError = StageError::new(Stage::PortalAuth, "timeout").into();
        assert_eq!(
            err.error_name(),
            DaemonError::Portal(String::new()).error_name()
        );
        assert!(err.message().contains("timeout"));
        let err: DaemonError = StageError::new(Stage::Layout, "overlap").into();
        assert_eq!(
            err.error_name(),
            DaemonError::Mutter(String::new()).error_name()
        );
    }
}
