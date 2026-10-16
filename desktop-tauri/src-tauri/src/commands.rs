use mkv_core::manifest::InspectionManifest;
use mkv_core::request::{BatchRequest, EffectiveSettings};
use mkv_desktop_core::actor::{CoreHandle, ViewState};
use mkv_desktop_core::control::{launch, resolve_canonical, InspectCacheState, ScanState};
use mkv_desktop_core::job::{display_canonical_path, JobState};
use mkv_desktop_core::snapshot::JobSnapshot;
use serde::Deserialize;
use std::path::PathBuf;
use tauri::State;

pub(crate) const CLEANUP_ERROR: &str = "cleanup-error";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectOptions {
    no_cache: Option<bool>,
}

/// Canonicalizes `path` on the blocking pool and pairs the result with its Directory Identity:
/// the display form of the canonical path, which is the only spelling the UI compares
/// directories by.
async fn canonicalize_directory(path: String) -> Result<(PathBuf, String), String> {
    let dir = tauri::async_runtime::spawn_blocking(move || std::fs::canonicalize(path))
        .await
        .map_err(|e| format!("Async runtime error: {e}"))?
        .map_err(|e| format!("Invalid directory path: {e}"))?;
    let identity = display_canonical_path(&dir.to_string_lossy());
    Ok((dir, identity))
}

/// The Directory Identity of `path`: what `inspect_directory` reports as the manifest's
/// `canonical_dir`. The UI resolves a typed path through it before comparing directories.
#[tauri::command]
pub async fn canonical_directory(path: String) -> Result<String, String> {
    canonicalize_directory(path)
        .await
        .map(|(_, identity)| identity)
}

#[tauri::command]
pub async fn inspect_directory(
    core: State<'_, CoreHandle>,
    state: State<'_, ScanState>,
    cache_state: State<'_, InspectCacheState>,
    path: String,
    options: Option<InspectOptions>,
) -> Result<InspectionManifest, String> {
    // A closing app refuses before it touches the path. Canonicalizing runs before
    // `ScanState::begin` takes the start lock, whose guard cannot be held across this await.
    state.ensure_accepting_starts()?;
    let (dir, identity) = canonicalize_directory(path).await?;
    let (_registration, cancel_token) = state.begin(&dir)?;
    core.scan_began();

    let no_cache = options.and_then(|o| o.no_cache).unwrap_or(false);
    let cache = if no_cache {
        None
    } else {
        Some(cache_state.shared().await?)
    };
    let mut manifest =
        mkv_core::manifest::generate_inspection_manifest(&dir, cache, Some(&cancel_token)).await?;
    manifest
        .extra
        .insert("canonical_dir".to_string(), identity.into());
    Ok(manifest)
}

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

#[tauri::command]
pub fn get_preset_profiles() -> Vec<mkv_core::preset::PresetProfile> {
    mkv_core::preset::get_preset_profiles()
}

/// The Effective Settings of a `BatchRequest` that leaves every setting unset, for the UI's
/// "auto (N)" and idle displays.
#[tauri::command]
pub fn get_default_settings() -> EffectiveSettings {
    EffectiveSettings::defaults()
}

/// The Desktop Core's published Job Snapshot, which the webview's restore path reads.
#[tauri::command]
pub async fn get_job_snapshot(core: State<'_, CoreHandle>) -> Result<Option<JobSnapshot>, String> {
    core.job_snapshot().await
}

/// The Desktop Core's last published View State.
#[tauri::command]
pub async fn get_state(core: State<'_, CoreHandle>) -> Result<ViewState, String> {
    core.state().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::WebviewPublisher;
    use mkv_desktop_core::actor::Core;
    use tauri::Manager;

    #[test]
    fn test_inspect_options_deserialization() {
        let opts: InspectOptions = serde_json::from_str(r#"{"noCache": true}"#).unwrap();
        assert_eq!(opts.no_cache, Some(true));

        let opts_default: InspectOptions = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(opts_default.no_cache, None);
    }

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

    #[test]
    fn test_get_default_settings_match_a_resolved_empty_request() {
        let tmp = tempfile::tempdir().unwrap();
        let defaults = get_default_settings();
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
    fn test_canonical_directory_is_the_display_form_of_the_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        // A dotted spelling of the directory.
        let spelled = tmp.path().join(".");

        let identity = tauri::async_runtime::block_on(canonical_directory(
            spelled.to_string_lossy().into_owned(),
        ))
        .unwrap();

        // The identity names the same real directory, in the display form.
        assert_eq!(
            std::fs::canonicalize(&identity).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
        assert!(!identity.starts_with(r"\\?\"), "{identity}");
        // Only Windows has a verbatim prefix to strip, so elsewhere the canonical path is the identity.
        #[cfg(not(windows))]
        assert_eq!(
            identity,
            std::fs::canonicalize(tmp.path()).unwrap().to_string_lossy()
        );
    }

    #[test]
    fn test_inspect_directory_reports_the_identity_canonical_directory_returns() {
        let app = tauri::test::mock_app();
        app.manage(Core::spawn(
            tauri::async_runtime::handle().inner().clone(),
            WebviewPublisher(app.handle().clone()),
        ));
        app.manage(ScanState::default());
        app.manage(InspectCacheState::default());
        let tmp = tempfile::tempdir().unwrap();
        let spelled = tmp.path().join(".").to_string_lossy().into_owned();

        let manifest = tauri::async_runtime::block_on(inspect_directory(
            app.state(),
            app.state(),
            app.state(),
            spelled.clone(),
            Some(InspectOptions {
                no_cache: Some(true),
            }),
        ))
        .unwrap();
        let identity = tauri::async_runtime::block_on(canonical_directory(spelled)).unwrap();

        assert_eq!(
            manifest.extra["canonical_dir"].as_str(),
            Some(identity.as_str())
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_canonical_directory_resolves_a_symlink_to_its_target() {
        let target = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(target.path(), &link).unwrap();

        let identity = tauri::async_runtime::block_on(canonical_directory(
            link.to_string_lossy().into_owned(),
        ))
        .unwrap();

        let expected = std::fs::canonicalize(target.path()).unwrap();
        assert_eq!(identity, expected.to_string_lossy());
    }

    #[test]
    fn test_canonical_directory_rejects_a_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing");

        let error = tauri::async_runtime::block_on(canonical_directory(
            missing.to_string_lossy().into_owned(),
        ))
        .unwrap_err();

        assert!(error.starts_with("Invalid directory path: "), "{error}");
    }
}
