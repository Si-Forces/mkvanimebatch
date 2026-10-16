//! Log Channel lines, kept in a leaf module so `media_tool`, `remux` and `extract` can
//! build them without depending on `pipeline`, which re-exports both types.

use serde::Serialize;

/// What a Log Channel line reports, which decides how a console shows it. `Note` is the plain
/// default. No engine code writes `Cache` lines; the Desktop Core's clear-cache line is their
/// only producer.
///
/// The serialized names are read by `desktop-tauri/ui/log-view.js`, whose `KIND_CLASS` must
/// list the same names (its Node test checks this file).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogKind {
    Error,
    Warning,
    Skipped,
    Succeeded,
    Step,
    Cache,
    #[default]
    Note,
}

/// One Log Channel line: its kind, which decides how the line is shown, and its text, which
/// rendering leaves unchanged. Error lines say "ERROR:" in their own text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogLine {
    pub kind: LogKind,
    pub text: String,
}

impl LogLine {
    pub fn new(kind: LogKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self::new(LogKind::Error, text)
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self::new(LogKind::Warning, text)
    }

    pub fn step(text: impl Into<String>) -> Self {
        Self::new(LogKind::Step, text)
    }

    pub fn note(text: impl Into<String>) -> Self {
        Self::new(LogKind::Note, text)
    }
}
