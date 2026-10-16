//! The webview side of the Desktop Core: the `Publisher` adapter that emits the View State, each
//! Run Log batch and a job's completion to the webview.

use mkv_desktop_core::actor::{Publisher, ViewState};
use mkv_desktop_core::job::JobDone;
use mkv_desktop_core::log::LogBatch;
use tauri::{AppHandle, Emitter, Runtime};

const VIEW_STATE: &str = "view-state";
const JOB_LOG: &str = "job-log";
const JOB_DONE: &str = "job-done";

/// Emits what the core publishes to the webview as `view-state`, `job-log` and `job-done`.
pub(crate) struct WebviewPublisher<R: Runtime>(pub(crate) AppHandle<R>);

impl<R: Runtime> Publisher for WebviewPublisher<R> {
    fn state(&self, state: &ViewState) {
        let _ = self.0.emit(VIEW_STATE, state);
    }

    fn logs(&self, batch: &LogBatch) {
        let _ = self.0.emit(JOB_LOG, batch);
    }

    fn done(&self, done: &JobDone) {
        let _ = self.0.emit(JOB_DONE, done);
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
    fn the_webview_publisher_emits_the_view_state_and_the_two_job_events() {
        let app = tauri::test::mock_app();
        let states = record::<Value>(&app, VIEW_STATE);
        let logs = record::<Value>(&app, JOB_LOG);
        let dones = record::<Value>(&app, JOB_DONE);
        let publisher = WebviewPublisher(app.handle().clone());

        let state = ViewState {
            seq: 1,
            ..ViewState::new(None)
        };
        publisher.state(&state);
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

        assert_eq!(
            *states.lock().unwrap(),
            [serde_json::to_value(&state).unwrap()]
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
