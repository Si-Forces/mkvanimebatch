//! The presentation values of the Desktop Core (ADR-0016 decision 3, S4c): the texts, tones and
//! numbers a view shows for a queue, derived here so a view only renders them. Pure functions;
//! the actor calls them when it builds a mode's view, with `now` on the monotonic clock. The
//! wording is the webview's before S4c, byte for byte.

use crate::actor::{ModeView, Outcome, ScanView};
use crate::control::mode_name;
use crate::queue::Row;
use crate::snapshot::{BatchStatus, ItemStatus, JobSnapshot};
use mkv_core::manifest::InspectionMetadata;
use mkv_core::pipeline::PipelineMode;
use mkv_core::request::EffectiveSettings;
use serde::Serialize;
use std::time::{Duration, Instant};

/// The status label and hover text of a skipped item the engine gave no reason for.
pub const DEFAULT_SKIPPED: &str = "Skipped";
/// The status label and hover text of a cancelled item the engine gave no reason for.
pub const DEFAULT_CANCELLED: &str = "Cancelled";
/// The status label of a failed item, and its error when the engine gave none.
pub const DEFAULT_FAILED: &str = "Failed";

/// A measured item duration: `<0.1s`, `4.2s`, `1m 15.4s`.
pub fn format_outcome_duration(seconds: f64) -> String {
    if seconds < 0.1 {
        return "<0.1s".into();
    }
    // Rounded to tenths first, so the one-decimal formatting below never meets an exact tie.
    let rounded = (seconds * 10.0).round() / 10.0;
    if rounded < 60.0 {
        return format!("{rounded:.1}s");
    }
    format!("{}m {:.1}s", (rounded / 60.0).floor(), rounded % 60.0)
}

/// An elapsed time in whole seconds: `5s`, `1m 15s`.
pub fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// The color token of an item status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Info,
    Ok,
    Err,
    Mute,
}

/// Running is blue, done green, failed red, the rest muted.
pub fn status_tone(status: ItemStatus) -> Tone {
    match status {
        ItemStatus::Processing => Tone::Info,
        ItemStatus::Completed => Tone::Ok,
        ItemStatus::Error => Tone::Err,
        _ => Tone::Mute,
    }
}

/// The status column text of a row: a processing item's progress and `elapsed` time (none
/// before it started reads `0s`), else its outcome.
pub fn status_label(
    status: ItemStatus,
    progress: Option<f64>,
    elapsed: Option<Duration>,
    duration_sec: Option<f64>,
) -> String {
    match status {
        ItemStatus::Processing => {
            let elapsed = format_elapsed(elapsed.unwrap_or_default());
            match progress {
                Some(progress) => format!("{}% · {elapsed}", progress.round()),
                None => format!("Processing · {elapsed}"),
            }
        }
        ItemStatus::Completed => match duration_sec {
            Some(duration) => format!("Done in {}", format_outcome_duration(duration)),
            None => "Done".into(),
        },
        ItemStatus::Skipped => DEFAULT_SKIPPED.into(),
        ItemStatus::Cancelled => DEFAULT_CANCELLED.into(),
        ItemStatus::Planned => "Planned".into(),
        ItemStatus::Error => DEFAULT_FAILED.into(),
        ItemStatus::Queued => "Queued".into(),
    }
}

/// The error a failed row shows: the engine's, else the default.
pub fn row_error(status: ItemStatus, error: Option<&str>) -> Option<&str> {
    error.or((status == ItemStatus::Error).then_some(DEFAULT_FAILED))
}

/// The reason a row shows: the engine's, else the default of a skipped or cancelled item.
pub fn row_reason(status: ItemStatus, reason: Option<&str>) -> Option<&str> {
    reason.or(match status {
        ItemStatus::Skipped => Some(DEFAULT_SKIPPED),
        ItemStatus::Cancelled => Some(DEFAULT_CANCELLED),
        _ => None,
    })
}

/// The first of `texts` that is set and not empty, else `fallback`: JavaScript's `a || b`.
pub(crate) fn first_text<'a>(texts: &[Option<&'a str>], fallback: &'a str) -> &'a str {
    texts
        .iter()
        .flatten()
        .find(|text| !text.is_empty())
        .copied()
        .unwrap_or(fallback)
}

/// A row's hover text: its error, else its reason, else none; none while it is processing.
pub fn status_title(status: ItemStatus, error: Option<&str>, reason: Option<&str>) -> String {
    if status == ItemStatus::Processing {
        return String::new();
    }
    first_text(&[row_error(status, error), row_reason(status, reason)], "").into()
}

/// The meta line of a failed row: why it failed.
pub fn failed_meta(error: Option<&str>) -> String {
    format!(
        "Failed · {}",
        first_text(&[row_error(ItemStatus::Error, error)], "Error")
    )
}

/// The meta line of a row the Job Snapshot lists but the manifest does not, so it has no
/// inspection to show. A failed one shows `failed_meta`. The defaults are laid over the reason
/// first, as the webview did, so a skipped item with no reason reads `Skipped · Skipped`.
pub fn job_only_meta(
    status: ItemStatus,
    duration_sec: Option<f64>,
    reason: Option<&str>,
    error: Option<&str>,
) -> String {
    let reason = row_reason(status, reason);
    match status {
        ItemStatus::Completed => match duration_sec {
            Some(duration) => format!("Processed · Done in {}", format_outcome_duration(duration)),
            None => "Processed".into(),
        },
        ItemStatus::Skipped => format!("Skipped · {}", first_text(&[reason], "Already processed")),
        ItemStatus::Cancelled => format!("Cancelled · {}", first_text(&[reason], "Cancelled")),
        ItemStatus::Error => failed_meta(error),
        _ => "Queued".into(),
    }
}

