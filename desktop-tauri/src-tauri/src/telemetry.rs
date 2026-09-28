use std::sync::Mutex;

use sysinfo::{Pid, ProcessesToUpdate, System};
use tauri::State;

use crate::process::{JobState, ScanState};

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
        Self {
            inner: Mutex::new(TelemetryInner {
                sys: System::new_all(),
                last_poll: None,
            }),
        }
    }
}

#[derive(serde::Serialize)]
pub struct TelemetryData {
    cpu_pct: f32,
    ram_used_mb: u64,
    ram_total_mb: u64,
    ram_pct: f32,
    io_mb_s: f32,
}

const KNOWN_ENGINE_PREFIXES: [&str; 3] = ["mkvmerge", "mkvextract", "ffmpeg"];

#[inline]
fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Determines if a process is an engine process (the desktop app itself, or its
/// direct child media tools: mkvmerge, mkvextract, ffmpeg)
/// by checking process name and whether its parent is `self_pid`.
fn is_engine_process(
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

    parent_fn(pid) == Some(self_pid)
}

#[tauri::command]
pub fn get_system_telemetry(
    state: State<'_, TelemetryState>,
    job_state: State<'_, JobState>,
    scan_state: State<'_, ScanState>,
) -> Result<TelemetryData, String> {
    let mut guard = state.inner.lock().map_err(|e| e.to_string())?;
    let TelemetryInner { sys, last_poll } = &mut *guard;
    let self_pid = Pid::from_u32(std::process::id());

    let has_active_job = job_state.is_active() || scan_state.is_active();

    sys.refresh_cpu_all();
    sys.refresh_memory();

    if has_active_job {
        sys.refresh_processes(ProcessesToUpdate::All, false);
    } else {
        sys.refresh_processes(ProcessesToUpdate::Some(&[self_pid]), true);
    }

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
    let io_mb_s = match last_poll {
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
    *last_poll = Some(std::time::Instant::now());

    Ok(TelemetryData {
        cpu_pct,
        ram_used_mb: ram_used,
        ram_total_mb: ram_total,
        ram_pct,
        io_mb_s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_is_engine_process_ancestry_and_naming() {
        let self_pid = Pid::from_u32(100);
        let child_pid = Pid::from_u32(101);
        let grandchild_pid = Pid::from_u32(102);
        let unrelated_process = Pid::from_u32(201);
        let unrelated_app = Pid::from_u32(300);

        let mut parents = HashMap::new();
        parents.insert(child_pid, self_pid);
        parents.insert(grandchild_pid, child_pid);
        parents.insert(unrelated_process, Pid::from_u32(999));
        parents.insert(unrelated_app, self_pid);

        let parent_fn = |p| parents.get(&p).copied();

        // 1. self_pid is always engine
        assert!(is_engine_process(
            self_pid,
            "mkvanimebatch",
            parent_fn,
            self_pid
        ));

        // 2. direct child tool (mkvmerge, mkvextract, ffmpeg) of self_pid is engine
        assert!(is_engine_process(
            child_pid, "mkvmerge", parent_fn, self_pid
        ));
        assert!(is_engine_process(
            child_pid,
            "mkvextract",
            parent_fn,
            self_pid
        ));
        assert!(is_engine_process(child_pid, "ffmpeg", parent_fn, self_pid));

        // 3. grandchild is NOT engine (only direct children matched)
        assert!(!is_engine_process(
            grandchild_pid,
            "mkvmerge",
            parent_fn,
            self_pid
        ));

        // 4. unrelated tool with different parent is NOT engine
        assert!(!is_engine_process(
            unrelated_process,
            "mkvmerge",
            parent_fn,
            self_pid
        ));

        // 5. unrelated binary even if child of self_pid is NOT engine
        assert!(!is_engine_process(
            unrelated_app,
            "chrome",
            parent_fn,
            self_pid
        ));

        // 6. Unicode process names must not panic on UTF-8 character boundaries
        assert!(!is_engine_process(child_pid, "ééééé", parent_fn, self_pid));
        assert!(!is_engine_process(
            child_pid,
            "アニメプロセス",
            parent_fn,
            self_pid
        ));
    }
}
