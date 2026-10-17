//! The Run Log (ADR-0016, S4): one per mode, the newest Log Channel lines and the lines the
//! Desktop Core writes itself, kept across a webview reload. Each push or clear yields the
//! `LogBatch` a view appends; `get_log` reads the whole log back.

use crate::text::is_js_whitespace;
use mkv_core::log_line::{LogKind, LogLine};
use mkv_core::pipeline::PipelineMode;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::Display;

/// How many lines each Run Log keeps: the oldest go first.
pub const RUN_LOG_CAP: usize = 2000;

/// A line as a console shows it: `display` is `text` with paths Finder-style, left out when it
/// is `text` unchanged. Copy uses `text`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShownLine {
    pub kind: LogKind,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

impl From<&LogLine> for ShownLine {
    fn from(line: &LogLine) -> Self {
        let display = log_display_text(&line.text);
        Self {
            kind: line.kind,
            display: (display != line.text).then(|| display.into_owned()),
            text: line.text.clone(),
        }
    }
}

/// An error line the Desktop Core writes itself: `ERROR: {msg}`, the engine's convention.
pub fn error_line(msg: impl Display) -> LogLine {
    LogLine::error(format!("ERROR: {msg}"))
}

/// What a push or a clear appends to a mode's console (the `job-log` event). A view whose
/// `generation` is older clears first, then drops every line it holds below `keep_from`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogBatch {
    pub mode: PipelineMode,
    pub generation: u64,
    /// The absolute index of this batch's first line.
    pub first: u64,
    /// The absolute index of the oldest line the log still keeps.
    pub keep_from: u64,
    /// How many kept lines are errors: the Log button's badge.
    pub errors: usize,
    pub lines: Vec<ShownLine>,
}

/// The whole of a mode's Run Log, for `get_log`: its lines start at `first`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogView {
    pub mode: PipelineMode,
    pub generation: u64,
    pub first: u64,
    pub errors: usize,
    pub lines: Vec<ShownLine>,
}

/// A mode's Run Log.
#[derive(Debug)]
pub struct RunLog {
    mode: PipelineMode,
    lines: VecDeque<ShownLine>,
    /// The absolute index of the oldest kept line.
    first: u64,
    generation: u64,
    errors: usize,
    cap: usize,
}

impl RunLog {
    pub fn new(mode: PipelineMode) -> Self {
        Self::with_cap(mode, RUN_LOG_CAP)
    }

    pub fn with_cap(mode: PipelineMode, cap: usize) -> Self {
        Self {
            mode,
            lines: VecDeque::new(),
            first: 0,
            generation: 0,
            errors: 0,
            cap,
        }
    }

    /// Appends `lines`, dropping the oldest beyond the cap, and returns what a view appends: the
    /// pushed lines still kept, so a push larger than the cap sends only its own newest.
    pub fn push(&mut self, lines: &[LogLine]) -> LogBatch {
        let end = self.first + self.lines.len() as u64;
        for line in lines {
            self.errors += usize::from(line.kind == LogKind::Error);
            self.lines.push_back(line.into());
        }
        while self.lines.len() > self.cap {
            if let Some(evicted) = self.lines.pop_front() {
                self.errors -= usize::from(evicted.kind == LogKind::Error);
            }
            self.first += 1;
        }
        let first = end.max(self.first);
        let skip = (first - self.first) as usize;
        self.batch(first, self.lines.range(skip..).cloned().collect())
    }

    /// Empties the log under a new generation, which tells a view to clear its console.
    pub fn clear(&mut self) -> LogBatch {
        self.lines.clear();
        self.first = 0;
        self.errors = 0;
        self.generation += 1;
        self.batch(0, Vec::new())
    }

    pub fn view(&self) -> LogView {
        LogView {
            mode: self.mode,
            generation: self.generation,
            first: self.first,
            errors: self.errors,
            lines: self.lines.iter().cloned().collect(),
        }
    }

    fn batch(&self, first: u64, lines: Vec<ShownLine>) -> LogBatch {
        LogBatch {
            mode: self.mode,
            generation: self.generation,
            first,
            keep_from: self.first,
            errors: self.errors,
            lines,
        }
    }
}

fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

/// A log line as the console shows it: a path's leading separator is dropped and every other one
/// becomes ` › `. A run of separators counts as one, and a run between two digits stays as
/// written, so `3/12` and dates read as they were. The port of the webview's former
/// `logDisplayText`.
///
/// Known limit: this works on the whole line, not on parsed paths, so a folder named `2` followed
/// by `01.mkv` keeps its slash, and a codec ID such as `S_TEXT/ASS` gets a chevron.
pub fn log_display_text(text: &str) -> Cow<'_, str> {
    // The common line holds no separator: nothing to rewrite, nothing to allocate.
    if !text.contains(is_separator) {
        return Cow::Borrowed(text);
    }
    let chars: Vec<char> = text.chars().collect();
    // Each maximal run of separators, as `(start, end)` char indices.
    let mut runs = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        if is_separator(chars[at]) {
            let start = at;
            while at < chars.len() && is_separator(chars[at]) {
                at += 1;
            }
            runs.push((start, at));
        } else {
            at += 1;
        }
    }
    // First pass: a run at the start or after whitespace, before a character that is neither, is
    // a leading separator and goes.
    let mut stripped = Vec::with_capacity(chars.len());
    let mut next_run = runs.iter().peekable();
    let mut at = 0;
    while at < chars.len() {
        if let Some(&&(start, end)) = next_run.peek().filter(|run| run.0 == at) {
            next_run.next();
            let after_space = start == 0 || is_js_whitespace(chars[start - 1]);
            let before_name = chars.get(end).is_some_and(|&c| !is_js_whitespace(c));
            if !(after_space && before_name) {
                stripped.extend_from_slice(&chars[start..end]);
            }
            at = end;
        } else {
            stripped.push(chars[at]);
            at += 1;
        }
    }
    // Second pass: every other run becomes a chevron, unless it sits between two digits.
    let digit = |at: Option<&char>| at.is_some_and(char::is_ascii_digit);
    let mut shown = String::with_capacity(text.len());
    let mut at = 0;
    while at < stripped.len() {
        if !is_separator(stripped[at]) {
            shown.push(stripped[at]);
            at += 1;
            continue;
        }
        let start = at;
        while at < stripped.len() && is_separator(stripped[at]) {
            at += 1;
        }
        let before = start.checked_sub(1).and_then(|i| stripped.get(i));
        if digit(before) && digit(stripped.get(at)) {
            shown.extend(&stripped[start..at]);
        } else {
            shown.push_str(" › ");
        }
    }
    Cow::Owned(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REMUX: PipelineMode = PipelineMode::Remux;

    fn texts(lines: &[ShownLine]) -> Vec<&str> {
        lines.iter().map(|line| line.text.as_str()).collect()
    }

    fn notes(texts: &[&str]) -> Vec<LogLine> {
        texts.iter().map(|text| LogLine::note(*text)).collect()
    }

    /// The webview's former `pushCapped` cases: the newest lines are kept across single and batch
    /// pushes, and a batch larger than the cap keeps only its own newest.
    #[test]
    fn test_the_log_keeps_the_newest_lines_across_single_and_batch_pushes() {
        let mut log = RunLog::with_cap(REMUX, 4);
        for (at, text) in ["a", "b", "c"].into_iter().enumerate() {
            let batch = log.push(&notes(&[text]));
            assert_eq!((batch.first, batch.keep_from), (at as u64, 0));
        }
        assert_eq!(texts(&log.view().lines), ["a", "b", "c"]);

        let batch = log.push(&notes(&["d", "e"]));
        assert_eq!((batch.first, batch.keep_from), (3, 1));
        assert_eq!(texts(&batch.lines), ["d", "e"]);
        assert_eq!(texts(&log.view().lines), ["b", "c", "d", "e"]);

        log.push(&notes(&["f"]));
        assert_eq!(texts(&log.view().lines), ["c", "d", "e", "f"]);

        let batch = log.push(&notes(&["g", "h", "i", "j", "k"]));
        assert_eq!(texts(&batch.lines), ["h", "i", "j", "k"]);
        assert_eq!((batch.first, batch.keep_from), (7, 7));
        let view = log.view();
        assert_eq!(texts(&view.lines), ["h", "i", "j", "k"]);
        assert_eq!(view.first, 7);

        // A cap of 0 keeps nothing.
        let mut none = RunLog::with_cap(REMUX, 0);
        let batch = none.push(&notes(&["a"]));
        assert!(batch.lines.is_empty());
        assert_eq!(batch.keep_from, 1);
        assert!(none.view().lines.is_empty());
    }

    #[test]
    fn test_the_error_count_follows_the_lines_kept_through_evictions_and_clears() {
        let mut log = RunLog::with_cap(REMUX, 3);
        log.push(&[LogLine::error("ERROR: one"), LogLine::note("two")]);
        // A warning or a line that merely says ERROR is not an error.
        let batch = log.push(&[LogLine::warning("ERROR-ish"), LogLine::note("ERROR: no")]);
        assert_eq!(batch.errors, 0);
        assert_eq!(log.view().errors, 0);
        log.push(&[LogLine::error("ERROR: three")]);
        assert_eq!(log.view().errors, 1);

        let batch = log.clear();
        assert_eq!(
            (batch.generation, batch.first, batch.keep_from, batch.errors),
            (1, 0, 0, 0)
        );
        assert!(batch.lines.is_empty());
        let batch = log.push(&[LogLine::error("ERROR: again")]);
        assert_eq!((batch.generation, batch.first, batch.errors), (1, 0, 1));
        assert_eq!(log.view().generation, 1);
    }

    #[test]
    fn test_the_log_shows_chevrons_for_path_separators_and_keeps_digit_fractions() {
        let raw = "Remuxed: /media/anime/Season 1/Show - 01.mkv (3/12)";
        assert_eq!(
            log_display_text(raw),
            "Remuxed: media › anime › Season 1 › Show - 01.mkv (3/12)"
        );
        assert_eq!(log_display_text("Output/Show.mkv"), "Output › Show.mkv");
        assert_eq!(
            log_display_text("C:\\Anime\\Show.mkv"),
            "C: › Anime › Show.mkv"
        );
        assert!(matches!(
            log_display_text("no paths here"),
            Cow::Borrowed("no paths here")
        ));
        // A run of separators is one separator: between digits it stays whole, elsewhere one
        // chevron.
        assert_eq!(log_display_text("16/9 and 2//1"), "16/9 and 2//1");
        assert_eq!(
            log_display_text("a//b C:\\Anime\\ trailing/"),
            "a › b C: › Anime ›  trailing › "
        );
        // JavaScript's `\s`: U+FEFF is whitespace, U+0085 is not.
        assert_eq!(log_display_text("\u{feff}/a"), "\u{feff}a");
        assert_eq!(log_display_text("\u{85}/a"), "\u{85} › a");
        // A lone or trailing separator run after whitespace is not a leading one.
        assert_eq!(log_display_text("x / y"), "x  ›  y");
    }

    #[test]
    fn test_a_line_carries_its_display_only_when_it_differs() {
        let mut log = RunLog::new(REMUX);
        let batch = log.push(&[LogLine::note("plain"), LogLine::step("Remuxing /a/b.mkv")]);
        assert_eq!(
            serde_json::to_value(&batch).unwrap(),
            serde_json::json!({
                "mode": "remux",
                "generation": 0,
                "first": 0,
                "keep_from": 0,
                "errors": 0,
                "lines": [
                    { "kind": "note", "text": "plain" },
                    { "kind": "step", "text": "Remuxing /a/b.mkv", "display": "Remuxing a › b.mkv" },
                ]
            })
        );
        assert_eq!(
            serde_json::to_value(log.view()).unwrap(),
            serde_json::json!({
                "mode": "remux",
                "generation": 0,
                "first": 0,
                "errors": 0,
                "lines": serde_json::to_value(&batch.lines).unwrap()
            })
        );
    }
}