/// The meta line of an inspected manifest row: what a run writes for it (remux), or what it
/// holds (extract). `candidates` are its release's subtitle candidates; `assignment` is the
/// asset path of its Track Assignment; `bitrate` and `assets_dir` are the remux draft's, trimmed.
pub fn inspected_meta(
    remux: bool,
    meta: &InspectionMetadata,
    release: &str,
    candidates: &[String],
    assignment: Option<&str>,
    bitrate: &str,
    assets_dir: &str,
) -> String {
    let fonts = meta.fonts_count.unwrap_or(0);
    if remux {
        let ladder = subtitle_ladder(
            release,
            candidates,
            meta.sub_summary.as_deref(),
            assignment,
            assets_dir,
        );
        format!(
            "{} · {} · {fonts} fonts",
            remux_audio_text(Some(meta), bitrate),
            subtitle_source_text(&ladder)
        )
    } else {
        format!(
            "{} · {fonts} fonts",
            first_text(&[meta.sub_summary.as_deref()], "No subs")
        )
    }
}

/// The filter chip a row counts under; `None` for an unselected row, which only "all" shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Bucket {
    Todo,
    Done,
    Failed,
}

/// The filter chip of a row with `status`.
pub fn bucket(status: ItemStatus, selected: bool) -> Option<Bucket> {
    if !selected {
        return None;
    }
    Some(match status {
        ItemStatus::Queued | ItemStatus::Processing | ItemStatus::Cancelled => Bucket::Todo,
        ItemStatus::Completed | ItemStatus::Skipped | ItemStatus::Planned => Bucket::Done,
        ItemStatus::Error => Bucket::Failed,
    })
}

/// How many rows each filter chip shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Filters {
    pub all: usize,
    pub todo: usize,
    pub done: usize,
    pub failed: usize,
}

pub fn filter_counts(rows: &[Row]) -> Filters {
    let mut counts = Filters {
        all: rows.len(),
        ..Filters::default()
    };
    for row in rows {
        match row.bucket {
            Some(Bucket::Todo) => counts.todo += 1,
            Some(Bucket::Done) => counts.done += 1,
            Some(Bucket::Failed) => counts.failed += 1,
            None => {}
        }
    }
    counts
}

/// A path's segments as a Finder path bar shows them: split on `/` or `\`, without empty, `.`
/// and `..` segments. Display only: never compare or send the result.
pub fn path_segments(path: &str) -> Vec<&str> {
    path.split(['/', '\\'])
        .filter(|segment| !matches!(*segment, "" | "." | ".."))
        .collect()
}

/// A path as one line of Finder-style text: `media › anime › Show`.
pub fn pretty_path(path: &str) -> String {
    path_segments(path).join(" › ")
}

/// The folder crumbs of an Episode Inspector item: its Item ID minus the file name, which the
/// title shows.
pub fn inspector_crumbs(id: &str) -> Vec<&str> {
    let mut segments = path_segments(id);
    segments.pop();
    segments
}

/// The audio a Remux run writes for an item's inspected tracks: FLAC becomes Opus at the
/// draft's bitrate (named only when set), anything else passes through.
pub fn remux_audio_text(meta: Option<&InspectionMetadata>, bitrate: &str) -> String {
    let Some(meta) = meta else {
        return "No audio".into();
    };
    let audio = first_text(&[meta.audio_summary.as_deref()], "");
    if audio.is_empty() {
        "No audio".into()
    } else if meta.has_flac != Some(true) {
        format!("{audio} · passthrough")
    } else if bitrate.is_empty() {
        format!("{audio} → Opus")
    } else {
        format!("{audio} → Opus {bitrate}")
    }
}

/// The ladder rung of a manifest subtitle candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Same,
    Ancestor,
}

/// `Same` when the candidate sits in the item's Release Directory, else `Ancestor`. Both are
/// root-relative POSIX paths from the manifest; an empty release is the root.
pub fn candidate_tier(path: &str, release: &str) -> Tier {
    let dir = path.rfind('/').map_or(".", |at| &path[..at]);
    let release = if release.is_empty() { "." } else { release };
    if dir == release {
        Tier::Same
    } else {
        Tier::Ancestor
    }
}

/// One rung of the subtitle precedence ladder.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rung {
    pub key: &'static str,
    pub label: &'static str,
    /// The meta line's subtitle part when this rung is chosen.
    pub short: &'static str,
    /// What the inspection found for it: a path, a count of files, the embedded tracks, or "".
    pub detail: String,
    pub chosen: bool,
    pub tag: &'static str,
}

/// The rungs, highest precedence first: key, label, short label.
const RUNGS: [(&str, &str, &str); 5] = [
    ("manual", "Manual assignment", "Manual subs"),
    ("same", "Same folder", "Same folder subs"),
    ("embedded", "Embedded track", "Embedded subs"),
    ("ancestor", "Parent folder", "Parent folder subs"),
    ("assets", "Assets directory", "Assets dir fallback"),
];

/// One path, else how many subtitle files, else "".
fn files_text(paths: &[&String]) -> String {
    match paths {
        [] => String::new(),
        [path] => (*path).clone(),
        _ => format!("{} subtitle files", paths.len()),
    }
}

/// The subtitle precedence ladder of one item (ADR-0004): manual > same folder > embedded >
/// parent folder > assets dir. A Track Assignment is "Used". Without one, the highest rung that
/// found anything is "Auto": the engine tries it first but matches files to episodes itself, so
/// the ladder does not claim which file wins. The assets dir is a setting, not a found file, so
/// it is only ever a "Fallback".
pub fn subtitle_ladder(
    release: &str,
    candidates: &[String],
    embedded: Option<&str>,
    assignment: Option<&str>,
    assets_dir: &str,
) -> Vec<Rung> {
    let tiered = |tier: Tier| {
        let paths: Vec<&String> = candidates
            .iter()
            .filter(|path| candidate_tier(path, release) == tier)
            .collect();
        files_text(&paths)
    };
    let details = [
        assignment.unwrap_or_default().to_string(),
        tiered(Tier::Same),
        embedded.unwrap_or_default().to_string(),
        tiered(Tier::Ancestor),
        assets_dir.to_string(),
    ];
    let winner = details[..4].iter().position(|detail| !detail.is_empty());
    RUNGS
        .iter()
        .zip(details)
        .enumerate()
        .map(|(at, (&(key, label, short), detail))| {
            let chosen = winner == Some(at);
            let tag = match (chosen, key) {
                (true, "manual") => "Used",
                (true, _) => "Auto",
                _ if detail.is_empty() => "None",
                (_, "assets") => "Fallback",
                _ => "Found",
            };
            Rung {
                key,
                label,
                short,
                detail,
                chosen,
                tag,
            }
        })
        .collect()
}

