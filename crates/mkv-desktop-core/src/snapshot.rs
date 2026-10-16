//! The Job Snapshot: the in-memory projection of one job, reduced from its `JobEvent`s so a
//! webview reload can rebuild the queue. A plain value with no lock: the supervisor keeps one as
//! its private settlement ledger and the Desktop Core actor keeps the published one, both built
//! by the same `JobSnapshot::apply` over the same events.

use mkv_core::item_id::ItemId;
use mkv_core::pipeline::{ItemOutcome, JobEvent, CANCELLED_REASON, EVENT_VERSION};
use mkv_core::request::EffectiveSettings;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BatchStatus {
    Running,
    Completed,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Queued,
    Processing,
    Completed,
    Skipped,
    Error,
    Cancelled,
    Planned,
}

impl ItemStatus {
    /// Whether the item has a final outcome. `Planned` is the dry-run outcome,
    /// not a pending state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ItemStatus::Completed
                | ItemStatus::Skipped
                | ItemStatus::Error
                | ItemStatus::Cancelled
                | ItemStatus::Planned
        )
    }
}

impl From<ItemOutcome> for ItemStatus {
    fn from(outcome: ItemOutcome) -> Self {
        match outcome {
            ItemOutcome::Completed => ItemStatus::Completed,
            ItemOutcome::Skipped => ItemStatus::Skipped,
            ItemOutcome::Error => ItemStatus::Error,
            ItemOutcome::Cancelled => ItemStatus::Cancelled,
            ItemOutcome::Planned => ItemStatus::Planned,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemSnapshot {
    pub id: String,
    pub file: String,
    pub status: ItemStatus,
    pub outcome: Option<ItemOutcome>,
    pub progress: Option<f64>,
    pub start_time_ms: Option<u64>,
    pub duration_sec: Option<f64>,
    pub reason: Option<String>,
    pub error: Option<String>,
    /// When the bridge saw the item start, on a monotonic clock: the origin of the duration of
    /// an item the bridge settles. Not serialized; `start_time_ms` is the wall-clock value the
    /// webview gets.
    #[serde(skip)]
    started_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct JobSnapshot {
    pub job_id: u64,
    pub mode: String,
    pub status: BatchStatus,
    pub start_time_ms: u64,
    pub total_items: usize,
    pub input_dir: String,
    /// What the job runs with, fixed when it starts.
    pub effective_settings: EffectiveSettings,
    pub ordered_item_ids: Vec<String>,
    pub items: HashMap<String, ItemSnapshot>,
}

impl JobSnapshot {
    /// The snapshot of job `job_id`, started at `start_time_ms`: running, with no batch yet.
    pub fn begin(
        job_id: u64,
        mode: &str,
        start_time_ms: u64,
        input_dir: String,
        effective_settings: EffectiveSettings,
    ) -> Self {
        Self {
            job_id,
            mode: mode.to_string(),
            status: BatchStatus::Running,
            start_time_ms,
            total_items: 0,
            input_dir,
            effective_settings,
            ordered_item_ids: Vec::new(),
            items: HashMap::new(),
        }
    }

    /// Applies `event`, one of this job's, to the snapshot. Drops an event of another event
    /// version, and any but a `BatchStart` once the batch has ended. A `BatchStart` replaces the
    /// batch but keeps the job's start time, input directory and Effective Settings.
    pub fn apply(&mut self, event: &JobEvent) {
        let (JobEvent::BatchStart { version, .. }
        | JobEvent::ItemStart { version, .. }
        | JobEvent::ItemProgress { version, .. }
        | JobEvent::ItemComplete { version, .. }
        | JobEvent::BatchComplete { version, .. }) = event;
        // Webview twin: the v1 guard in queue-state.js `reduceQueueEvent`, until S2b removes that
        // reducer. Both must drop the same events.
        if *version != EVENT_VERSION {
            return;
        }
        if !matches!(event, JobEvent::BatchStart { .. }) && self.status != BatchStatus::Running {
            return;
        }
        match event {
            JobEvent::BatchStart { total, items, .. } => {
                self.status = BatchStatus::Running;
                self.total_items = *total;
                self.ordered_item_ids.clear();
                self.items.clear();
                for item in items.iter().flatten() {
                    let id = item.id.to_string();
                    if self.items.contains_key(&id) {
                        continue;
                    }
                    self.ordered_item_ids.push(id.clone());
                    self.items.insert(
                        id.clone(),
                        ItemSnapshot {
                            id,
                            file: item.file.clone(),
                            status: ItemStatus::Queued,
                            outcome: None,
                            progress: None,
                            start_time_ms: None,
                            duration_sec: None,
                            reason: None,
                            error: None,
                            started_at: None,
                        },
                    );
                }
            }
            JobEvent::ItemStart { id, file, .. } => {
                if let Some(item) = self.items.get_mut(id.as_str()) {
                    if !item.status.is_terminal() {
                        item.file = file.clone();
                        item.status = ItemStatus::Processing;
                        item.start_time_ms.get_or_insert_with(timestamp_ms);
                        item.started_at.get_or_insert_with(Instant::now);
                    }
                }
            }
            JobEvent::ItemProgress { id, progress, .. } => {
                if let Some(item) = self.items.get_mut(id.as_str()) {
                    if item.status == ItemStatus::Processing && progress.is_finite() {
                        item.progress = Some(progress.clamp(0.0, 100.0));
                    }
                }
            }
            JobEvent::ItemComplete {
                id,
                file,
                status,
                reason,
                error,
                duration_sec,
                ..
            } => {
                if let Some(item) = self.items.get_mut(id.as_str()) {
                    if !item.status.is_terminal() {
                        item.file = file.clone();
                        item.status = (*status).into();
                        item.outcome = Some(*status);
                        // An outcome without a duration falls back to how long the item ran
                        // since the bridge saw it start, on the monotonic clock.
                        item.duration_sec = duration_sec.or_else(|| {
                            item.started_at
                                .map(|started_at| started_at.elapsed().as_secs_f64())
                        });
                        // Only the reason the event carries: a default is a label for the UI to
                        // show, not a stored value.
                        item.reason = reason.clone();
                        item.error = error.clone();
                    }
                }
            }
            JobEvent::BatchComplete {
                success, cancelled, ..
            } => {
                // The flag trips on any error item, which fails a batch the engine called a
                // success (the supervisor settled one of its items as an error). As a backstop it
                // also trips on any item still open, which Item Settlement (`settlement_events`)
                // should have closed, so a success terminal can never yield Completed (code 0)
                // with open items.
                let has_failed_items = self
                    .items
                    .values()
                    .any(|item| item.status == ItemStatus::Error || !item.status.is_terminal());
                self.status = terminal_status(*success, *cancelled, has_failed_items);
            }
        }
    }

    /// The error of an item settled by a failed job whose terminal carries no error or panic text.
    /// The webview's guard (`queue-state.js`) repeats it for a run that broke Item Settlement;
    /// `test_settlement_wording_matches_the_golden_fixture` pins both to one fixture.
    const BATCH_FAILED_ERROR: &'static str = "Batch failed";

    /// Item Settlement: the `ItemComplete` events that end every item the Batch Pipeline left
    /// open (queued or processing) when `terminal` ends the job, in batch order. They are
    /// cancelled when `terminal` is a stop, else errors carrying the run's own failure: its error,
    /// else its panic, else `BATCH_FAILED_ERROR`; a success `terminal` has no failure of its own,
    /// so its open items carry a protocol error. An item that had started reports how long it
    /// ran, on a monotonic clock; one that never started has no duration.
    ///
    /// The supervisor applies and emits them before `terminal`, so the webview and a reloaded
    /// snapshot see the same events. Empty for a `terminal` that is not a `BatchComplete` and for
    /// a snapshot that is no longer running.
    pub fn settlement_events(&self, terminal: &JobEvent) -> Vec<JobEvent> {
        let JobEvent::BatchComplete {
            success,
            cancelled,
            extra,
            ..
        } = terminal
        else {
            return Vec::new();
        };
        if self.status != BatchStatus::Running {
            return Vec::new();
        }
        let text = |key: &str| extra.get(key).and_then(|value| value.as_str());
        let message = if *success {
            // The engine called the batch a success, so nothing should be open.
            "Protocol error: missing completion event"
        } else {
            text("error")
                .or_else(|| text("panic"))
                .unwrap_or(Self::BATCH_FAILED_ERROR)
        };
        // The outcome is the same for every open item of the job.
        let (status, reason, error) = if *cancelled {
            (
                ItemOutcome::Cancelled,
                Some(CANCELLED_REASON.to_string()),
                None,
            )
        } else {
            (ItemOutcome::Error, None, Some(message.to_string()))
        };
        self.ordered_item_ids
            .iter()
            .filter_map(|id| self.items.get(id))
            .filter(|item| !item.status.is_terminal())
            .map(|item| JobEvent::ItemComplete {
                version: EVENT_VERSION,
                // The snapshot's id came from an `ItemId`, so parsing it again is a no-op.
                id: ItemId::parse(&item.id),
                file: item.file.clone(),
                status,
                reason: reason.clone(),
                error: error.clone(),
                duration_sec: item
                    .started_at
                    .map(|started_at| started_at.elapsed().as_secs_f64()),
                extra: Default::default(),
            })
            .collect()
    }

    /// How many items have an outcome, derived from their statuses rather than counted.
    pub fn completed_items(&self) -> usize {
        self.items
            .values()
            .filter(|item| item.status.is_terminal())
            .count()
    }
}

/// The `get_job_snapshot` payload: the fields in order, with `completed_items` derived, since
/// the webview's restore path still reads it.
/// `test_reducer_derives_completed_items_from_item_outcomes` pins the serialized key set and is
/// the guard against a field silently dropping off the wire; do not "simplify" this back to a
/// derive without keeping `completed_items`.
impl Serialize for JobSnapshot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("JobSnapshot", 10)?;
        state.serialize_field("job_id", &self.job_id)?;
        state.serialize_field("mode", &self.mode)?;
        state.serialize_field("status", &self.status)?;
        state.serialize_field("start_time_ms", &self.start_time_ms)?;
        state.serialize_field("total_items", &self.total_items)?;
        state.serialize_field("completed_items", &self.completed_items())?;
        state.serialize_field("input_dir", &self.input_dir)?;
        state.serialize_field("effective_settings", &self.effective_settings)?;
        state.serialize_field("ordered_item_ids", &self.ordered_item_ids)?;
        state.serialize_field("items", &self.items)?;
        state.end()
    }
}

pub(crate) fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Tags an engine event with the job that produced it so the UI can route it.
pub(crate) fn attach_context(event: &mut JobEvent, mode: &str, job_id: u64) {
    let (JobEvent::BatchStart { extra, .. }
    | JobEvent::ItemStart { extra, .. }
    | JobEvent::ItemProgress { extra, .. }
    | JobEvent::ItemComplete { extra, .. }
    | JobEvent::BatchComplete { extra, .. }) = event;
    extra.insert("mode".to_string(), mode.into());
    extra.insert("job_id".to_string(), job_id.into());
}

fn terminal_status(success: bool, cancelled: bool, has_failed_items: bool) -> BatchStatus {
    if cancelled {
        BatchStatus::Cancelled
    } else if success && !has_failed_items {
        BatchStatus::Completed
    } else {
        BatchStatus::Error
    }
}

/// Test helpers, also exported to the desktop adapter's tests through the `test-support` feature.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use super::JobSnapshot;
    use mkv_core::item_id::ItemId;
    use mkv_core::pipeline::{BatchItemInfo, ItemOutcome, JobEvent};
    use mkv_core::request::EffectiveSettings;

    /// Effective Settings that differ from every default, so a test sees them pass through
    /// unchanged.
    pub fn effective_settings() -> EffectiveSettings {
        EffectiveSettings {
            workers: 3,
            io_jobs: 5,
            ffmpeg_threads: 2,
            bitrate: "256k".into(),
            complexity: 8,
        }
    }

    /// The snapshot of remux job `job_id` of `/anime` as it starts, with `effective_settings`.
    pub fn begun(job_id: u64) -> JobSnapshot {
        JobSnapshot::begin(
            job_id,
            "remux",
            1_700_000_000_000,
            "/anime".into(),
            effective_settings(),
        )
    }

    /// A `BatchStart` listing `ids` in engine order; each item's file is `<id>.mkv`.
    pub fn batch_start(ids: &[&str]) -> JobEvent {
        JobEvent::BatchStart {
            version: 1,
            total: ids.len(),
            input_dir: "/anime".into(),
            items: Some(
                ids.iter()
                    .map(|id| BatchItemInfo {
                        id: ItemId::parse(id),
                        file: format!("{id}.mkv"),
                    })
                    .collect(),
            ),
            extra: Default::default(),
        }
    }

    pub fn item_start(id: &str) -> JobEvent {
        JobEvent::ItemStart {
            version: 1,
            id: ItemId::parse(id),
            file: format!("{id}.mkv"),
            extra: Default::default(),
        }
    }

    pub fn item_progress(id: &str, progress: f64) -> JobEvent {
        JobEvent::ItemProgress {
            version: 1,
            id: ItemId::parse(id),
            file: format!("{id}.mkv"),
            progress,
            extra: Default::default(),
        }
    }

    pub fn item_complete(id: &str, duration_sec: f64) -> JobEvent {
        JobEvent::ItemComplete {
            version: 1,
            id: ItemId::parse(id),
            file: format!("{id}.mkv"),
            status: ItemOutcome::Completed,
            reason: None,
            error: None,
            duration_sec: Some(duration_sec),
            extra: Default::default(),
        }
    }

    pub fn batch_complete(success: bool, cancelled: bool) -> JobEvent {
        JobEvent::BatchComplete {
            version: 1,
            success,
            cancelled,
            extra: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use serde_json::Value;

    #[test]
    fn test_snapshot_lifecycle() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1"]));
        assert_eq!(snap.job_id, 10);
        assert_eq!(snap.mode, "remux");
        assert_eq!(snap.status, BatchStatus::Running);
        assert_eq!(snap.total_items, 1);
        assert_eq!(snap.completed_items(), 0);
        assert_eq!(snap.ordered_item_ids, vec!["ep1".to_string()]);

        snap.apply(&item_start("ep1"));
        assert_eq!(snap.items["ep1"].status, ItemStatus::Processing);

        let complete = item_complete("ep1", 42.5);
        snap.apply(&complete);
        assert_eq!(snap.completed_items(), 1);
        let item = &snap.items["ep1"];
        assert_eq!(item.status, ItemStatus::Completed);
        assert_eq!(item.duration_sec, Some(42.5));
        assert_eq!(item.outcome, Some(ItemOutcome::Completed));

        // A duplicate completion must not count the item twice.
        snap.apply(&complete);
        assert_eq!(snap.completed_items(), 1);

        snap.apply(&batch_complete(true, false));
        assert_eq!(snap.status, BatchStatus::Completed);
    }

    #[test]
    fn test_snapshot_carries_the_effective_settings_of_its_job() {
        let mut snap = begun(7);
        let settings = effective_settings();
        assert_eq!(snap.effective_settings, settings);

        // Neither the engine's BatchStart nor the terminal event replaces them.
        snap.apply(&batch_start(&["ep1"]));
        assert_eq!(snap.effective_settings, settings);
        snap.apply(&batch_complete(true, false));
        assert_eq!(snap.effective_settings, settings);

        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["effective_settings"]["io_jobs"], 5);
        assert_eq!(json["effective_settings"]["bitrate"], "256k");

        // A second BatchStart replaces the batch, never the job: its start time, input directory
        // and settings stay. A later job begins its own snapshot, so it inherits none of them
        // (the actor's `test_a_later_job_replaces_the_snapshot_and_stale_events_are_dropped`).
        let start_time_ms = snap.start_time_ms;
        snap.apply(&batch_start(&["ep2"]));
        assert_eq!(snap.effective_settings, settings);
        assert_eq!(snap.input_dir, "/anime");
        assert_eq!(snap.start_time_ms, start_time_ms);
        assert_eq!(snap.ordered_item_ids, ["ep2"]);
    }

    /// Applies the settlement of `terminal` as the supervisor does, and returns the events as the
    /// webview receives them.
    fn settle(snap: &mut JobSnapshot, terminal: &JobEvent) -> Vec<Value> {
        let events = snap.settlement_events(terminal);
        for event in &events {
            snap.apply(event);
        }
        serde_json::to_value(events)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }

    /// A failed `BatchComplete` whose `extra` is the object `extra`.
    fn failed_with(extra: Value) -> JobEvent {
        JobEvent::BatchComplete {
            version: 1,
            success: false,
            cancelled: false,
            extra: extra.as_object().unwrap().clone(),
        }
    }

    #[test]
    fn test_success_that_left_items_open_settles_them_as_a_failed_batch() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1", "ep2"]));
        snap.apply(&item_complete("ep1", 10.0));
        let terminal = batch_complete(true, false);
        let settled = settle(&mut snap, &terminal);
        snap.apply(&terminal);

        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0]["id"], "ep2");
        assert_eq!(
            settled[0]["error"],
            "Protocol error: missing completion event"
        );
        assert_eq!(snap.status, BatchStatus::Error);
        assert_eq!(snap.items["ep1"].status, ItemStatus::Completed);
        assert_eq!(snap.items["ep2"].status, ItemStatus::Error);
    }

    #[test]
    fn test_success_terminal_with_an_open_item_and_no_settlement_is_an_error() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1", "ep2"]));
        snap.apply(&item_complete("ep1", 10.0));
        snap.apply(&item_start("ep2"));

        // The supervisor settles before the terminal; without it ep2 is still open.
        snap.apply(&batch_complete(true, false));

        assert_eq!(snap.status, BatchStatus::Error);
        assert_eq!(snap.items["ep2"].status, ItemStatus::Processing);
    }

    #[test]
    fn test_settlement_cancels_open_items_in_batch_order_with_a_duration_only_if_started() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep3", "ep1", "ep2", "ep4"]));
        snap.apply(&item_complete("ep3", 1.0));
        snap.apply(&item_start("ep4"));
        snap.apply(&item_start("ep1"));

        let terminal = batch_complete(false, true);
        let settled = settle(&mut snap, &terminal);

        let ids: Vec<_> = settled.iter().map(|event| &event["id"]).collect();
        assert_eq!(ids, ["ep1", "ep2", "ep4"]);
        for event in &settled {
            assert_eq!(event["type"], "item_complete");
            assert_eq!(event["version"], 1);
            assert_eq!(event["status"], "cancelled");
            assert_eq!(event["reason"], CANCELLED_REASON);
            assert!(event["error"].is_null());
        }
        // ep1 and ep4 started, so each reports how long it ran; ep2 never did.
        for started in [&settled[0], &settled[2]] {
            assert!(started["duration_sec"]
                .as_f64()
                .is_some_and(|seconds| seconds >= 0.0));
        }
        assert!(settled[1].get("duration_sec").is_none());

        assert!(snap.items.values().all(|item| item.status.is_terminal()));
        assert_eq!(snap.completed_items(), 4);
        assert_eq!(snap.items["ep1"].reason.as_deref(), Some(CANCELLED_REASON));
        assert!(snap.items["ep1"].duration_sec.is_some());
        assert_eq!(snap.items["ep2"].duration_sec, None);
        snap.apply(&terminal);
        assert_eq!(snap.status, BatchStatus::Cancelled);
        // Nothing is left open, so a second settlement has nothing to say.
        assert!(snap.settlement_events(&terminal).is_empty());
    }

    #[test]
    fn test_settlement_error_is_the_runs_own_failure() {
        for (terminal, expected) in [
            (
                failed_with(serde_json::json!({ "error": "scan failed" })),
                "scan failed",
            ),
            (
                failed_with(serde_json::json!({ "panic": "worker died" })),
                "worker died",
            ),
            (
                failed_with(serde_json::json!({ "error": "scan failed", "panic": "worker died" })),
                "scan failed",
            ),
            // The supervisor's own terminal when the pipeline returned without a completion.
            (
                failed_with(serde_json::json!({
                    "error": "Pipeline ended without a batch completion event"
                })),
                "Pipeline ended without a batch completion event",
            ),
            (
                batch_complete(false, false),
                JobSnapshot::BATCH_FAILED_ERROR,
            ),
            (
                batch_complete(true, false),
                "Protocol error: missing completion event",
            ),
        ] {
            let mut snap = begun(10);
            snap.apply(&batch_start(&["ep2", "ep1"]));
            snap.apply(&item_start("ep2"));

            let settled = settle(&mut snap, &terminal);

            let ids: Vec<_> = settled.iter().map(|event| &event["id"]).collect();
            assert_eq!(ids, ["ep2", "ep1"], "{terminal:?}");
            for event in &settled {
                assert_eq!(event["status"], "error", "{terminal:?}");
                assert_eq!(event["error"], expected, "{terminal:?}");
                assert!(event["reason"].is_null(), "{terminal:?}");
            }
            assert!(settled[0]["duration_sec"].is_number());
            assert!(settled[1].get("duration_sec").is_none());
            assert!(snap.items.values().all(|item| item.status.is_terminal()));
            assert_eq!(snap.items["ep1"].error.as_deref(), Some(expected));
        }
    }

    /// Each snapshot is one job's, so another job's settlement cannot reach it: the supervisor's
    /// ledger is private to its job.
    #[test]
    fn test_settlement_events_are_only_for_a_running_snapshot_and_a_batch_complete() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1"]));
        let terminal = batch_complete(false, true);

        assert!(snap.settlement_events(&item_start("ep1")).is_empty());
        // Only a snapshot that is still running has items to settle.
        snap.apply(&terminal);
        assert!(snap.settlement_events(&terminal).is_empty());
    }

    /// A job that ends with `ep1` done, `ep2` and `ep3` started and `ep4` never started: the
    /// `terminal` and the `ItemComplete`s that settle `ep2` to `ep4`, serialized as the webview
    /// receives them (tagged by `attach_context`) but without `duration_sec`, the one field that
    /// differs from run to run.
    fn settlement_scenario(mut terminal: JobEvent) -> Value {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1", "ep2", "ep3", "ep4"]));
        snap.apply(&item_start("ep1"));
        snap.apply(&item_complete("ep1", 4.2));
        snap.apply(&item_start("ep2"));
        snap.apply(&item_start("ep3"));

        attach_context(&mut terminal, "remux", 10);
        let settled: Vec<Value> = snap
            .settlement_events(&terminal)
            .into_iter()
            .map(|mut event| {
                attach_context(&mut event, "remux", 10);
                let mut event = serde_json::to_value(event).unwrap();
                event.as_object_mut().unwrap().remove("duration_sec");
                event
            })
            .collect();
        serde_json::json!({ "terminal": terminal, "settled": settled })
    }

    /// Pins the Item Settlement wording across the language boundary. The fixture holds the
    /// events Rust sends for a stopped job and for a failed job whose terminal has no error or
    /// panic text; `queue-state.test.js` reads the same file and asserts that its guard, which
    /// settles an item the bridge left open, uses the same `reason` and `error`.
    ///
    /// Set `UPDATE_SETTLEMENT_FIXTURE=1` to rewrite the file from the real code path instead of
    /// comparing, then review the diff.
    #[test]
    fn test_settlement_wording_matches_the_golden_fixture() {
        const FIXTURE: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../desktop-tauri/ui/fixtures/settlement-events.json"
        );
        let built = serde_json::json!({
            "stopped": settlement_scenario(batch_complete(false, true)),
            "failed": settlement_scenario(batch_complete(false, false)),
        });

        if std::env::var("UPDATE_SETTLEMENT_FIXTURE").is_ok_and(|value| value == "1") {
            let path = std::path::Path::new(FIXTURE);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, serde_json::to_string_pretty(&built).unwrap() + "\n").unwrap();
            return;
        }
        let fixture: Value = serde_json::from_str(
            &std::fs::read_to_string(FIXTURE)
                .expect("read the settlement fixture; regenerate with UPDATE_SETTLEMENT_FIXTURE=1"),
        )
        .unwrap();
        assert_eq!(
            fixture, built,
            "settlement wording changed; regenerate with UPDATE_SETTLEMENT_FIXTURE=1"
        );
    }

    #[test]
    fn test_item_status_wire_names() {
        let statuses = [
            ItemStatus::Queued,
            ItemStatus::Processing,
            ItemStatus::Completed,
            ItemStatus::Skipped,
            ItemStatus::Error,
            ItemStatus::Cancelled,
            ItemStatus::Planned,
        ];
        assert_eq!(
            serde_json::to_value(statuses).unwrap(),
            serde_json::json!([
                "queued",
                "processing",
                "completed",
                "skipped",
                "error",
                "cancelled",
                "planned"
            ])
        );
    }

    #[test]
    fn test_live_snapshot_retains_only_active_order_and_clamped_progress() {
        let mut snap = begun(10);
        let start_time_ms = snap.start_time_ms;
        snap.apply(&batch_start(&["ep2", "ep1"]));
        for id in ["ep3", "ep2"] {
            snap.apply(&item_start(id));
        }
        for progress in [150.0, -8.0, 70.0] {
            snap.apply(&item_progress("ep2", progress));
            assert_eq!(snap.items["ep2"].progress, Some(progress.clamp(0.0, 100.0)));
        }

        assert_eq!(snap.start_time_ms, start_time_ms);
        assert_eq!(
            snap.ordered_item_ids,
            vec!["ep2".to_string(), "ep1".to_string()]
        );
        assert_eq!(snap.items.len(), 2);
        assert_eq!(snap.input_dir, "/anime");
        assert!(!snap.items.contains_key("ep3"));
        assert_eq!(snap.items["ep2"].status, ItemStatus::Processing);
        assert!(snap.items["ep2"].start_time_ms.is_some());
        assert_eq!(snap.items["ep1"].status, ItemStatus::Queued);
        assert_eq!(snap.items["ep1"].file, "ep1.mkv");
        assert_eq!(snap.items["ep1"].start_time_ms, None);
        let value = serde_json::to_value(snap).unwrap();
        assert_eq!(value["ordered_item_ids"], serde_json::json!(["ep2", "ep1"]));
        assert_eq!(value["items"]["ep2"]["progress"], 70.0);
        assert!(value["items"]["ep2"]["start_time_ms"].is_number());
    }

    #[test]
    fn test_batch_complete_ends_a_snapshot_whose_open_items_were_settled() {
        let mut cancelled = begun(1);
        cancelled.apply(&batch_start(&["ep1"]));
        cancelled.apply(&item_start("ep1"));
        let terminal = batch_complete(false, true);
        settle(&mut cancelled, &terminal);
        cancelled.apply(&terminal);
        assert_eq!(cancelled.status, BatchStatus::Cancelled);
        assert_eq!(cancelled.items["ep1"].status, ItemStatus::Cancelled);
        assert_eq!(
            cancelled.items["ep1"].reason.as_deref(),
            Some(CANCELLED_REASON)
        );

        let mut failed = begun(2);
        failed.apply(&batch_start(&["ep1"]));
        failed.apply(&item_start("ep1"));
        let terminal = failed_with(serde_json::json!({
            "error": "Fatal error",
        }));
        settle(&mut failed, &terminal);
        failed.apply(&terminal);
        assert_eq!(failed.status, BatchStatus::Error);
        assert_eq!(failed.items["ep1"].status, ItemStatus::Error);
        assert_eq!(failed.items["ep1"].error.as_deref(), Some("Fatal error"));
    }

    #[test]
    fn test_terminal_status() {
        // Cancelled takes precedence over success and failed items
        assert_eq!(terminal_status(false, true, false), BatchStatus::Cancelled);
        assert_eq!(terminal_status(true, true, false), BatchStatus::Cancelled);
        assert_eq!(terminal_status(true, true, true), BatchStatus::Cancelled);
        assert_eq!(terminal_status(false, true, true), BatchStatus::Cancelled);

        // Success with no failed item
        assert_eq!(terminal_status(true, false, false), BatchStatus::Completed);

        // Success with a failed item (a settled protocol gap) or one still open (the backstop):
        // downgraded to error
        assert_eq!(terminal_status(true, false, true), BatchStatus::Error);

        // Failure
        assert_eq!(terminal_status(false, false, false), BatchStatus::Error);
        assert_eq!(terminal_status(false, false, true), BatchStatus::Error);
    }

    /// An `ItemComplete` of `id` with outcome `status` and no reason or duration.
    fn bare_outcome(id: &str, status: ItemOutcome) -> JobEvent {
        JobEvent::ItemComplete {
            version: EVENT_VERSION,
            id: ItemId::parse(id),
            file: format!("{id}.mkv"),
            status,
            reason: None,
            error: None,
            duration_sec: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn test_reducer_drops_an_event_of_another_version() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1"]));
        let mut start = item_start("ep1");
        let JobEvent::ItemStart { version, .. } = &mut start else {
            unreachable!()
        };
        *version = EVENT_VERSION + 1;
        snap.apply(&start);
        assert_eq!(snap.items["ep1"].status, ItemStatus::Queued);

        // Not even a `BatchStart` of another version replaces the batch.
        let mut next = batch_start(&["ep9"]);
        let JobEvent::BatchStart { version, .. } = &mut next else {
            unreachable!()
        };
        *version = EVENT_VERSION + 1;
        snap.apply(&next);
        assert_eq!(snap.ordered_item_ids, ["ep1"]);
    }

    /// Events of another job never reach this snapshot: the supervisor's ledger is its own job's,
    /// and the actor drops them (`test_a_later_job_replaces_the_snapshot_and_stale_events_are_dropped`).
    #[test]
    fn test_reducer_drops_events_after_the_batch() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1", "ep2"]));

        snap.apply(&batch_complete(false, false));
        snap.apply(&item_start("ep2"));
        snap.apply(&item_complete("ep2", 1.0));
        assert_eq!(snap.items["ep2"].status, ItemStatus::Queued);
        assert_eq!(snap.status, BatchStatus::Error);
    }

    #[test]
    fn test_reducer_applies_progress_only_to_processing_items_with_finite_values() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1", "ep2"]));
        // ep1 is still queued, so its progress is dropped.
        snap.apply(&item_progress("ep1", 30.0));
        assert_eq!(snap.items["ep1"].progress, None);

        snap.apply(&item_start("ep2"));
        snap.apply(&item_progress("ep2", 40.0));
        for progress in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            snap.apply(&item_progress("ep2", progress));
        }
        assert_eq!(snap.items["ep2"].progress, Some(40.0));
    }

    #[test]
    fn test_reducer_item_start_renames_the_items_file() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1"]));
        snap.apply(&JobEvent::ItemStart {
            version: EVENT_VERSION,
            id: ItemId::parse("ep1"),
            file: "Show - 01.mkv".into(),
            extra: Default::default(),
        });
        assert_eq!(snap.items["ep1"].file, "Show - 01.mkv");
    }

    #[test]
    fn test_reducer_falls_back_to_the_monotonic_start_for_a_missing_duration() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1", "ep2", "ep3"]));
        snap.apply(&item_start("ep1"));
        snap.apply(&item_start("ep3"));
        snap.apply(&bare_outcome("ep1", ItemOutcome::Completed));
        // An item that never started has nothing to measure from.
        snap.apply(&bare_outcome("ep2", ItemOutcome::Skipped));
        // A duration the event carries wins over the fallback.
        snap.apply(&item_complete("ep3", 42.5));

        assert!(snap.items["ep1"]
            .duration_sec
            .is_some_and(|seconds| seconds >= 0.0));
        assert_eq!(snap.items["ep2"].duration_sec, None);
        assert_eq!(snap.items["ep3"].duration_sec, Some(42.5));
    }

    #[test]
    fn test_reducer_derives_completed_items_from_item_outcomes() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1", "ep2", "ep3"]));
        snap.apply(&item_start("ep1"));
        snap.apply(&item_complete("ep1", 1.0));
        snap.apply(&bare_outcome("ep2", ItemOutcome::Error));
        // An outcome for an item the batch never listed counts for nothing.
        snap.apply(&bare_outcome("ep9", ItemOutcome::Completed));

        assert_eq!(snap.completed_items(), 2);
        // The webview's restore path still reads the count from the payload.
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["completed_items"], 2);
        assert_eq!(json["total_items"], 3);
        // The hand-written `Serialize` lists the fields: pin the key set so none drops out.
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "completed_items",
                "effective_settings",
                "input_dir",
                "items",
                "job_id",
                "mode",
                "ordered_item_ids",
                "start_time_ms",
                "status",
                "total_items",
            ]
        );
    }

    #[test]
    fn test_reducer_stores_no_default_reason() {
        let mut snap = begun(10);
        snap.apply(&batch_start(&["ep1", "ep2"]));
        snap.apply(&bare_outcome("ep1", ItemOutcome::Skipped));
        snap.apply(&bare_outcome("ep2", ItemOutcome::Cancelled));

        assert_eq!(snap.items["ep1"].reason, None);
        assert_eq!(snap.items["ep2"].reason, None);
    }
}
