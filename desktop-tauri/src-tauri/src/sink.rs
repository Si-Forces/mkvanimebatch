//! The webview side of a job: the `JobSink` adapter that emits a job's events, log batches and
//! completion to the webview.

use mkv_core::log_line::LogLine;
use mkv_core::pipeline::JobEvent;
use mkv_desktop_core::job::{JobDone, JobSink};
use tauri::{AppHandle, Emitter, Runtime};

const JOB_EVENT: &str = "job-event";
const JOB_LOG: &str = "job-log";
const JOB_DONE: &str = "job-done";

/// Emits what a job reports to the webview as `job-event`, `job-log` and `job-done`.
pub(crate) struct WebviewSink<R: Runtime>(pub(crate) AppHandle<R>);

impl<R: Runtime> JobSink for WebviewSink<R> {
    fn event(&self, event: &JobEvent) {
        let _ = self.0.emit(JOB_EVENT, event);
    }

    fn logs(&self, lines: &[LogLine]) {
        let _ = self.0.emit(JOB_LOG, lines);
    }

    fn done(&self, done: &JobDone) {
        let _ = self.0.emit(JOB_DONE, done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mkv_desktop_core::snapshot::test_support::batch_start;
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
    fn the_app_handle_sink_emits_the_three_job_events() {
        let app = tauri::test::mock_app();
        let events = record::<Value>(&app, JOB_EVENT);
        let logs = record::<Value>(&app, JOB_LOG);
        let dones = record::<Value>(&app, JOB_DONE);
        let sink = WebviewSink(app.handle().clone());

        JobSink::event(&sink, &batch_start(&["ep1"]));
        JobSink::logs(
            &sink,
            &[
                LogLine::note("probing"),
                LogLine::error("ERROR: Pipeline failed: x"),
            ],
        );
        JobSink::done(
            &sink,
            &JobDone {
                mode: "remux",
                job_id: 7,
                code: -1,
            },
        );

        assert_eq!(events.lock().unwrap()[0]["type"], "batch_start");
        assert_eq!(
            *logs.lock().unwrap(),
            [serde_json::json!([
                { "kind": "note", "text": "probing" },
                { "kind": "error", "text": "ERROR: Pipeline failed: x" },
            ])]
        );
        assert_eq!(
            *dones.lock().unwrap(),
            [serde_json::json!({ "mode": "remux", "job_id": 7, "code": -1 })]
        );
    }
}
