//! Minimal console [`Logger`] adapter — leveled records on stderr.

use cicada_core::ports::logger::{LogLevel, Logger};

/// Leveled stderr logger. Structured fields render as
/// `key=value` pairs after the message — enough for a CLI.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConsoleLogger {
    /// Minimum level emitted; quieter records are dropped.
    min_level: LogLevel,
}

impl ConsoleLogger {
    /// Log at `Info` and above.
    pub(crate) fn new() -> Self {
        Self {
            min_level: LogLevel::Info,
        }
    }
}

impl Logger for ConsoleLogger {
    fn log(&self, level: LogLevel, message: &str, fields: &[(&str, &str)]) {
        if level < self.min_level {
            return;
        }
        if fields.is_empty() {
            eprintln!("{level}: {message}");
        } else {
            let kv = fields
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!("{level}: {message} ({kv})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_drops_below_min_level_and_renders_fields() {
        let logger = ConsoleLogger::new();
        // Debug < Info — dropped entirely.
        logger.log(LogLevel::Debug, "quiet", &[]);
        // Info with no fields — plain rendering.
        logger.log(LogLevel::Info, "plain", &[]);
        // Fields render as key=value pairs.
        logger.log(LogLevel::Warn, "with fields", &[("calendar", "work")]);
        logger.log(LogLevel::Error, "two", &[("a", "1"), ("b", "2")]);
    }
}
