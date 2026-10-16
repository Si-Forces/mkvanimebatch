use mkv_core::request::{BatchRequest, EffectiveSettings};
use mkv_desktop_core::actor::{CoreHandle, Intent, ViewState};
use mkv_desktop_core::control::{launch, resolve_canonical, InspectCacheState};
use mkv_desktop_core::job::JobState;
use tauri::State;

pub(crate) const CLEANUP_ERROR: &str = "cleanup-error";

/// The Effective Settings `run_job` would run `request` with, or the reason it would refuse it.
/// Synchronous and slot-free, so the UI can check a request before it starts its preflight scan.
#[tauri::command]
pub fn resolve_batch_request(request: BatchRequest) -> Result<EffectiveSettings, String> {
    resolve_canonical(request).map(|config| config.effective_settings())
}

#[tauri::command]
pub fn run_job(
    state: State<'_, JobState>,
    core: State<'_, CoreHandle>,
    cache_state: State<'_, InspectCacheState>,
    request: BatchRequest,
) -> Result<u64, String> {
    launch(&state, core.job_sink(), &cache_state, request)
}

#[tauri::command]
pub async fn clear_cache(cache_state: State<'_, InspectCacheState>) -> Result<String, String> {
    // Clearing a separate instance would not stick: the shared one's next save would write
    // the cleared entries back.
    cache_state.clear().await?;
    Ok("Persistent inspection cache cleared.".to_string())
}

#[tauri::command]
pub fn pick_directory() -> Option<String> {
    rfd::FileDialog::new()
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn pick_subtitle_file() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("Subtitle Files", &["ass", "ssa", "srt", "vtt", "sub"])
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

/// The Desktop Core's last published View State.
#[tauri::command]
pub async fn get_state(core: State<'_, CoreHandle>) -> Result<ViewState, String> {
    core.state().await
}

/// Applies an Intent to the Desktop Core and answers once its effect is published.
#[tauri::command]
pub async fn dispatch(core: State<'_, CoreHandle>, intent: Intent) -> Result<bool, String> {
    core.dispatch(intent).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::WebviewPublisher;
    use mkv_desktop_core::actor::Core;
    use mkv_desktop_core::inspection::EngineInspector;
    use std::sync::Arc;
    use tauri::Manager;

    /// A dry-run extract request of `input_dir` that sets nothing else.
    fn dry_run_request(input_dir: &std::path::Path) -> BatchRequest {
        BatchRequest {
            mode: mkv_core::pipeline::PipelineMode::Extract,
            input_dir: input_dir.to_path_buf(),
            dry_run: Some(true),
            no_cache: Some(true),
            ..Default::default()
        }
    }

    /// The View State's `defaults` are what a run of an empty form resolves to.
    #[test]
    fn test_default_settings_match_a_resolved_empty_request() {
        let tmp = tempfile::tempdir().unwrap();
        let defaults = EffectiveSettings::defaults();
        assert_eq!(
            resolve_batch_request(dry_run_request(tmp.path())),
            Ok(defaults.clone())
        );
        // `BatchRequest::resolve` accepts 1-64; the default is clamped to 1-16.
        assert!(
            (1..=16).contains(&defaults.workers),
            "workers out of range: {}",
            defaults.workers
        );
    }

    #[test]
    fn test_resolve_batch_request_layers_explicit_values_over_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = resolve_batch_request(BatchRequest {
            workers: Some(3),
            io_jobs: Some(0),
            ffmpeg_threads: Some(2),
            bitrate: Some(" 256k ".into()),
            complexity: Some(8),
            ..dry_run_request(tmp.path())
        })
        .unwrap();
        // An explicit `io_jobs` of 0 is the worker count.
        assert_eq!(
            settings,
            EffectiveSettings {
                workers: 3,
                io_jobs: 3,
                ffmpeg_threads: 2,
                bitrate: "256k".into(),
                complexity: 8,
            }
        );
    }

    #[test]
    fn test_resolve_batch_request_reports_the_first_validation_error() {
        let tmp = tempfile::tempdir().unwrap();
        let error = resolve_batch_request(BatchRequest {
            workers: Some(0),
            io_jobs: Some(65),
            ..dry_run_request(tmp.path())
        })
        .unwrap_err();
        assert_eq!(error, "Worker thread count must be between 1 and 64");

        let missing = tmp.path().join("missing");
        let error = resolve_batch_request(dry_run_request(&missing)).unwrap_err();
        assert!(error.contains("Input directory does not exist"), "{error}");
    }

    #[test]
    fn test_dispatch_scans_a_directory_into_the_published_state() {
        let app = tauri::test::mock_app();
        app.manage(Core::spawn(
            tauri::async_runtime::handle().inner().clone(),
            WebviewPublisher(app.handle().clone()),
            EngineInspector {
                scan: Arc::default(),
                cache: InspectCacheState::default(),
            },
        ));
        let tmp = tempfile::tempdir().unwrap();
        let intent: Intent = serde_json::from_value(serde_json::json!({
            "type": "scan_directory",
            "mode": "extract",
            "path": tmp.path().join("."),
            "force": true,
            "no_cache": true,
        }))
        .unwrap();

        assert_eq!(
            tauri::async_runtime::block_on(dispatch(app.state(), intent)),
            Ok(true)
        );
        let state = tauri::async_runtime::block_on(get_state(app.state())).unwrap();
        let directory = state.extract.directory.unwrap();
        assert_eq!(
            std::fs::canonicalize(directory).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
        assert!(state.extract.rows.is_empty());
    }
}
