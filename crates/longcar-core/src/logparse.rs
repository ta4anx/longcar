// SPDX-License-Identifier: GPL-3.0-or-later

//! Event extraction from Sunshine's per-instance log.

use std::sync::LazyLock;

use regex::Regex;

/// Meaningful events found in a Sunshine log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent {
    Connected,
    Disconnected,
    SessionStarted,
    SessionEnded,
    /// Negotiated stream bitrate in bits per second.
    Bitrate(u64),
    RequestedFps(u32),
    Encoder(String),
    Size(u32, u32),
    PortalTokenSaved,
    PortalTokenStale,
    /// A portal `SelectSources` failure, with the original line.
    PortalError(String),
    /// A fatal error or detected hang, with the original line.
    Fatal(String),
}

const PORTAL_TOKEN_SAVED: &str = "[portalgrab] Saved portal restore token";
const PORTAL_TOKEN_STALE: &str = "[portalgrab] Portal returned no streams for the saved restore token; discarding the stale token";

fn capture_u64(regex: &Regex, line: &str) -> Option<u64> {
    regex.captures(line)?.get(1)?.as_str().parse().ok()
}

/// Sunshine logs this while the desktop picker is open; it is not a failure.
const PORTAL_AWAITING_PERMISSION: &str = "Portal capture is awaiting user permission";

/// Classify one log line. More severe events win over generic ones.
pub fn parse_line(line: &str) -> Option<LogEvent> {
    if line.contains(PORTAL_AWAITING_PERMISSION) {
        // The capture client reports this as `Fatal:` while the picker waits for the
        // user; the screen must keep waiting for the portal token, not roll back.
        return Some(LogEvent::PortalError(line.to_string()));
    }
    if line.contains("Hang detected!") || line.contains("fatal") || line.contains("Fatal") {
        return Some(LogEvent::Fatal(line.to_string()));
    }
    if line.contains("[portalgrab]") && line.contains("SelectSources") {
        return Some(LogEvent::PortalError(line.to_string()));
    }
    if line.contains(PORTAL_TOKEN_STALE) {
        return Some(LogEvent::PortalTokenStale);
    }
    if line.contains(PORTAL_TOKEN_SAVED) {
        return Some(LogEvent::PortalTokenSaved);
    }

    static BITRATE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"Streaming bitrate is (\d+)").expect("bitrate regex"));
    static FPS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"Requested frame rate \[(\d+)fps\]").expect("fps regex"));
    static ENCODER_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"Creating encoder \[([^\]]+)\]").expect("encoder regex"));
    static SIZE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"Size: (\d+)x(\d+)").expect("size regex"));

    if let Some(bps) = capture_u64(&BITRATE_RE, line) {
        return Some(LogEvent::Bitrate(bps));
    }
    if let Some(fps) = capture_u64(&FPS_RE, line).and_then(|fps| u32::try_from(fps).ok()) {
        return Some(LogEvent::RequestedFps(fps));
    }
    if let Some(caps) = ENCODER_RE.captures(line) {
        return Some(LogEvent::Encoder(caps.get(1)?.as_str().to_string()));
    }
    if let Some(caps) = SIZE_RE.captures(line) {
        let width = caps.get(1)?.as_str().parse().ok()?;
        let height = caps.get(2)?.as_str().parse().ok()?;
        return Some(LogEvent::Size(width, height));
    }

    if line.contains("CLIENT CONNECTED") {
        return Some(LogEvent::Connected);
    }
    if line.contains("CLIENT DISCONNECTED") {
        return Some(LogEvent::Disconnected);
    }
    if line.contains("New streaming session started") {
        return Some(LogEvent::SessionStarted);
    }
    if line.contains("Session ended") {
        return Some(LogEvent::SessionEnded);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_lines_map_to_events() {
        let fixture: &[(&str, LogEvent)] = &[
            (
                "[2026-11-21 12:00:00.000]: Info: CLIENT CONNECTED",
                LogEvent::Connected,
            ),
            (
                "[2026-11-21 12:00:01.000]: Info: CLIENT DISCONNECTED",
                LogEvent::Disconnected,
            ),
            (
                "[2026-11-21 12:00:02.000]: Info: New streaming session started",
                LogEvent::SessionStarted,
            ),
            (
                "[2026-11-21 12:00:03.000]: Info: Session ended",
                LogEvent::SessionEnded,
            ),
            (
                "[2026-11-21 12:00:04.000]: Info: Streaming bitrate is 24000000",
                LogEvent::Bitrate(24_000_000),
            ),
            (
                "[2026-11-21 12:00:05.000]: Info: Requested frame rate [120fps]",
                LogEvent::RequestedFps(120),
            ),
            (
                "[2026-11-21 12:00:06.000]: Info: Creating encoder [vaapi]",
                LogEvent::Encoder("vaapi".to_string()),
            ),
            (
                "[2026-11-21 12:00:07.000]: Info: Size: 2560x1600",
                LogEvent::Size(2560, 1600),
            ),
            (
                "[2026-11-21 12:00:08.000]: Info: [portalgrab] Saved portal restore token",
                LogEvent::PortalTokenSaved,
            ),
            (
                "[2026-11-21 12:00:09.000]: Info: [portalgrab] Portal returned no streams for the saved restore token; discarding the stale token",
                LogEvent::PortalTokenStale,
            ),
            (
                "[2026-11-21 12:00:10.000]: Error: [portalgrab] SelectSources failed: response was cancelled",
                LogEvent::PortalError(
                    "[2026-11-21 12:00:10.000]: Error: [portalgrab] SelectSources failed: response was cancelled"
                        .to_string(),
                ),
            ),
            (
                "[2026-11-21 12:00:11.000]: Info: Hang detected! dumping thread stacks",
                LogEvent::Fatal(
                    "[2026-11-21 12:00:11.000]: Info: Hang detected! dumping thread stacks"
                        .to_string(),
                ),
            ),
        ];
        for (line, expected) in fixture {
            assert_eq!(parse_line(line).as_ref(), Some(expected), "line: {line}");
        }
    }

    #[test]
    fn noise_is_ignored() {
        for line in [
            "",
            "Info: Configuration loaded",
            "Info: Sunshine version: v2026.1201.120214",
            "Debug: listening on port 48049",
            "Info: [portalgrab] restoring previous stream",
        ] {
            assert_eq!(parse_line(line), None, "line: {line}");
        }
    }

    #[test]
    fn portal_permission_wait_is_not_fatal() {
        let line = "[2026-11-21 01:33:22.794]: Fatal: Portal capture is awaiting user permission. The current session will attempt to use a fallback capture method.";
        assert!(matches!(parse_line(line), Some(LogEvent::PortalError(_))));
    }

    #[test]
    fn capitalised_fatal_is_detected() {
        let line = "[2026-11-21 12:00:11.000]: Fatal: encoder failed";
        assert!(matches!(parse_line(line), Some(LogEvent::Fatal(_))));
    }

    #[test]
    fn fatal_wins_over_connection_event() {
        assert_eq!(
            parse_line("CLIENT CONNECTED then fatal: encoder crashed"),
            Some(LogEvent::Fatal(
                "CLIENT CONNECTED then fatal: encoder crashed".to_string()
            ))
        );
    }

    #[test]
    fn bitrate_accepts_large_values() {
        assert_eq!(
            parse_line("Streaming bitrate is 200000000000"),
            Some(LogEvent::Bitrate(200_000_000_000))
        );
    }
}
