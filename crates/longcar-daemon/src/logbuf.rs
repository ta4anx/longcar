// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded in-memory daemon log used by `GetLogs`.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Default number of retained log lines.
pub const DEFAULT_CAPACITY: usize = 2000;

/// Thread-safe ring buffer of the most recent daemon log lines.
#[derive(Debug)]
pub struct LogBuffer {
    capacity: usize,
    lines: Mutex<VecDeque<String>>,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            lines: Mutex::new(VecDeque::with_capacity(capacity.min(256))),
        }
    }

    /// Append one line, dropping the oldest lines beyond the capacity.
    pub fn push(&self, line: impl Into<String>) {
        let mut lines = self
            .lines
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if lines.len() >= self.capacity {
            lines.pop_front();
        }
        lines.push_back(line.into());
    }

    /// Newest `n` lines, oldest first, with credential-bearing lines redacted.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let lines = self
            .lines
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let take = n.min(lines.len());
        lines
            .iter()
            .skip(lines.len() - take)
            .map(|line| redact(line))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lines
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Replace a line that mentions a password with a fixed marker.
///
/// The rule is deliberately blunt: any case-insensitive occurrence of
/// `password` wins, so a value can never leak through ad-hoc key/value shapes.
pub fn redact(line: &str) -> String {
    if line.to_lowercase().contains("password") {
        "[redacted line: contains credentials]".to_string()
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_to_capacity_keeping_newest() {
        let buffer = LogBuffer::new(3);
        for line in ["a", "b", "c", "d", "e"] {
            buffer.push(line);
        }
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.tail(10), vec!["c", "d", "e"]);
    }

    #[test]
    fn tail_is_oldest_first_and_bounded() {
        let buffer = LogBuffer::new(10);
        for line in ["one", "two", "three"] {
            buffer.push(line);
        }
        assert_eq!(buffer.tail(2), vec!["two", "three"]);
        assert_eq!(buffer.tail(0), Vec::<String>::new());
    }

    #[test]
    fn redacts_password_lines() {
        let buffer = LogBuffer::new(4);
        buffer.push("password = hunter2");
        buffer.push("starting screen tablet");
        let tail = buffer.tail(10);
        assert_eq!(tail.len(), 2);
        assert!(!tail[0].contains("hunter2"));
        assert!(tail[0].contains("credentials"));
        assert_eq!(tail[1], "starting screen tablet");
    }

    #[test]
    fn redact_is_case_insensitive_and_multiline_safe() {
        assert!(redact("PASSWORD: s3cret").contains("credentials"));
        assert!(redact("PassWord = x").contains("credentials"));
        assert_eq!(redact("no secrets here"), "no secrets here");
        assert!(redact("x\npassword = y").contains("credentials"));
    }

    #[test]
    fn empty_buffer_tail_is_empty() {
        let buffer = LogBuffer::new(DEFAULT_CAPACITY);
        assert!(buffer.is_empty());
        assert_eq!(buffer.tail(5), Vec::<String>::new());
    }
}
