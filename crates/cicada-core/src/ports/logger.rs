//! `Logger` — structured, leveled logging (first adapter:
//! `tracing`).

/// Log level, ordered `Trace < Debug < Info < Warn < Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    /// Finest-grained diagnostics.
    Trace,
    /// Debug detail.
    Debug,
    /// Normal, noteworthy events (ops applied, skips).
    Info,
    /// Recoverable problems (capability warnings, drift).
    Warn,
    /// Failures.
    Error,
}

impl LogLevel {
    /// Lowercase name for rendering.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Structured leveled logging — fields are key/value string pairs so
/// adapters can forward them to `tracing` fields, JSON, or plain
/// output without core knowing which.
pub trait Logger: Send + Sync {
    /// Emit one record.
    fn log(&self, level: LogLevel, message: &str, fields: &[(&str, &str)]);

    /// `Info` with no fields.
    fn info(&self, message: &str) {
        self.log(LogLevel::Info, message, &[]);
    }

    /// `Warn` with no fields.
    fn warn(&self, message: &str) {
        self.log(LogLevel::Warn, message, &[]);
    }

    /// `Error` with no fields.
    fn error(&self, message: &str) {
        self.log(LogLevel::Error, message, &[]);
    }

    /// `Debug` with no fields.
    fn debug(&self, message: &str) {
        self.log(LogLevel::Debug, message, &[]);
    }

    /// `Trace` with no fields.
    fn trace(&self, message: &str) {
        self.log(LogLevel::Trace, message, &[]);
    }
}

/// Discards everything — tests and quiet contexts.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopLogger;

impl Logger for NoopLogger {
    fn log(&self, _level: LogLevel, _message: &str, _fields: &[(&str, &str)]) {}
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn level_names_and_display() {
        for (level, name) in [
            (LogLevel::Trace, "trace"),
            (LogLevel::Debug, "debug"),
            (LogLevel::Info, "info"),
            (LogLevel::Warn, "warn"),
            (LogLevel::Error, "error"),
        ] {
            assert_eq!(level.as_str(), name);
            assert_eq!(level.to_string(), name);
        }
    }

    #[test]
    fn convenience_methods_delegate_to_log() {
        let noop = NoopLogger;
        noop.info("i");
        noop.warn("w");
        noop.error("e");
        noop.debug("d");
        noop.trace("t");
        noop.log(LogLevel::Info, "direct", &[("k", "v")]);
    }
}
