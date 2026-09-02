use std::sync::Mutex;

use sysinfo::{Pid, ProcessesToUpdate, System};
use tauri::State;

pub struct TelemetryState {
    inner: Mutex<TelemetryInner>,
}

struct TelemetryInner {
    sys: System,
    /// Timestamp of the previous telemetry poll; the window between polls
    /// converts the per-refresh written-bytes delta into a rate.
    last_poll: Option<std::time::Instant>,
}

impl Default for TelemetryState {
    fn default() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        Self {
            inner: Mutex::new(TelemetryInner {
                sys,
                last_poll: None,
            }),
        }
    }
}

#[derive(serde::Serialize)]
pub struct TelemetryData {
    pub cpu_pct: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub ram_pct: f32,
    pub io_mb_s: f32,
}

// ─── Tauri Commands ──────────────────────────────────────────────────────────

const KNOWN_ENGINE_PREFIXES: [&str; 5] = [
    "mkv-remux",
    "mkv-extract",
    "mkvmerge",
    "mkvextract",
    "ffmpeg",
];

#[inline]
fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Determines if a process is an engine process (the desktop app itself, or its
/// descendant sidecars and media tools: mkv-remux, mkv-extract, mkvmerge, mkvextract, ffmpeg)
/// by checking process name and tracing its parent chain back to `self_pid`.
pub fn is_engine_process(
    pid: Pid,
    process_name: &str,
    parent_fn: impl Fn(Pid) -> Option<Pid>,
    self_pid: Pid,
) -> bool {
    if pid == self_pid {
        return true;
    }

    let is_known_name = KNOWN_ENGINE_PREFIXES
        .iter()
        .any(|prefix| starts_with_ignore_ascii_case(process_name, prefix));

    if !is_known_name {
        return false;
    }

    let mut current = parent_fn(pid);
    for _ in 0..5 {
        if let Some(parent) = current {
            if parent == self_pid {
                return true;
            }
            current = parent_fn(parent);
        } else {
            break;
        }
    }
    false
}

#[tauri::command]
pub fn get_system_telemetry(state: State<'_, TelemetryState>) -> Result<TelemetryData, String> {
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
    let sys = &mut inner.sys;
    sys.refresh_cpu_all();
    sys.refresh_memory();
    // `written_bytes` is a per-refresh delta, so the poll interval doubles as
    // the measurement window. Scoped to this app and its descendant sidecars
    // and media tools (mkv-remux, mkv-extract, mkvmerge, mkvextract, ffmpeg)
    // by tracing process ancestry back to self_pid.
    sys.refresh_processes(ProcessesToUpdate::All, false);

    let self_pid = Pid::from_u32(std::process::id());
    let mut written_window = 0u64;
    for (pid, process) in sys.processes() {
        if is_engine_process(
            *pid,
            &process.name().to_string_lossy(),
            |p| sys.process(p).and_then(|proc| proc.parent()),
            self_pid,
        ) {
            written_window += process.disk_usage().written_bytes;
        }
    }
    let cpus = sys.cpus();
    let cpu_pct = if !cpus.is_empty() {
        let total: f32 = cpus.iter().map(|c| c.cpu_usage()).sum();
        (total / cpus.len() as f32).round()
    } else {
        sys.global_cpu_usage().round()
    };

    let ram_used = sys.used_memory() / 1024 / 1024;
    let ram_total = sys.total_memory() / 1024 / 1024;
    let ram_pct = if ram_total > 0 {
        ((ram_used as f32 / ram_total as f32) * 100.0).round()
    } else {
        0.0
    };
    // `sys` is no longer borrowed past this point, so the poll bookkeeping
    // can read `last_poll` freely.
    let io_mb_s = match inner.last_poll {
        Some(prev) => {
            let elapsed = prev.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                (written_window as f64 / elapsed / (1024.0 * 1024.0)) as f32
            } else {
                0.0
            }
        }
        None => 0.0,
    };
    inner.last_poll = Some(std::time::Instant::now());

    Ok(TelemetryData {
        cpu_pct,
        ram_used_mb: ram_used,
        ram_total_mb: ram_total,
        ram_pct,
        io_mb_s,
    })
}
