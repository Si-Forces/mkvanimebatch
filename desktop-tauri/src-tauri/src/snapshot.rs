//! The Job Snapshot: the in-memory projection of the one job the app runs at a time, reduced
//! from its `JobEvent`s so a webview reload can rebuild the queue.

use mkv_core::item_id::ItemId;
use mkv_core::pipeline::{ItemOutcome, JobEvent, CANCELLED_REASON, EVENT_VERSION};
use mkv_core::request::EffectiveSettings;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
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

#[derive(Debug, Clone, Serialize)]
pub struct JobSnapshot {
    pub job_id: u64,
    pub mode: String,
    pub status: BatchStatus,
    pub start_time_ms: u64,
    pub duration_sec: Option<f64>,
    pub total_items: usize,
    pub completed_items: usize,
    pub input_dir: Option<String>,
    /// What the job runs with, fixed when it starts. `None` only for a snapshot that no
    /// `SnapshotStore::begin` preceded.
    pub effective_settings: Option<EffectiveSettings>,
    pub ordered_item_ids: Vec<String>,
    pub items: HashMap<String, ItemSnapshot>,
}

fn timestamp_ms() -> u64 {
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

/// The one Job Snapshot the app holds, behind one lock. `JobState` owns it and decides when it
/// may be cleared.
#[derive(Default)]
pub(crate) struct SnapshotStore(Mutex<Option<JobSnapshot>>);

impl SnapshotStore {
    pub(crate) fn begin(
        &self,
        job_id: u64,
        mode: &str,
        input_dir: String,
        effective_settings: EffectiveSettings,
    ) -> Result<(), String> {
        *self.0.lock().map_err(|error| error.to_string())? = Some(JobSnapshot {
            job_id,
            mode: mode.to_string(),
            status: BatchStatus::Running,
            start_time_ms: timestamp_ms(),
            duration_sec: None,
            total_items: 0,
            completed_items: 0,
            input_dir: Some(input_dir),
            effective_settings: Some(effective_settings),
            ordered_item_ids: Vec::new(),
            items: HashMap::new(),
        });
        Ok(())
    }

    pub(crate) fn update(&self, event: &JobEvent, job_id: u64, mode: &str) {
        if let Ok(mut snap) = self.0.lock() {
            if !matches!(event, JobEvent::BatchStart { .. })
                && !snap.as_ref().is_some_and(|snapshot| {
                    snapshot.job_id == job_id && snapshot.status == BatchStatus::Running
                })
            {
                return;
            }
            match event {
                JobEvent::BatchStart { total, items, .. } => {
                    let start_time_ms = snap
                        .as_ref()
                        .filter(|snapshot| snapshot.job_id == job_id)
                        .map(|snapshot| snapshot.start_time_ms)
                        .unwrap_or_else(timestamp_ms);
                    let mut snapshot_items = HashMap::new();
                    let mut ordered_item_ids = Vec::new();
                    if let Some(items) = items {
                        for item in items {
                            let id = item.id.to_string();
                            if snapshot_items.contains_key(&id) {
                                continue;
                            }
                            ordered_item_ids.push(id.clone());
                            snapshot_items.insert(
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
                    // Only this job's own snapshot, begun by `JobState::start`, carries over.
                    let own = snap.as_ref().filter(|snapshot| snapshot.job_id == job_id);
                    *snap = Some(JobSnapshot {
                        job_id,
                        mode: mode.to_string(),
                        status: BatchStatus::Running,
                        start_time_ms,
                        duration_sec: None,
                        total_items: *total,
                        completed_items: 0,
                        input_dir: own.and_then(|snapshot| snapshot.input_dir.clone()),
                        effective_settings: own
                            .and_then(|snapshot| snapshot.effective_settings.clone()),
                        ordered_item_ids,
                        items: snapshot_items,
                    });
                }
                JobEvent::ItemStart { id, file, .. } => {
                    if let Some(item) = snap
                        .as_mut()
                        .and_then(|snapshot| snapshot.items.get_mut(id.as_str()))
                    {
                        if !item.status.is_terminal() {
                            item.file = file.clone();
                            item.status = ItemStatus::Processing;
                            item.start_time_ms.get_or_insert_with(timestamp_ms);
                            item.started_at.get_or_insert_with(Instant::now);
                        }
                    }
                }
                JobEvent::ItemProgress { id, progress, .. } => {
                    if let Some(item) = snap
                        .as_mut()
                        .and_then(|snapshot| snapshot.items.get_mut(id.as_str()))
                    {
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
                    if let Some(snapshot) = snap.as_mut() {
                        if let Some(item) = snapshot.items.get_mut(id.as_str()) {
                            if !item.status.is_terminal() {
                                item.file = file.clone();
                                item.status = (*status).into();
                                item.outcome = Some(*status);
                                item.duration_sec = *duration_sec;
                                item.reason = reason.clone();
                                item.error = error.clone();
                                snapshot.completed_items += 1;
                            }
                        }
                    }
                }
                JobEvent::BatchComplete {
                    success,
                    cancelled,
                    extra,
                    ..
                } => {
                    if let Some(snapshot) = snap.as_mut() {
                        // The flag trips on any error item, which fails a batch the engine called
                        // a success (the supervisor settled one of its items as an error). As a
                        // backstop it also trips on any item still open, which Item Settlement
                        // (`settlement_events`) should have closed, so a success terminal can
                        // never yield Completed (code 0) with open items.
                        let has_failed_items = snapshot.items.values().any(|item| {
                            item.status == ItemStatus::Error || !item.status.is_terminal()
                        });
                        snapshot.status = terminal_status(*success, *cancelled, has_failed_items);
                        snapshot.duration_sec =
                            extra.get("duration_sec").and_then(|value| value.as_f64());
                    }
                }
            }
        }
    }

    /// The error of an item settled by a failed job whose terminal carries no error or panic text.
    /// The webview's guard (`queue-state.js`) repeats it for a run that broke Item Settlement;
    /// `test_settlement_wording_matches_the_golden_fixture` pins both to one fixture.
    const BATCH_FAILED_ERROR: &'static str = "Batch failed";

    /// Item Settlement: the `ItemComplete` events that end every item of job `job_id` that the
    /// Batch Pipeline left open (queued or processing) when `terminal` ends the job, in batch
    /// order. They are cancelled when `terminal` is a stop, else errors carrying the run's own
    /// failure: its error, else its panic, else `BATCH_FAILED_ERROR`; a success `terminal` has no
    /// failure of its own, so its open items carry a protocol error. An item that had started
    /// reports how long it ran, on a monotonic clock; one that never started has no duration.
    ///
    /// The supervisor applies and emits them before `terminal`, so the webview and a reloaded
    /// snapshot see the same events. Empty for a `terminal` that is not a `BatchComplete` and for
    /// a snapshot that is not job `job_id`'s running one.
    pub(crate) fn settlement_events(&self, job_id: u64, terminal: &JobEvent) -> Vec<JobEvent> {
        let JobEvent::BatchComplete {
            success,
            cancelled,
            extra,
            ..
        } = terminal
        else {
            return Vec::new();
        };
        let Ok(snap) = self.0.lock() else {
            return Vec::new();
        };
        let Some(snapshot) = snap.as_ref().filter(|snapshot| {
            snapshot.job_id == job_id && snapshot.status == BatchStatus::Running
        }) else {
            return Vec::new();
        };
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
        snapshot
            .ordered_item_ids
            .iter()
            .filter_map(|id| snapshot.items.get(id))
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

    /// The status of job `job_id`'s snapshot, `None` when the snapshot is another job's or empty.
    pub(crate) fn status_of(&self, job_id: u64) -> Option<BatchStatus> {
        self.0.lock().ok().and_then(|snapshot| {
            snapshot
                .as_ref()
                .filter(|item| item.job_id == job_id)
                .map(|item| item.status)
        })
    }

    pub(crate) fn get(&self) -> Result<Option<JobSnapshot>, String> {
        Ok(self.0.lock().map_err(|e| e.to_string())?.clone())
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut snap) = self.0.lock() {
            *snap = None;
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use mkv_core::item_id::ItemId;
    use mkv_core::pipeline::{BatchItemInfo, ItemOutcome, JobEvent};
    use mkv_core::request::EffectiveSettings;

    /// Effective Settings that differ from every default, so a test sees them pass through
    /// unchanged.
    pub(crate) fn effective_settings() -> EffectiveSettings {
        EffectiveSettings {
            workers: 3,
            io_jobs: 5,
            ffmpeg_threads: 2,
            bitrate: "256k".into(),
            complexity: 8,
        }
    }

    /// A `BatchStart` listing `ids` in engine order; each item's file is `<id>.mkv`.
    pub(crate) fn batch_start(ids: &[&str]) -> JobEvent {
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

    pub(crate) fn item_start(id: &str) -> JobEvent {
        JobEvent::ItemStart {
            version: 1,
            id: ItemId::parse(id),
            file: format!("{id}.mkv"),
            extra: Default::default(),
        }
    }

    pub(crate) fn item_progress(id: &str, progress: f64) -> JobEvent {
        JobEvent::ItemProgress {
            version: 1,
            id: ItemId::parse(id),
            file: format!("{id}.mkv"),
            progress,
            speed: None,
            eta: None,
            extra: Default::default(),
        }
    }

    pub(crate) fn item_complete(id: &str, duration_sec: f64) -> JobEvent {
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

    pub(crate) fn batch_complete(success: bool, cancelled: bool) -> JobEvent {
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

    fn snapshot(store: &SnapshotStore) -> JobSnapshot {
        store.get().unwrap().unwrap()
    }

    #[test]
    fn test_snapshot_lifecycle() {
        let store = SnapshotStore::default();
        store.update(&batch_start(&["ep1"]), 10, "remux");
        let snap = snapshot(&store);
        assert_eq!(snap.job_id, 10);
        assert_eq!(snap.mode, "remux");
        assert_eq!(snap.status, BatchStatus::Running);
        assert_eq!(snap.total_items, 1);
        assert_eq!(snap.completed_items, 0);
        assert_eq!(snap.ordered_item_ids, vec!["ep1".to_string()]);

        store.update(&item_start("ep1"), 10, "remux");
        assert_eq!(snapshot(&store).items["ep1"].status, ItemStatus::Processing);

        let complete = item_complete("ep1", 42.5);
        store.update(&complete, 10, "remux");
        let snap = snapshot(&store);
        assert_eq!(snap.completed_items, 1);
        let item = &snap.items["ep1"];
        assert_eq!(item.status, ItemStatus::Completed);
        assert_eq!(item.duration_sec, Some(42.5));
        assert_eq!(item.outcome, Some(ItemOutcome::Completed));

        // A duplicate completion must not count the item twice.
        store.update(&complete, 10, "remux");
        assert_eq!(snapshot(&store).completed_items, 1);

        store.update(&batch_complete(true, false), 10, "remux");
        let snap = snapshot(&store);
        assert_eq!(snap.status, BatchStatus::Completed);
        assert_eq!(snap.duration_sec, None);

        store.clear();
        assert!(store.get().unwrap().is_none());
    }

    #[test]
    fn test_snapshot_carries_the_effective_settings_of_its_job() {
        let store = SnapshotStore::default();
        let settings = Some(effective_settings());
        store
            .begin(7, "remux", "/anime".into(), effective_settings())
            .unwrap();
        assert_eq!(snapshot(&store).effective_settings, settings);

        // Neither the engine's BatchStart nor the terminal event replaces them.
        store.update(&batch_start(&["ep1"]), 7, "remux");
        assert_eq!(snapshot(&store).effective_settings, settings);
        store.update(&batch_complete(true, false), 7, "remux");
        assert_eq!(snapshot(&store).effective_settings, settings);

        let json = serde_json::to_value(snapshot(&store)).unwrap();
        assert_eq!(json["effective_settings"]["io_jobs"], 5);
        assert_eq!(json["effective_settings"]["bitrate"], "256k");

        // A later job inherits neither them nor the input directory from the snapshot it
        // replaces.
        store.update(&batch_start(&["ep1"]), 8, "remux");
        assert_eq!(snapshot(&store).effective_settings, None);
        assert_eq!(snapshot(&store).input_dir, None);
    }

    /// Applies the settlement of `terminal` to job `job_id` as the supervisor does, and returns
    /// the events as the webview receives them.
    fn settle(store: &SnapshotStore, job_id: u64, terminal: &JobEvent) -> Vec<Value> {
        let events = store.settlement_events(job_id, terminal);
        for event in &events {
            store.update(event, job_id, "remux");
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
        let store = SnapshotStore::default();
        store.update(&batch_start(&["ep1", "ep2"]), 10, "remux");
        store.update(&item_complete("ep1", 10.0), 10, "remux");
        let terminal = batch_complete(true, false);
        let settled = settle(&store, 10, &terminal);
        store.update(&terminal, 10, "remux");

        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0]["id"], "ep2");
        assert_eq!(
            settled[0]["error"],
            "Protocol error: missing completion event"
        );
        let snap = snapshot(&store);
        assert_eq!(snap.status, BatchStatus::Error);
        assert_eq!(snap.items["ep1"].status, ItemStatus::Completed);
        assert_eq!(snap.items["ep2"].status, ItemStatus::Error);
    }

    #[test]
    fn test_success_terminal_with_an_open_item_and_no_settlement_is_an_error() {
        let store = SnapshotStore::default();
        store.update(&batch_start(&["ep1", "ep2"]), 10, "remux");
        store.update(&item_complete("ep1", 10.0), 10, "remux");
        store.update(&item_start("ep2"), 10, "remux");

        // The supervisor settles before the terminal; without it ep2 is still open.
        store.update(&batch_complete(true, false), 10, "remux");

        let snap = snapshot(&store);
        assert_eq!(snap.status, BatchStatus::Error);
        assert_eq!(snap.items["ep2"].status, ItemStatus::Processing);
    }

    #[test]
    fn test_settlement_cancels_open_items_in_batch_order_with_a_duration_only_if_started() {
        let store = SnapshotStore::default();
        store.update(&batch_start(&["ep3", "ep1", "ep2", "ep4"]), 10, "remux");
        store.update(&item_complete("ep3", 1.0), 10, "remux");
        store.update(&item_start("ep4"), 10, "remux");
        store.update(&item_start("ep1"), 10, "remux");

        let terminal = batch_complete(false, true);
        let settled = settle(&store, 10, &terminal);

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

        let snap = snapshot(&store);
        assert!(snap.items.values().all(|item| item.status.is_terminal()));
        assert_eq!(snap.completed_items, 4);
        assert_eq!(snap.items["ep1"].reason.as_deref(), Some(CANCELLED_REASON));
        assert!(snap.items["ep1"].duration_sec.is_some());
        assert_eq!(snap.items["ep2"].duration_sec, None);
        store.update(&terminal, 10, "remux");
        assert_eq!(snapshot(&store).status, BatchStatus::Cancelled);
        // Nothing is left open, so a second settlement has nothing to say.
        assert!(store.settlement_events(10, &terminal).is_empty());
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
                SnapshotStore::BATCH_FAILED_ERROR,
            ),
            (
                batch_complete(true, false),
                "Protocol error: missing completion event",
            ),
        ] {
            let store = SnapshotStore::default();
            store.update(&batch_start(&["ep2", "ep1"]), 10, "remux");
            store.update(&item_start("ep2"), 10, "remux");

            let settled = settle(&store, 10, &terminal);

            let ids: Vec<_> = settled.iter().map(|event| &event["id"]).collect();
            assert_eq!(ids, ["ep2", "ep1"], "{terminal:?}");
            for event in &settled {
                assert_eq!(event["status"], "error", "{terminal:?}");
                assert_eq!(event["error"], expected, "{terminal:?}");
                assert!(event["reason"].is_null(), "{terminal:?}");
            }
            assert!(settled[0]["duration_sec"].is_number());
            assert!(settled[1].get("duration_sec").is_none());
            let snap = snapshot(&store);
            assert!(snap.items.values().all(|item| item.status.is_terminal()));
            assert_eq!(snap.items["ep1"].error.as_deref(), Some(expected));
        }
    }

    #[test]
    fn test_settlement_events_are_only_for_the_running_job_and_a_batch_complete() {
        let store = SnapshotStore::default();
        store.update(&batch_start(&["ep1"]), 10, "remux");
        let terminal = batch_complete(false, true);

        assert!(store.settlement_events(11, &terminal).is_empty());
        assert!(store.settlement_events(10, &item_start("ep1")).is_empty());
        // Only a snapshot that is still running has items to settle.
        store.update(&terminal, 10, "remux");
        assert!(store.settlement_events(10, &terminal).is_empty());
    }

    /// A job that ends with `ep1` done, `ep2` and `ep3` started and `ep4` never started: the
    /// `terminal` and the `ItemComplete`s that settle `ep2` to `ep4`, serialized as the webview
    /// receives them (tagged by `attach_context`) but without `duration_sec`, the one field that
    /// differs from run to run.
    fn settlement_scenario(mut terminal: JobEvent) -> Value {
        let store = SnapshotStore::default();
        store.update(&batch_start(&["ep1", "ep2", "ep3", "ep4"]), 10, "remux");
        store.update(&item_start("ep1"), 10, "remux");
        store.update(&item_complete("ep1", 4.2), 10, "remux");
        store.update(&item_start("ep2"), 10, "remux");
        store.update(&item_start("ep3"), 10, "remux");

        attach_context(&mut terminal, "remux", 10);
        let settled: Vec<Value> = store
            .settlement_events(10, &terminal)
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
            "/../ui/fixtures/settlement-events.json"
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
        let store = SnapshotStore::default();
        store
            .begin(10, "remux", "/anime".into(), effective_settings())
            .unwrap();
        let start_time_ms = snapshot(&store).start_time_ms;
        store.update(&batch_start(&["ep2", "ep1"]), 10, "remux");
        for id in ["ep3", "ep2"] {
            store.update(&item_start(id), 10, "remux");
        }
        for progress in [150.0, -8.0, 70.0] {
            store.update(&item_progress("ep2", progress), 10, "remux");
            assert_eq!(
                snapshot(&store).items["ep2"].progress,
                Some(progress.clamp(0.0, 100.0))
            );
        }

        let snapshot = snapshot(&store);
        assert_eq!(snapshot.start_time_ms, start_time_ms);
        assert_eq!(
            snapshot.ordered_item_ids,
            vec!["ep2".to_string(), "ep1".to_string()]
        );
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.input_dir.as_deref(), Some("/anime"));
        assert!(!snapshot.items.contains_key("ep3"));
        assert_eq!(snapshot.items["ep2"].status, ItemStatus::Processing);
        assert!(snapshot.items["ep2"].start_time_ms.is_some());
        assert_eq!(snapshot.items["ep1"].status, ItemStatus::Queued);
        assert_eq!(snapshot.items["ep1"].file, "ep1.mkv");
        assert_eq!(snapshot.items["ep1"].start_time_ms, None);
        let value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["ordered_item_ids"], serde_json::json!(["ep2", "ep1"]));
        assert_eq!(value["items"]["ep2"]["progress"], 70.0);
        assert!(value["items"]["ep2"]["start_time_ms"].is_number());
    }

    #[test]
    fn test_batch_complete_ends_a_snapshot_whose_open_items_were_settled() {
        let cancelled = SnapshotStore::default();
        cancelled.update(&batch_start(&["ep1"]), 1, "remux");
        cancelled.update(&item_start("ep1"), 1, "remux");
        let terminal = batch_complete(false, true);
        settle(&cancelled, 1, &terminal);
        cancelled.update(&terminal, 1, "remux");
        let snap = snapshot(&cancelled);
        assert_eq!(snap.status, BatchStatus::Cancelled);
        assert_eq!(snap.items["ep1"].status, ItemStatus::Cancelled);
        assert_eq!(snap.items["ep1"].reason.as_deref(), Some(CANCELLED_REASON));

        let failed = SnapshotStore::default();
        failed.update(&batch_start(&["ep1"]), 2, "remux");
        failed.update(&item_start("ep1"), 2, "remux");
        let terminal = failed_with(serde_json::json!({
            "error": "Fatal error",
            "duration_sec": 18.25,
        }));
        settle(&failed, 2, &terminal);
        failed.update(&terminal, 2, "remux");
        let snap = snapshot(&failed);
        assert_eq!(snap.status, BatchStatus::Error);
        assert_eq!(snap.duration_sec, Some(18.25));
        assert_eq!(snap.items["ep1"].status, ItemStatus::Error);
        assert_eq!(snap.items["ep1"].error.as_deref(), Some("Fatal error"));
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
}
