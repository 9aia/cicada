//! Structured schema errors: what failed, where in the document.
//!
//! Every parse/lower failure is a [`SchemaError`] carrying the field
//! path (`spec.events[2].start`) and, once the CLI attaches it, the
//! file it came from — never a bare serde sprawl. Schema errors are
//! validation failures, so they map to [`CicadaError::Fatal`]
//! (ADR-0002 §8).

use std::fmt;
use std::path::{Path, PathBuf};

/// A YAML→domain failure with file and field-path context.
///
/// `path` uses dotted field syntax with bracketed indices
/// (`spec.events[0].start`); `line`/`column` come from the YAML
/// scanner for syntax errors that have no meaningful field path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError {
    /// Source file, when the caller knows it.
    pub file: Option<PathBuf>,
    /// Field path inside the document, when known.
    pub path: Option<String>,
    /// 1-based line in the source (syntax errors).
    pub line: Option<usize>,
    /// 1-based column in the source (syntax errors).
    pub column: Option<usize>,
    /// What went wrong.
    pub message: String,
}

impl SchemaError {
    /// An error tied to a field path.
    pub fn at(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            file: None,
            path: Some(path.into()),
            line: None,
            column: None,
            message: message.into(),
        }
    }

    /// An error with no field context (envelope, syntax).
    pub fn document(message: impl Into<String>) -> Self {
        Self {
            file: None,
            path: None,
            line: None,
            column: None,
            message: message.into(),
        }
    }

    /// A YAML syntax error, carrying the scanner location.
    pub fn syntax(err: &serde_yaml::Error) -> Self {
        let loc = err.location();
        Self {
            file: None,
            path: None,
            line: loc.as_ref().map(serde_yaml::Location::line),
            column: loc.as_ref().map(serde_yaml::Location::column),
            message: err.to_string(),
        }
    }

    /// Attach the file this error came from (the parser deals in
    /// strings; callers who know the path decorate the error).
    #[must_use]
    pub fn with_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.file = Some(file.into());
        self
    }
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{}:", Path::new(file).display())?;
        }
        if let (Some(l), Some(c)) = (self.line, self.column) {
            write!(f, "{l}:{c}: ")?;
        }
        if let Some(path) = &self.path {
            write!(f, "{path}: ")?;
        }
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SchemaError {}

impl From<SchemaError> for cicada_core::CicadaError {
    /// Schema errors are `Fatal` — validation is never retryable
    /// (ADR-0002 §8).
    fn from(e: SchemaError) -> Self {
        Self::fatal(e.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// A writer whose `write_str` always errors — drives the `?`
    /// arms of each conditional `write!` in `Display::fmt`.
    struct Fail;

    impl fmt::Write for Fail {
        fn write_str(&mut self, _s: &str) -> fmt::Result {
            Err(fmt::Error)
        }
    }

    #[test]
    fn display_propagates_write_failures() {
        // `file:` set — the first `write!` fails.
        let e = SchemaError::document("m").with_file("f.yaml");
        assert!(write!(Fail, "{e}").is_err());
        // Only line/column set — first write is skipped, second fails.
        let mut e = SchemaError::document("m");
        e.line = Some(3);
        e.column = Some(4);
        assert!(write!(Fail, "{e}").is_err());
        // Only the field path set — the third `write!` fails.
        let e = SchemaError::at("spec.events", "m");
        assert!(write!(Fail, "{e}").is_err());
        // Every piece present — and the `line` without `column`
        // edge of the `if let` guard.
        let mut e = SchemaError::at("spec.events", "m").with_file("f.yaml");
        e.line = Some(3);
        e.column = Some(4);
        assert_eq!(e.to_string(), "f.yaml:3:4: spec.events: m");
        e.column = None;
        assert_eq!(e.to_string(), "f.yaml:spec.events: m");
    }
}
