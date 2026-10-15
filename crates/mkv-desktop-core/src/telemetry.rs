//! The telemetry sampler: CPU, memory, and the write rate of the app and its Media Tools.

use std::sync::Mutex;

use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind,
    System,
};

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
                // CPU usage needs a baseline for the first poll to diff against; the process
                // map fills on the first poll, which runs off the main thread.
                sys: System::new_with_specifics(
                    RefreshKind::nothing()
                        .with_cpu(CpuRefreshKind::everything())
                        .with_memory(MemoryRefreshKind::everything()),
                ),
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

const KNOWN_ENGINE_PREFIXES: [&str; 3] = ["mkvmerge", "mkvextract", "ffmpeg"];

#[inline]
fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Whether `pid` is the desktop app itself (`self_pid`) or one of its direct child
/// Media Tools, matched by name prefix and parent.
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

impl TelemetryState {
    /// Polls this process and its Media Tools. It walks every process, so a caller runs it off
    /// any thread a UI or the engine's tasks need.
    pub fn sample(&self) -> Result<TelemetryData, String> {
        let mut guard = self.inner.lock().map_err(|e| e.to_string())?;
        Ok(poll(&mut guard, Pid::from_u32(std::process::id())))
    }
}

/// Samples CPU, memory, and the engine write rate since the previous poll.
fn poll(inner: &mut TelemetryInner, self_pid: Pid) -> TelemetryData {
    let TelemetryInner { sys, last_poll } = inner;

    sys.refresh_cpu_all();
    sys.refresh_memory();

    // Every poll refreshes all processes, but only their disk usage: name and parent are set
    // without asking. Dead processes are dropped, or an exited child would stay in the map and
    // its last write delta would be counted again on every poll.
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_disk_usage(),
    );

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

    TelemetryData {
        cpu_pct,
        ram_used_mb: ram_used,
        ram_total_mb: ram_total,
        ram_pct,
        io_mb_s,
    }
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

    /// An exited child must leave the process map once reaped, or its last write delta would be
    /// counted on every later poll and the map would grow with every media tool a batch runs.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_poll_drops_reaped_child_processes() {
        let mut inner = TelemetryInner {
            sys: System::new(),
            last_poll: None,
        };
        let self_pid = Pid::from_u32(std::process::id());
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let child_pid = Pid::from_u32(child.id());

        // Not reaped yet, so /proc still lists the child: running, or a zombie once it exits.
        poll(&mut inner, self_pid);
        assert!(inner.sys.process(child_pid).is_some());

        child.wait().unwrap();
        poll(&mut inner, self_pid);
        assert!(inner.sys.process(child_pid).is_none());
    }

    /// The poll asks sysinfo for disk usage only, but `is_engine_process` matches children by name
    /// and parent. Both must stay populated on every platform, or the write rate would quietly
    /// count only the app itself.
    #[test]
    fn test_poll_keeps_the_name_and_parent_of_a_live_child() {
        let mut inner = TelemetryInner {
            sys: System::new(),
            last_poll: None,
        };
        let self_pid = Pid::from_u32(std::process::id());
        let (program, args, expected_name): (&str, &[&str], &str) = if cfg!(windows) {
            ("ping", &["-n", "30", "127.0.0.1"], "ping")
        } else {
            ("sleep", &["30"], "sleep")
        };
        let mut child = std::process::Command::new(program)
            .args(args)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();

        poll(&mut inner, self_pid);
        let listed = inner.sys.process(Pid::from_u32(child.id())).map(|process| {
            (
                process.name().to_string_lossy().into_owned(),
                process.parent(),
            )
        });
        // Stop the child before asserting, so a failure does not leave it running.
        child.kill().unwrap();
        child.wait().unwrap();

        let (listed_name, parent) = listed.expect("the live child is missing from the map");
        assert!(
            starts_with_ignore_ascii_case(&listed_name, expected_name),
            "{listed_name}"
        );
        assert_eq!(parent, Some(self_pid));
    }
}
