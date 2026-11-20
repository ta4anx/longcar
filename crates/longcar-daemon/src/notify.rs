// SPDX-License-Identifier: GPL-3.0-or-later

//! Desktop notifications with de-duplication.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const DEDUPE_WINDOW: Duration = Duration::from_secs(5);

/// Sends user-facing notifications through the desktop notification service.
///
/// Sending happens on a blocking worker thread; the caller never waits and
/// failures are ignored, so a missing notification daemon cannot break a
/// screen transition.
#[derive(Debug)]
pub struct Notifier {
    enabled: AtomicBool,
    recent: Mutex<HashMap<String, Instant>>,
}

impl Notifier {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            recent: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Whether a notification with `key` should be delivered now.
    ///
    /// Returns `false` when notifications are disabled or the same key fired
    /// within the last five seconds; otherwise records the attempt.
    pub fn should_send(&self, key: &str) -> bool {
        if !self.enabled.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        let mut recent = self
            .recent
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(last) = recent.get(key)
            && now.duration_since(*last) < DEDUPE_WINDOW
        {
            return false;
        }
        recent.insert(key.to_string(), now);
        true
    }

    /// Send a notification unless disabled or de-duplicated.
    pub fn notify(&self, key: &str, summary: &str, body: &str) {
        if !self.should_send(key) {
            return;
        }
        let summary = summary.to_string();
        let body = body.to_string();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn_blocking(move || {
            let _ = notify_rust::Notification::new()
                .summary(&summary)
                .body(&body)
                .appname("Longcar")
                .icon("io.github.ta4anx.Longcar")
                .show();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_never_sends() {
        let notifier = Notifier::new(false);
        assert!(!notifier.should_send("portal"));
    }

    #[test]
    fn same_key_within_window_is_deduped() {
        let notifier = Notifier::new(true);
        assert!(notifier.should_send("portal"));
        assert!(!notifier.should_send("portal"));
        // Different keys are independent.
        assert!(notifier.should_send("connected"));
    }

    #[test]
    fn re_enabling_restores_sending() {
        let notifier = Notifier::new(false);
        assert!(!notifier.should_send("layout"));
        notifier.set_enabled(true);
        assert!(notifier.should_send("layout"));
    }
}
