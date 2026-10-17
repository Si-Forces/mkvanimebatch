use mkv_core::pipeline::PipelineMode;
use mkv_desktop_core::actor::{CoreHandle, Intent, ViewState};
use mkv_desktop_core::log::LogView;
use tauri::State;

pub(crate) const CLEANUP_ERROR: &str = "cleanup-error";

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

/// The whole of a mode's Run Log, for a view that boots or reloads.
#[tauri::command]
pub async fn get_log(core: State<'_, CoreHandle>, mode: PipelineMode) -> Result<LogView, String> {
    core.log(mode).await
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
    use mkv_desktop_core::actor::{Core, Ports};
    use mkv_desktop_core::control::{EngineLauncher, InspectCacheState, ScanState};
    use mkv_desktop_core::inspection::EngineInspector;
    use mkv_desktop_core::telemetry::EngineSampler;
    use std::sync::Arc;
    use tauri::Manager;

    #[test]
    fn test_dispatch_scans_a_directory_into_the_published_state() {
        let app = tauri::test::mock_app();
        let (scan, cache) = (Arc::new(ScanState::default()), InspectCacheState::default());
        let ports = Ports {
            inspector: EngineInspector {
                scan: Arc::clone(&scan),
                cache: cache.clone(),
            },
            launcher: EngineLauncher {
                job: mkv_desktop_core::job::test_support::job_state(),
                scan,
                cache,
            },
            sampler: EngineSampler::default(),
        };
        app.manage(Core::spawn(
            tauri::async_runtime::handle().inner().clone(),
            WebviewPublisher::new(app.handle().clone()),
            ports,
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
        let log = tauri::async_runtime::block_on(get_log(app.state(), PipelineMode::Extract));
        assert!(log.unwrap().lines.is_empty());
    }
}