/// The meta line's subtitle part: the chosen rung, else the assets fallback, else `No subs`.
pub fn subtitle_source_text(ladder: &[Rung]) -> &'static str {
    ladder
        .iter()
        .find(|rung| rung.chosen)
        .or_else(|| {
            ladder
                .iter()
                .find(|rung| rung.key == "assets" && !rung.detail.is_empty())
        })
        .map_or("No subs", |rung| rung.short)
}

/// The batch of a mode's Job Snapshot, once its `BatchStart` named at least one item: before
/// that, or with no job, the queue shows its selection.
#[derive(Debug, Clone, Copy)]
pub struct Batch {
    pub status: BatchStatus,
    /// How long the job has run.
    pub elapsed: Duration,
}

/// The batch of `job`, which started at `started_at`, at `now`: none for a job that ended before
/// its `BatchStart` named an item, which ran no batch.
pub fn batch(job: &JobSnapshot, started_at: Instant, now: Instant) -> Option<Batch> {
    (!job.ordered_item_ids.is_empty()).then(|| Batch {
        status: job.status,
        elapsed: now.saturating_duration_since(started_at),
    })
}

/// The queue count beside the list header: the scan's state while a scan runs or after it
/// failed, else the Queue Selection before a batch, then `6 of 12 · 5 done · 1 failed` over the
/// batch's participants (settled, then each outcome that occurred).
pub fn count_text(scan: &ScanView, rows: &[Row], batch: Option<&Batch>) -> String {
    match scan {
        ScanView::Scanning { .. } => return "Scanning…".into(),
        ScanView::Failed => return "Scan failed".into(),
        ScanView::Idle => {}
    }
    if batch.is_none() {
        if rows.is_empty() {
            return "Ready".into();
        }
        let selected = rows.iter().filter(|row| row.selected).count();
        return format!("{selected} / {} Selected", rows.len());
    }
    let (mut total, mut done, mut skipped, mut cancelled, mut failed) = (0, 0, 0, 0, 0);
    for row in rows.iter().filter(|row| row.participant) {
        total += 1;
        match row.status {
            ItemStatus::Completed | ItemStatus::Planned => done += 1,
            ItemStatus::Skipped => skipped += 1,
            ItemStatus::Cancelled => cancelled += 1,
            ItemStatus::Error => failed += 1,
            _ => {}
        }
    }
    let mut parts = vec![format!("{done} done")];
    for (count, word) in [
        (skipped, "skipped"),
        (cancelled, "cancelled"),
        (failed, "failed"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {word}"));
        }
    }
    let settled = done + skipped + cancelled + failed;
    format!("{settled} of {total} · {}", parts.join(" · "))
}

/// The bottom bar's clock: how long the batch has run, or how it ended.
pub fn batch_clock(batch: Option<&Batch>) -> String {
    match batch {
        None => "Not started".into(),
        Some(batch) => match batch.status {
            BatchStatus::Running => format!("{} elapsed", format_elapsed(batch.elapsed)),
            BatchStatus::Cancelled => "Stopped".into(),
            BatchStatus::Completed => "Finished".into(),
            BatchStatus::Error => "Finished with errors".into(),
        },
    }
}

/// The stacked progress bar's four widths, in percent of the batch's participants once it
/// started, else of the Queue Selection. A processing item adds its own progress to `live`, or a
/// sliver while it has none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Progress {
    pub done: f64,
    pub skipped: f64,
    pub failed: f64,
    pub live: f64,
}

pub fn progress_segments(rows: &[Row], batch: Option<&Batch>) -> Progress {
    let mut parts = Progress::default();
    let mut total = 0usize;
    let counted = rows.iter().filter(|row| match batch {
        Some(_) => row.participant,
        None => row.selected,
    });
    for row in counted {
        total += 1;
        match row.status {
            ItemStatus::Completed | ItemStatus::Planned => parts.done += 1.0,
            ItemStatus::Skipped | ItemStatus::Cancelled => parts.skipped += 1.0,
            ItemStatus::Error => parts.failed += 1.0,
            ItemStatus::Processing => parts.live += row.progress.map_or(0.05, |p| p / 100.0),
            ItemStatus::Queued => {}
        }
    }
    let total = total.max(1) as f64;
    for part in [
        &mut parts.done,
        &mut parts.skipped,
        &mut parts.failed,
        &mut parts.live,
    ] {
        *part = (*part / total * 100.0).min(100.0);
    }
    parts
}

/// One Library entry: the input folder (`release` `None`), then each other Release Directory
/// in queue order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LibraryEntry {
    pub release: Option<String>,
    pub label: String,
    pub tooltip: String,
    /// The folder's rows: every row for the input folder.
    pub count: usize,
    /// The list's group header over this release's rows; the input folder's covers the rows
    /// whose release is `.` or empty.
    pub group_label: String,
}

