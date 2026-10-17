//! The webview side of the Desktop Core: the `Publisher` adapter that emits the View State, as a
//! whole or as a patch, each Run Log batch and a job's completion to the webview.

use crate::view_patch::view_patch;
use mkv_desktop_core::actor::{Publisher, ViewState};
use mkv_desktop_core::job::JobDone;
use mkv_desktop_core::log::LogBatch;
use std::sync::{Mutex, PoisonError};
use tauri::{AppHandle, Emitter, Runtime};

const VIEW_STATE: &str = "view-state";
const JOB_LOG: &str = "job-log";
const JOB_DONE: &str = "job-done";

/// Emits what the core publishes to the webview as `view-state`, `job-log` and `job-done`.
pub(crate) struct WebviewPublisher<R: Runtime> {
    app: AppHandle<R>,
    /// The View State last emitted, which the next one is diffed against: `None` until the
    /// first, which goes out whole.
    last: Mutex<Option<ViewState>>,
}

impl<R: Runtime> WebviewPublisher<R> {
    pub(crate) fn new(app: AppHandle<R>) -> Self {
        Self {
            app,
            last: Mutex::new(None),
        }
    }
}

impl<R: Runtime> Publisher for WebviewPublisher<R> {
    /// Emits the patch from the last emission, or the whole state when there is none or the
    /// patch does not serialize. A patch the webview cannot apply makes it read `get_state`.
    fn state(&self, state: &ViewState) {
        // A panic while the lock was held leaves `last` as it was: at worst the next patch has
        // a base the webview never saw, and it resyncs.
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = match last.as_ref().map(|prev| view_patch(prev, state)) {
            Some(Ok(patch)) => self.app.emit(VIEW_STATE, patch),
            _ => self.app.emit(VIEW_STATE, state),
        };
        // The rows are shared, so this copy costs no row.
        *last = Some(state.clone());
    }

    fn logs(&self, batch: &LogBatch) {
        let _ = self.app.emit(JOB_LOG, batch);
    }

    fn done(&self, done: &JobDone) {
        let _ = self.app.emit(JOB_DONE, done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mkv_core::log_line::LogLine;
    use mkv_core::pipeline::PipelineMode;
    use mkv_desktop_core::log::RunLog;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use tauri::test::MockRuntime;
    use tauri::Listener;

    type Recorder<T> = Arc<Mutex<Vec<T>>>;

    fn record<T: serde::de::DeserializeOwned + Send + 'static>(
        app: &tauri::App<MockRuntime>,
        event: &str,
    ) -> Recorder<T> {
        let recorder = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&recorder);
        app.listen(event, move |event| {
            writer
                .lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        recorder
    }

    #[test]
    fn the_webview_publisher_emits_the_view_state_whole_then_as_patches_and_the_two_job_events() {
        let app = tauri::test::mock_app();
        let states = record::<Value>(&app, VIEW_STATE);
        let logs = record::<Value>(&app, JOB_LOG);
        let dones = record::<Value>(&app, JOB_DONE);
        let publisher = WebviewPublisher::new(app.handle().clone());

        let state = ViewState {
            seq: 1,
            ..ViewState::new(None)
        };
        publisher.state(&state);
        let busy = ViewState {
            seq: 2,
            busy: true,
            ..state.clone()
        };
        publisher.state(&busy);
        publisher.state(&ViewState { seq: 3, ..busy });
        let mut log = RunLog::new(PipelineMode::Extract);
        publisher.logs(&log.push(&[
            LogLine::note("probing /anime"),
            LogLine::error("ERROR: Pipeline failed: x"),
        ]));
        publisher.done(&JobDone {
            mode: "remux",
            job_id: 7,
            code: -1,
        });

        // The first emission is whole, with no `base`; each later one patches the one before.
        assert_eq!(
            *states.lock().unwrap(),
            [
                serde_json::to_value(&state).unwrap(),
                serde_json::json!({ "seq": 2, "base": 1, "busy": true }),
                serde_json::json!({ "seq": 3, "base": 2 }),
            ]
        );
        assert_eq!(
            *logs.lock().unwrap(),
            [serde_json::json!({
                "mode": "extract",
                "generation": 0,
                "first": 0,
                "keep_from": 0,
                "errors": 1,
                "lines": [
                    { "kind": "note", "text": "probing /anime", "display": "probing anime" },
                    { "kind": "error", "text": "ERROR: Pipeline failed: x" },
                ]
            })]
        );
        assert_eq!(
            *dones.lock().unwrap(),
            [serde_json::json!({ "mode": "remux", "job_id": 7, "code": -1 })]
        );
    }
}
