use mkv_desktop_core::telemetry::{TelemetryData, TelemetryState};
use tauri::{AppHandle, Manager};

/// Runs the poll on the blocking pool: a sync command would walk every process on the main
/// thread, and a plain async one would block a Tokio worker the engine's tasks run on. The state
/// is read through `app` because `State<'_>` is not `'static`.
#[tauri::command]
pub async fn get_system_telemetry(app: AppHandle) -> Result<TelemetryData, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<TelemetryState>().sample())
        .await
        .map_err(|error| format!("Async runtime error: {error}"))?
}