/// The Library of a queue on `directory`: empty with no folder open. The counts are each
/// release's total, whichever filter the list shows.
pub fn library(directory: Option<&str>, rows: &[Row]) -> Vec<LibraryEntry> {
    let Some(dir) = directory.filter(|dir| !dir.is_empty()) else {
        return Vec::new();
    };
    let shown_dir = pretty_path(dir);
    let group_label = |release: &str| {
        [pretty_path(release), shown_dir.clone()]
            .into_iter()
            .find(|label| !label.is_empty())
            .unwrap_or_else(|| "Episodes".into())
    };
    let mut entries = vec![LibraryEntry {
        release: None,
        label: path_segments(dir).last().unwrap_or(&dir).to_string(),
        tooltip: shown_dir.clone(),
        count: rows.len(),
        group_label: group_label("."),
    }];
    let mut at = std::collections::HashMap::new();
    for row in rows {
        let release = if row.release.is_empty() {
            "."
        } else {
            row.release.as_str()
        };
        if release == "." {
            continue;
        }
        let index = *at.entry(release).or_insert_with(|| {
            entries.push(LibraryEntry {
                release: Some(release.to_string()),
                label: pretty_path(release),
                tooltip: pretty_path(release),
                count: 0,
                group_label: group_label(release),
            });
            entries.len() - 1
        });
        entries[index].count += 1;
    }
    entries
}

/// The list header's crumbs: the folder's path segments.
pub fn list_crumbs(directory: Option<&str>) -> Vec<String> {
    path_segments(directory.unwrap_or_default())
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// The list header's title: the folder's name, or `No folder open`.
pub fn list_title(directory: Option<&str>) -> String {
    path_segments(directory.unwrap_or_default())
        .last()
        .map_or_else(|| "No folder open".into(), |name| name.to_string())
}

/// Who holds the batch, which every mode's controls follow.
#[derive(Debug, Clone, Copy, Default)]
pub struct Hold {
    /// The mode that owns the batch: the Job Coordinator's, else that of a job still active.
    pub owner: Option<PipelineMode>,
    /// Whether the owner's Stop is tearing its start or job down.
    pub stopping: bool,
    /// Whether a start or a job holds the batch.
    pub busy: bool,
}

/// A mode's Run and Stop buttons and status pill, and whether its Queue Selection is locked, as
/// the core state decides them. A view also locks Run and the settings while a Run it sent is not
/// answered yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Controls {
    /// `Run remux · 2`, or `Plan …` for a dry run; without the count when the queue is empty.
    pub run_label: String,
    /// Why Run is disabled, for its tooltip; "" when it is not, or when nothing explains it.
    pub run_title: String,
    pub run_disabled: bool,
    /// Whether this mode owns the batch: the bar shows its Stop instead of its Run.
    pub owns: bool,
    pub stop_label: &'static str,
    pub stop_disabled: bool,
    /// Whether the core refuses a selection or order change: a batch is held, or the mode scans.
    pub selection_locked: bool,
    pub status: StatusPill,
}

/// The status pill: its style class and its text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StatusPill {
    pub class: &'static str,
    pub label: &'static str,
}

impl StatusPill {
    const IDLE: Self = Self::new("status-idle", "Idle");

    const fn new(class: &'static str, label: &'static str) -> Self {
        Self { class, label }
    }
}

/// The controls of `mode`, whose published view is `view`, while `hold` holds the batch.
pub fn controls(mode: PipelineMode, view: &ModeView, hold: &Hold) -> Controls {
    let name = mode_name(mode);
    let scanning = matches!(view.scan, ScanView::Scanning { .. });
    let has_items = view.total_count > 0;
    let unticked = has_items && view.selected_count == 0;
    let verb = if view.draft.dry_run { "Plan" } else { "Run" };
    let owns = hold.owner == Some(mode);
    let run_title = match hold.owner {
        Some(PipelineMode::Remux) if !owns => "Remux batch is running",
        Some(PipelineMode::Extract) if !owns => "Extract batch is running",
        _ if scanning => "Scanning the folder",
        _ if unticked => "Tick at least one episode",
        _ => "",
    };
    // The owner's phase (a start shows as running), else how the mode's last batch ended.
    let status = match (owns, view.outcome) {
        (true, _) if hold.stopping => StatusPill::new("status-running", "Stopping"),
        (true, _) => StatusPill::new("status-running", "Running"),
        (false, Some(Outcome::Done)) => StatusPill::new("status-success", "Done"),
        (false, Some(Outcome::Cancelled)) => StatusPill::new("status-cancelled", "Cancelled"),
        (false, Some(Outcome::Failed)) => StatusPill::new("status-error", "Failed"),
        (false, None) => StatusPill::IDLE,
    };
    Controls {
        run_label: if has_items {
            format!("{verb} {name} · {}", view.selected_count)
        } else {
            format!("{verb} {name}")
        },
        run_title: run_title.into(),
        run_disabled: hold.busy || scanning || unticked,
        owns,
        stop_label: if owns && hold.stopping {
            "Stopping…"
        } else {
            "Stop"
        },
        stop_disabled: !owns || hold.stopping,
        selection_locked: hold.busy || scanning,
        status,
    }
}

/// The worker and I/O job chips, and the `auto (N)` hints of the workers and Disk I/O fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Chips {
    pub workers: String,
    pub io_jobs: String,
    /// `None` until the defaults are known, which leaves the fields on their own hint.
    pub workers_hint: Option<String>,
    pub io_jobs_hint: Option<String>,
}

