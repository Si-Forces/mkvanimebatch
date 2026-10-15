//! Log Channel lines, kept in a leaf module so `media_tool`, `remux` and `extract` can
//! build them without depending on `pipeline`, which re-exports both types.

use serde::Serialize;

/// What a Log Channel line reports, which decides how a console shows it. `Note` is the plain
/// default.
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

/// One Log Channel line: its kind and its text, which carries no kind marker of its own.
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
}
