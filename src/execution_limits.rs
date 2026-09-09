//! Central limits for child execution and temporary in-memory diagnostics.

use anyhow::{Context, Result, ensure};
use std::time::Duration;

pub const TRUNCATED: &str = "[output truncated: limit exceeded]";

#[derive(Debug, Clone, Copy)]
pub struct HistoryLimits {
    pub event_bytes: usize,
    pub log_events: usize,
    pub log_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            event_bytes: 4096,
            log_events: 256,
            log_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessLimits {
    pub timeout: Duration,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub event_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct ExecutionLimits {
    pub implementer_timeout: Duration,
    pub verification_timeout: Duration,
    pub git_timeout: Duration,
    pub recovery_retention: Duration,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub history: HistoryLimits,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            implementer_timeout: Duration::from_secs(1800),
            verification_timeout: Duration::from_secs(600),
            git_timeout: Duration::from_secs(300),
            recovery_retention: Duration::from_secs(86400),
            stdout_bytes: 64 * 1024,
            stderr_bytes: 64 * 1024,
            history: HistoryLimits::default(),
        }
    }
}

// Git protocol/status/diff output cannot be silently truncated and parsed.
// Hitting this cap makes capture fail and keeps the workspace for recovery.
pub const GIT_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub const TERMINATION_GRACE: Duration = Duration::from_secs(3);

impl ExecutionLimits {
    pub fn process(&self, timeout: Duration) -> ProcessLimits {
        ProcessLimits {
            timeout,
            stdout_bytes: self.stdout_bytes,
            stderr_bytes: self.stderr_bytes,
            event_bytes: self.history.event_bytes,
        }
    }

    pub fn git(&self) -> ProcessLimits {
        ProcessLimits {
            stdout_bytes: GIT_OUTPUT_BYTES,
            ..self.process(self.git_timeout)
        }
    }

    pub fn load() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let mut limits = Self::default();
        let number = |key: &str, default: usize| -> Result<usize> {
            let value = match get(key) {
                Some(raw) => raw
                    .parse::<usize>()
                    .with_context(|| format!("{key} must be a positive integer"))?,
                None => default,
            };
            ensure!(value > 0, "{key} must be positive");
            Ok(value)
        };
        limits.implementer_timeout =
            Duration::from_secs(number("IMPLEMENTER_TIMEOUT_SECS", 1800)? as u64);
        limits.verification_timeout =
            Duration::from_secs(number("VERIFICATION_TIMEOUT_SECS", 600)? as u64);
        limits.git_timeout = Duration::from_secs(number("GIT_TIMEOUT_SECS", 300)? as u64);
        limits.recovery_retention =
            Duration::from_secs(number("WORKSPACE_RECOVERY_SECS", 86400)? as u64);
        limits.stdout_bytes = number("PROCESS_STDOUT_BYTES", limits.stdout_bytes)?;
        limits.stderr_bytes = number("PROCESS_STDERR_BYTES", limits.stderr_bytes)?;
        limits.history.event_bytes = number("LOG_EVENT_BYTES", limits.history.event_bytes)?;
        limits.history.log_events = number("TASK_LOG_EVENTS", limits.history.log_events)?;
        limits.history.log_bytes = number("TASK_LOG_BYTES", limits.history.log_bytes)?;
        ensure!(
            limits.history.event_bytes >= TRUNCATED.len(),
            "LOG_EVENT_BYTES must fit the truncation marker"
        );
        Ok(limits)
    }
}

pub fn bounded_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit.saturating_sub(TRUNCATED.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED}", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_rejects_zero_and_invalid_values() {
        for value in ["0", "no", "-1"] {
            assert!(
                ExecutionLimits::from_lookup(
                    |key| (key == "IMPLEMENTER_TIMEOUT_SECS").then(|| value.into())
                )
                .is_err()
            );
        }
        let limits = ExecutionLimits::from_lookup(|key| {
            (key == "VERIFICATION_TIMEOUT_SECS").then(|| "1".into())
        })
        .unwrap();
        assert_eq!(limits.verification_timeout, Duration::from_secs(1));
    }
    #[test]
    fn payload_limits_preserve_utf8_and_small_text() {
        assert_eq!(bounded_text("hello", 40), "hello");
        let text = bounded_text(&"🦀".repeat(100), 40);
        assert!(text.len() <= 40);
        assert!(text.ends_with(TRUNCATED));
    }
}