/// The chips while `busy` shows `batch`, the start's or the job's Effective Settings (none
/// before the start's request resolved), else `defaults`: a count is left out rather than
/// guessed while unknown.
pub fn chips(
    busy: bool,
    batch: Option<&EffectiveSettings>,
    defaults: Option<&EffectiveSettings>,
) -> Chips {
    let shown = if busy { batch } else { defaults };
    let chip = |idle: &str, count: Option<usize>, noun: &str| {
        let counted = count.map(|count| format!("{count} {noun}"));
        match (busy, counted) {
            (true, counted) => format!("{} Active", counted.as_deref().unwrap_or(noun)),
            (false, Some(counted)) => format!("{idle} ({counted})"),
            (false, None) => idle.into(),
        }
    };
    let hint = |count: fn(&EffectiveSettings) -> usize| {
        defaults.map(|defaults| format!("auto ({})", count(defaults)))
    };
    Chips {
        workers: chip("Standby", shown.map(|s| s.workers), "Workers"),
        io_jobs: chip("Idle", shown.map(|s| s.io_jobs), "IO Jobs"),
        workers_hint: hint(|s| s.workers),
        io_jobs_hint: hint(|s| s.io_jobs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(secs: u64) -> Option<Duration> {
        Some(Duration::from_secs(secs))
    }

    /// A row at `status`, selected unless `selected` says otherwise, as the queue derives it.
    fn row(id: &str, status: ItemStatus, participant: bool, selected: bool) -> Row {
        Row {
            participant,
            status,
            selected,
            bucket: bucket(status, selected),
            ..Row::test(id)
        }
    }

    #[test]
    fn test_the_default_labels_keep_their_wording() {
        assert_eq!(
            [DEFAULT_SKIPPED, DEFAULT_CANCELLED, DEFAULT_FAILED],
            ["Skipped", "Cancelled", "Failed"]
        );
    }

    // The webview's former path cases: Finder-style crumbs and the Inspector crumbs.
    #[test]
    fn test_paths_read_finder_style() {
        assert_eq!(
            path_segments("/media/anime/Show (2023)/"),
            ["media", "anime", "Show (2023)"]
        );
        assert_eq!(
            path_segments("../Subs/./Show - SP1.ass"),
            ["Subs", "Show - SP1.ass"]
        );
        assert_eq!(path_segments(r"C:\Anime\Show"), ["C:", "Anime", "Show"]);
        assert!(path_segments("").is_empty());
        assert_eq!(pretty_path("/media/anime/Show"), "media › anime › Show");
        assert_eq!(pretty_path("."), "");
        assert!(inspector_crumbs("Show - 01.mkv").is_empty());
        assert_eq!(
            inspector_crumbs("Show/Season 1/Show - 01.mkv"),
            ["Show", "Season 1"]
        );
        assert_eq!(list_title(Some("/media/anime/Show")), "Show");
        assert_eq!(list_title(Some("/")), "No folder open");
        assert_eq!(list_title(None), "No folder open");
    }

    #[test]
    fn test_status_tones() {
        assert_eq!(status_tone(ItemStatus::Processing), Tone::Info);
        assert_eq!(status_tone(ItemStatus::Completed), Tone::Ok);
        assert_eq!(status_tone(ItemStatus::Error), Tone::Err);
        for status in [
            ItemStatus::Queued,
            ItemStatus::Skipped,
            ItemStatus::Cancelled,
            ItemStatus::Planned,
        ] {
            assert_eq!(status_tone(status), Tone::Mute);
        }
    }

    // The webview's former status label cases, with the queue card's label parts.
    #[test]
    fn test_status_labels() {
        use ItemStatus::*;
        assert_eq!(
            status_label(Processing, Some(64.4), secs(72), None),
            "64% · 1m 12s"
        );
        assert_eq!(
            status_label(Processing, None, secs(6), None),
            "Processing · 6s"
        );
        assert_eq!(status_label(Processing, Some(64.5), None, None), "65% · 0s");
        assert_eq!(
            status_label(Completed, None, None, Some(48.0)),
            "Done in 48.0s"
        );
        assert_eq!(
            status_label(Completed, None, None, Some(0.0)),
            "Done in <0.1s"
        );
        assert_eq!(
            status_label(Completed, None, None, Some(75.4)),
            "Done in 1m 15.4s"
        );
        assert_eq!(status_label(Completed, None, None, None), "Done");
        assert_eq!(status_label(Error, None, None, None), "Failed");
        assert_eq!(status_label(Skipped, None, None, None), "Skipped");
        assert_eq!(status_label(Cancelled, None, None, None), "Cancelled");
        assert_eq!(status_label(Planned, None, None, None), "Planned");
        assert_eq!(status_label(Queued, None, None, None), "Queued");
    }

    // The webview's former queue cases: the default titles and the job-only meta lines.
    #[test]
    fn test_titles_and_job_only_meta_lay_the_defaults_over_what_the_engine_sent() {
        use ItemStatus::*;
        assert_eq!(
            status_title(Skipped, None, Some("Already up to date")),
            "Already up to date"
        );
        assert_eq!(status_title(Skipped, None, None), "Skipped");
        assert_eq!(status_title(Cancelled, None, None), "Cancelled");
        assert_eq!(
            status_title(Error, Some("FFmpeg encoding failed"), None),
            "FFmpeg encoding failed"
        );
        assert_eq!(status_title(Error, None, None), "Failed");
        assert_eq!(status_title(Queued, None, None), "");
        assert_eq!(status_title(Processing, Some("x"), Some("y")), "");

        assert_eq!(
            job_only_meta(Completed, Some(10.2), None, None),
            "Processed · Done in 10.2s"
        );
        assert_eq!(job_only_meta(Completed, None, None, None), "Processed");
        assert_eq!(
            job_only_meta(Skipped, None, Some("Already up to date"), None),
            "Skipped · Already up to date"
        );
        // The default reason comes first, as in the webview before S4c.
        assert_eq!(
            job_only_meta(Skipped, None, None, None),
            "Skipped · Skipped"
        );
        assert_eq!(
            job_only_meta(Skipped, None, Some(""), None),
            "Skipped · Already processed"
        );
        assert_eq!(
            job_only_meta(Cancelled, None, None, None),
            "Cancelled · Cancelled"
        );
        assert_eq!(job_only_meta(Queued, None, None, None), "Queued");
        assert_eq!(job_only_meta(Planned, None, None, None), "Queued");

        // A failed row says why; with no text, the default one.
        assert_eq!(
            failed_meta(Some("mkvmerge exited with 2")),
            "Failed · mkvmerge exited with 2"
        );
        assert_eq!(failed_meta(None), "Failed · Failed");
        assert_eq!(failed_meta(Some("")), "Failed · Error");
    }

    // The webview's former filter cases.
    #[test]
    fn test_filter_buckets_count_selected_rows_only_and_all_counts_every_row() {
        let rows = [
            row("a", ItemStatus::Completed, false, true),
            row("b", ItemStatus::Error, false, true),
            row("c", ItemStatus::Processing, false, true),
            row("d", ItemStatus::Queued, false, true),
            row("e", ItemStatus::Queued, false, false),
        ];
        assert_eq!(
            filter_counts(&rows),
            Filters {
                all: 5,
                todo: 2,
                done: 1,
                failed: 1
            }
        );
        assert_eq!(rows[4].bucket, None);
        assert_eq!(bucket(ItemStatus::Cancelled, true), Some(Bucket::Todo));
        assert_eq!(bucket(ItemStatus::Skipped, true), Some(Bucket::Done));
        assert_eq!(bucket(ItemStatus::Planned, true), Some(Bucket::Done));
    }

    // The webview's former progress segment and batch clock cases.
    #[test]
    fn test_progress_segments_cover_the_batch_and_the_clock_says_how_it_ended() {
        let mut processing = row("c", ItemStatus::Processing, true, true);
        processing.progress = Some(40.0);
        let rows = [
            row("a", ItemStatus::Completed, true, true),
            row("b", ItemStatus::Skipped, true, true),
            processing,
            row("d", ItemStatus::Queued, true, true),
            // Not a participant: the batch is the four above.
            row("e", ItemStatus::Error, false, true),
        ];
        assert_eq!(batch_clock(None), "Not started");
        let running = Batch {
            status: BatchStatus::Running,
            elapsed: Duration::from_millis(72_900),
        };
        assert_eq!(
            progress_segments(&rows, Some(&running)),
            Progress {
                done: 25.0,
                skipped: 25.0,
                failed: 0.0,
                live: 10.0
            }
        );
        assert_eq!(batch_clock(Some(&running)), "1m 12s elapsed");
        let ended = |status| batch_clock(Some(&Batch { status, ..running }));
        assert_eq!(ended(BatchStatus::Error), "Finished with errors");
        assert_eq!(ended(BatchStatus::Cancelled), "Stopped");
        assert_eq!(ended(BatchStatus::Completed), "Finished");

        // Before a batch: the selection, and a processing item with no progress is a sliver.
        let rows = [
            row("a", ItemStatus::Processing, false, true),
            row("b", ItemStatus::Queued, false, false),
        ];
        assert_eq!(progress_segments(&rows, None).live, 5.0);
        assert_eq!(progress_segments(&[], None), Progress::default());
    }

    // The count follows the job's own batch.
    #[test]
    fn test_the_count_text_follows_the_batch_else_the_selection() {
        let running = Batch {
            status: BatchStatus::Running,
            elapsed: Duration::ZERO,
        };
        let rows = |status| {
            [
                row("ep1", status, true, true),
                row("ep2", ItemStatus::Queued, false, false),
                row("ep3", ItemStatus::Queued, false, false),
            ]
        };
        assert_eq!(
            count_text(&ScanView::Idle, &rows(ItemStatus::Queued), Some(&running)),
            "0 of 1 · 0 done"
        );
        assert_eq!(
            count_text(
                &ScanView::Idle,
                &rows(ItemStatus::Completed),
                Some(&running)
            ),
            "1 of 1 · 1 done"
        );
        assert_eq!(
            count_text(&ScanView::Idle, &rows(ItemStatus::Queued), None),
            "1 / 3 Selected"
        );
        assert_eq!(count_text(&ScanView::Idle, &[], None), "Ready");
        assert_eq!(count_text(&ScanView::Failed, &[], None), "Scan failed");
        let mixed = [
            row("a", ItemStatus::Completed, true, true),
            row("b", ItemStatus::Planned, true, true),
            row("c", ItemStatus::Skipped, true, true),
            row("d", ItemStatus::Cancelled, true, true),
            row("e", ItemStatus::Error, true, true),
            row("f", ItemStatus::Processing, true, true),
        ];
        assert_eq!(
            count_text(&ScanView::Idle, &mixed, Some(&running)),
            "5 of 6 · 2 done · 1 skipped · 1 cancelled · 1 failed"
        );
    }

    // Rust's {:.1} and JavaScript's toFixed(1) could differ only on an exact binary tie; the value
    // is rounded to tenths first, so none of these, nor any real duration, meets one.
    #[test]
    fn test_format_outcome_duration() {
        for (seconds, text) in [
            (0.0, "<0.1s"),
            (0.04, "<0.1s"),
            (-1.0, "<0.1s"),
            (4.2, "4.2s"),
            (59.94, "59.9s"),
            (59.96, "1m 0.0s"),
            (60.0, "1m 0.0s"),
            (75.4, "1m 15.4s"),
            (119.96, "2m 0.0s"),
        ] {
            assert_eq!(format_outcome_duration(seconds), text, "{seconds}");
        }
    }

    #[test]
    fn test_format_elapsed() {
        // A Duration is never negative: the JavaScript table's -5 and -0.1 read 0s as this does.
        for (millis, text) in [
            (0, "0s"),
            (4_200, "4s"),
            (42_000, "42s"),
            (59_900, "59s"),
            (75_400, "1m 15s"),
            (130_000, "2m 10s"),
        ] {
            assert_eq!(format_elapsed(Duration::from_millis(millis)), text);
        }
    }

    // The webview's former remux audio cases.
    #[test]
    fn test_remux_audio_names_the_opus_bitrate_only_for_flac() {
        let meta = |audio: &str, flac| InspectionMetadata {
            audio_summary: Some(audio.into()),
            has_flac: Some(flac),
            ..InspectionMetadata::default()
        };
        assert_eq!(
            remux_audio_text(Some(&meta("FLAC 2.0", true)), "192k"),
            "FLAC 2.0 → Opus 192k"
        );
        assert_eq!(
            remux_audio_text(Some(&meta("FLAC 2.0", true)), ""),
            "FLAC 2.0 → Opus"
        );
        assert_eq!(
            remux_audio_text(Some(&meta("AAC 2.0", false)), "192k"),
            "AAC 2.0 · passthrough"
        );
        assert_eq!(remux_audio_text(Some(&meta("", true)), "192k"), "No audio");
        assert_eq!(remux_audio_text(None, "192k"), "No audio");
    }

    // The meta line from the draft's bitrate and the assignment.
    #[test]
    fn test_inspected_meta_lines() {
        let meta = InspectionMetadata {
            audio_summary: Some("FLAC 2.0".into()),
            has_flac: Some(true),
            sub_summary: Some("ASS (ind)".into()),
            fonts_count: Some(3),
            ..InspectionMetadata::default()
        };
        let remux =
            |assignment, bitrate| inspected_meta(true, &meta, ".", &[], assignment, bitrate, "");
        assert_eq!(
            remux(None, "192k"),
            "FLAC 2.0 → Opus 192k · Embedded subs · 3 fonts"
        );
        assert_eq!(remux(None, ""), "FLAC 2.0 → Opus · Embedded subs · 3 fonts");
        assert_eq!(
            remux(Some("/subs/01.ass"), "256k"),
            "FLAC 2.0 → Opus 256k · Manual subs · 3 fonts"
        );
        assert_eq!(
            inspected_meta(false, &meta, ".", &[], None, "192k", ""),
            "ASS (ind) · 3 fonts"
        );
        let bare = InspectionMetadata::default();
        assert_eq!(
            inspected_meta(false, &bare, ".", &[], None, "", ""),
            "No subs · 0 fonts"
        );
        assert_eq!(
            inspected_meta(true, &bare, ".", &[], None, "", "/assets"),
            "No audio · Assets dir fallback · 0 fonts"
        );
    }

    // The webview's former subtitle ladder cases.
    #[test]
    fn test_the_subtitle_ladder_follows_the_precedence_order_and_never_claims_an_automatic_file() {
        let candidates: Vec<String> = [
            "Season 1/Show - 04.ass",
            "Show - 01.ass",
            "Subs/Show - 02.ass",
        ]
        .map(String::from)
        .into();
        let tags = |ladder: &[Rung]| -> Vec<(&str, &str)> {
            ladder.iter().map(|rung| (rung.key, rung.tag)).collect()
        };
        let auto = subtitle_ladder("Season 1", &candidates, Some("ASS (ENG)"), None, "");
        assert_eq!(
            tags(&auto),
            [
                ("manual", "None"),
                ("same", "Auto"),
                ("embedded", "Found"),
                ("ancestor", "Found"),
                ("assets", "None")
            ]
        );
        assert_eq!(auto[1].detail, "Season 1/Show - 04.ass");
        assert_eq!(auto[3].detail, "2 subtitle files");
        assert_eq!(subtitle_source_text(&auto), "Same folder subs");

        let manual = subtitle_ladder("Season 1", &candidates, None, Some("/x/fixed.ass"), "");
        assert_eq!(manual[0].tag, "Used");
        assert_eq!(manual[1].tag, "Found");
        assert_eq!(subtitle_source_text(&manual), "Manual subs");

        // The root release lists only root files as "same folder".
        let root = subtitle_ladder(".", &["Show - 01.ass".into()], None, None, "");
        assert_eq!(root[1].tag, "Auto");
        assert_eq!(
            subtitle_source_text(&subtitle_ladder(".", &[], None, None, "")),
            "No subs"
        );
        // A configured assets dir is a fallback setting, never a found file.
        let assets_only = subtitle_ladder(".", &[], None, None, "/assets");
        assert_eq!(assets_only[4].tag, "Fallback");
        assert!(!assets_only.iter().any(|rung| rung.chosen));
        assert_eq!(subtitle_source_text(&assets_only), "Assets dir fallback");

        assert_eq!(
            candidate_tier("Season 1/Show - 04.ass", "Season 1"),
            Tier::Same
        );
        assert_eq!(candidate_tier("Show - 01.ass", "Season 1"), Tier::Ancestor);
        assert_eq!(candidate_tier("Show - 01.ass", "."), Tier::Same);
        assert_eq!(candidate_tier("Show - 01.ass", ""), Tier::Same);
    }

    #[test]
    fn test_the_library_lists_the_folder_then_each_release_in_queue_order() {
        assert!(library(None, &[]).is_empty());
        let in_release = |id: &str, release: &str| Row {
            release: release.into(),
            ..Row::test(id)
        };
        let rows = [
            in_release("a", "."),
            in_release("b", "Season 2"),
            in_release("c", "Season 1/Extras"),
            in_release("d", "Season 2"),
            // A row the manifest does not list has no release: it counts under the folder.
            in_release("e", ""),
        ];
        let entries = library(Some("/media/Show"), &rows);
        let shown: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.release.as_deref(),
                    entry.label.as_str(),
                    entry.tooltip.as_str(),
                    entry.count,
                    entry.group_label.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (None, "Show", "media › Show", 5, "media › Show"),
                (Some("Season 2"), "Season 2", "Season 2", 2, "Season 2"),
                (
                    Some("Season 1/Extras"),
                    "Season 1 › Extras",
                    "Season 1 › Extras",
                    1,
                    "Season 1 › Extras"
                ),
            ]
        );
        // A folder with no name of its own is labelled as typed, and its group as Episodes.
        let root = &library(Some("/"), &rows[..1])[0];
        assert_eq!(
            (root.label.as_str(), root.group_label.as_str()),
            ("/", "Episodes")
        );
    }

    /// A remux view of two selected rows of two, as the actor publishes it.
    fn remux_view() -> ModeView {
        let mut view = crate::actor::ViewState::new(None).remux;
        view.total_count = 2;
        view.selected_count = 2;
        view
    }

    // Run while the queue scans, starts or another mode runs.
    #[test]
    fn test_run_is_disabled_while_its_queue_scans_or_another_mode_holds_the_batch() {
        let mut view = remux_view();
        view.scan = ScanView::Scanning {
            directory: "/anime".into(),
        };
        let idle = Hold::default();
        let scanning = controls(PipelineMode::Remux, &view, &idle);
        assert!(scanning.run_disabled && scanning.selection_locked);
        assert_eq!(scanning.run_title, "Scanning the folder");
        assert_eq!(scanning.run_label, "Run remux · 2");

        view.scan = ScanView::Idle;
        let ready = controls(PipelineMode::Remux, &view, &idle);
        assert!(!ready.run_disabled && !ready.selection_locked);
        assert_eq!(ready.run_title, "");
        // A dry run plans; the label follows the draft.
        view.draft.dry_run = true;
        assert_eq!(
            controls(PipelineMode::Remux, &view, &idle).run_label,
            "Plan remux · 2"
        );

        // The other mode's start, or its job with an idle coordinator, blocks this Run.
        let extract = Hold {
            owner: Some(PipelineMode::Extract),
            stopping: false,
            busy: true,
        };
        let blocked = controls(PipelineMode::Remux, &view, &extract);
        assert!(blocked.run_disabled && !blocked.owns && blocked.selection_locked);
        assert_eq!(blocked.run_title, "Extract batch is running");
        assert_eq!((blocked.stop_label, blocked.stop_disabled), ("Stop", true));
        assert_eq!(
            controls(
                PipelineMode::Extract,
                &view,
                &Hold {
                    owner: Some(PipelineMode::Remux),
                    ..extract
                }
            )
            .run_title,
            "Remux batch is running"
        );

        // Nothing ticked in a queue with rows; an empty queue names no count.
        view.selected_count = 0;
        let unticked = controls(PipelineMode::Remux, &view, &idle);
        assert!(unticked.run_disabled);
        assert_eq!(unticked.run_title, "Tick at least one episode");
        view.total_count = 0;
        view.draft.dry_run = false;
        let empty = controls(PipelineMode::Remux, &view, &idle);
        assert!(!empty.run_disabled);
        assert_eq!(empty.run_label, "Run remux");
    }

    // The owner's Stop and pill, else the outcome's pill.
    #[test]
    fn test_the_status_pill_shows_the_owners_phase_else_the_modes_outcome() {
        let mut view = remux_view();
        let pill = |view: &ModeView, hold: &Hold| {
            let status = controls(PipelineMode::Remux, view, hold).status;
            (status.class, status.label)
        };
        let idle = Hold::default();
        for (outcome, expected) in [
            (None, ("status-idle", "Idle")),
            (Some(Outcome::Done), ("status-success", "Done")),
            (Some(Outcome::Cancelled), ("status-cancelled", "Cancelled")),
            (Some(Outcome::Failed), ("status-error", "Failed")),
        ] {
            view.outcome = outcome;
            assert_eq!(pill(&view, &idle), expected, "{outcome:?}");
        }
        // Another mode's batch leaves this pill on this mode's own outcome.
        view.outcome = Some(Outcome::Done);
        let extract = Hold {
            owner: Some(PipelineMode::Extract),
            stopping: false,
            busy: true,
        };
        assert_eq!(pill(&view, &extract), ("status-success", "Done"));

        let owner = Hold {
            owner: Some(PipelineMode::Remux),
            ..extract
        };
        let running = controls(PipelineMode::Remux, &view, &owner);
        assert_eq!(pill(&view, &owner), ("status-running", "Running"));
        assert!(running.owns && !running.stop_disabled && running.run_disabled);
        assert_eq!(running.stop_label, "Stop");
        let stopping = Hold {
            stopping: true,
            ..owner
        };
        let controls = controls(PipelineMode::Remux, &view, &stopping);
        assert_eq!(pill(&view, &stopping), ("status-running", "Stopping"));
        assert_eq!(
            (controls.stop_label, controls.stop_disabled),
            ("Stopping…", true)
        );
    }

    // The settings chips: the defaults while idle, the batch's settings while busy.
    #[test]
    fn test_the_chips_show_the_defaults_while_idle_and_the_batch_while_busy() {
        let settings = |workers, io_jobs| EffectiveSettings {
            workers,
            io_jobs,
            ffmpeg_threads: 1,
            bitrate: "192k".into(),
            complexity: 5,
        };
        let (defaults, batch) = (settings(6, 4), settings(3, 5));
        let idle = chips(false, Some(&batch), Some(&defaults));
        assert_eq!(
            idle,
            Chips {
                workers: "Standby (6 Workers)".into(),
                io_jobs: "Idle (4 IO Jobs)".into(),
                workers_hint: Some("auto (6)".into()),
                io_jobs_hint: Some("auto (4)".into()),
            }
        );
        // No defaults yet: no counts and no hints.
        assert_eq!(
            chips(false, None, None),
            Chips {
                workers: "Standby".into(),
                io_jobs: "Idle".into(),
                workers_hint: None,
                io_jobs_hint: None,
            }
        );
        // A start before its request resolved, then with its settings; the hints stay the
        // defaults'.
        let starting = chips(true, None, Some(&defaults));
        assert_eq!(
            (starting.workers.as_str(), starting.io_jobs.as_str()),
            ("Workers Active", "IO Jobs Active")
        );
        let running = chips(true, Some(&batch), Some(&defaults));
        assert_eq!(
            (running.workers.as_str(), running.io_jobs.as_str()),
            ("3 Workers Active", "5 IO Jobs Active")
        );
        assert_eq!(running.workers_hint.as_deref(), Some("auto (6)"));
    }
}
