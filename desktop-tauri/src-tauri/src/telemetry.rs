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
    tracked_pids: Vec<Pid>,
}

impl Default for TelemetryState {
    fn default() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        Self {
            inner: Mutex::new(TelemetryInner {
                sys,
                last_poll: None,
                tracked_pids: Vec::new(),
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

/// Maximum depth for traversing the process tree (e.g. sidecar -> media tool).
const MAX_DESCENDANT_SEARCH_DEPTH: usize = 4;

#[cfg(target_os = "linux")]
fn collect_linux_descendant_pids(roots: &[Pid]) -> Option<Vec<Pid>> {
    let mut collected = Vec::new();
    let mut queue = Vec::new();
    for &root in roots {
        if !collected.contains(&root) {
            collected.push(root);
            queue.push(root);
        }
    }

    let mut depth = 0;
    while !queue.is_empty() && depth < MAX_DESCENDANT_SEARCH_DEPTH {
        let mut next_queue = Vec::new();
        for pid in queue {
            let path = format!("/proc/{}/task/{}/children", pid.as_u32(), pid.as_u32());
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    for token in content.split_whitespace() {
                        if let Ok(child_u32) = token.parse::<u32>() {
                            let child_pid = Pid::from_u32(child_u32);
                            if !collected.contains(&child_pid) {
                                collected.push(child_pid);
                                next_queue.push(child_pid);
                            }
                        }
                    }
                }
                Err(err) => {
                    // PermissionDenied indicates the /proc children interface is restricted or
                    // unsupported on this kernel, so signal a fallback to full process scan.
                    if err.kind() == std::io::ErrorKind::PermissionDenied {
                        return None;
                    }
                }
            }
        }
        queue = next_queue;
        depth += 1;
    }
    Some(collected)
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
pub fn get_system_telemetry(
    state: State<'_, TelemetryState>,
    job_state: State<'_, JobState>,
    scan_state: State<'_, ScanState>,
) -> Result<TelemetryData, String> {
    let mut guard = state.inner.lock().map_err(|e| e.to_string())?;
    let TelemetryInner {
        ref mut sys,
        ref mut last_poll,
        ref mut tracked_pids,
    } = *guard;
    let self_pid = Pid::from_u32(std::process::id());

    let job_pid = job_state.active_pid();
    let scan_pid = scan_state.active_pid();
    let has_active_job = job_pid.is_some() || scan_pid.is_some();

    sys.refresh_cpu_all();
    sys.refresh_memory();

    #[cfg(target_os = "linux")]
    {
        if !has_active_job && tracked_pids.len() <= 1 {
            sys.refresh_processes(ProcessesToUpdate::Some(&[self_pid]), true);
            tracked_pids.clear();
        } else {
            let mut sidecar_roots = Vec::new();
            if let Some(p) = job_pid {
                sidecar_roots.push(p);
            }
            if let Some(p) = scan_pid {
                sidecar_roots.push(p);
            }
            match collect_linux_descendant_pids(&sidecar_roots) {
                Some(mut descendants) => {
                    if !descendants.contains(&self_pid) {
                        descendants.push(self_pid);
                    }
                    for prev_pid in tracked_pids.iter() {
                        if !descendants.contains(prev_pid) {
                            descendants.push(*prev_pid);
                        }
                    }
                    sys.refresh_processes(ProcessesToUpdate::Some(&descendants), true);
                }
                None => {
                    sys.refresh_processes(ProcessesToUpdate::All, false);
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        if !has_active_job {
            sys.refresh_processes(ProcessesToUpdate::Some(&[self_pid]), true);
            tracked_pids.clear();
        } else {
            sys.refresh_processes(ProcessesToUpdate::All, false);
        }
    }

    let mut written_window = 0u64;
    let mut active_engine_pids = Vec::new();
    for (pid, process) in sys.processes() {
        if is_engine_process(
            *pid,
            &process.name().to_string_lossy(),
            |p| sys.process(p).and_then(|proc| proc.parent()),
            self_pid,
        ) {
            written_window += process.disk_usage().written_bytes;
            active_engine_pids.push(*pid);
        }
    }
    *tracked_pids = active_engine_pids;
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
        let sidecar_pid = Pid::from_u32(101);
        let grandchild_pid = Pid::from_u32(102);
        let unrelated_sidecar = Pid::from_u32(201);
        let unrelated_grandchild = Pid::from_u32(202);
        let unrelated_app = Pid::from_u32(300);

        let mut parents = HashMap::new();
        parents.insert(sidecar_pid, self_pid);
        parents.insert(grandchild_pid, sidecar_pid);
        parents.insert(unrelated_sidecar, Pid::from_u32(999));
        parents.insert(unrelated_grandchild, unrelated_sidecar);
        parents.insert(unrelated_app, self_pid);

        let parent_fn = |p| parents.get(&p).copied();

        // 1. self_pid is always engine
        assert!(is_engine_process(
            self_pid,
            "mkvanimebatch",
            parent_fn,
            self_pid
        ));

        // 2. sidecar descendant of self_pid is engine
        assert!(is_engine_process(
            sidecar_pid,
            "mkv-remux-x86_64",
            parent_fn,
            self_pid
        ));

        // 3. grandchild (mkvmerge, mkvextract, ffmpeg) descending from self_pid is engine
        assert!(is_engine_process(
            grandchild_pid,
            "mkvmerge",
            parent_fn,
            self_pid
        ));

        // 4. unrelated sidecar or grandchild with different ancestor is NOT engine
        assert!(!is_engine_process(
            unrelated_sidecar,
            "mkv-remux-x86_64",
            parent_fn,
            self_pid
        ));
        assert!(!is_engine_process(
            unrelated_grandchild,
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
        assert!(!is_engine_process(
            sidecar_pid,
            "ééééé",
            parent_fn,
            self_pid
        ));
        assert!(!is_engine_process(
            sidecar_pid,
            "アニメプロセス",
            parent_fn,
            self_pid
        ));
    }

    #[test]
    fn test_active_pid_defaults_to_none() {
        let job_state = JobState::default();
        let scan_state = ScanState::default();
        assert_eq!(job_state.active_pid(), None);
        assert_eq!(scan_state.active_pid(), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_collect_linux_descendant_pids_resilience() {
        let self_pid = Pid::from_u32(std::process::id());
        let res = collect_linux_descendant_pids(&[self_pid]);
        assert!(res.is_some_and(|pids| pids.contains(&self_pid)));

        let non_existent = Pid::from_u32(999_999);
        let non_res = collect_linux_descendant_pids(&[non_existent]);
        assert_eq!(non_res, Some(vec![non_existent]));
    }
}
