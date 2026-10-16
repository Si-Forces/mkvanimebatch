//! The System Telemetry Provider: CPU, memory, and the write rate of the app and its Media Tools.
//! The `Sampler` port takes one sample off the actor, `EngineSampler` takes it with `sysinfo`,
//! and `TelemetryView` is what the View State shows of the samples so far.

use serde::Serialize;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

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

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
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

/// What the actor needs to sample the system. It runs off the actor.
pub trait Sampler: Send + Sync + 'static {
    fn sample(&self) -> impl Future<Output = Result<TelemetryData, String>> + Send;
}

/// The `Sampler` of the app: `TelemetryState::sample` on the blocking pool, since it walks
/// every process.
#[derive(Clone, Default)]
pub struct EngineSampler(pub Arc<TelemetryState>);

impl Sampler for EngineSampler {
    async fn sample(&self) -> Result<TelemetryData, String> {
        let state = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || state.sample())
            .await
            .map_err(|e| format!("Async runtime error: {e}"))?
    }
}

/// How many samples of each series a sparkline shows.
pub const SPARK_SAMPLES: usize = 40;

/// The telemetry a view shows: the texts of the last sample and the newest `SPARK_SAMPLES` of
/// each series, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TelemetryView {
    /// `37%`.
    pub cpu: String,
    /// The memory in use: `5.12 GB`.
    pub ram: String,
    /// The engine's write rate, `3.4 MB/s`, or `Idle` when it writes nothing.
    pub disk: String,
    pub samples: TelemetrySamples,
}

/// The sparkline series: CPU and RAM in percent, the disk in MB/s.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TelemetrySamples {
    pub cpu: VecDeque<f32>,
    pub ram: VecDeque<f32>,
    pub disk: VecDeque<f32>,
}

impl TelemetryView {
    /// Takes in one sample.
    pub fn push(&mut self, data: &TelemetryData) {
        // A NaN or an infinity reads 0, so the text never reads "inf%".
        let cpu = if data.cpu_pct.is_finite() {
            data.cpu_pct.max(0.0)
        } else {
            0.0
        };
        // The write rate of the engine's own processes is zero whenever no batch writes: that
        // shows as idle rather than "0 MB/s".
        let io = if data.io_mb_s > 0.0 {
            data.io_mb_s
        } else {
            0.0
        };
        let ram = if data.ram_pct.is_nan() {
            0.0
        } else {
            data.ram_pct
        };
        for (series, value) in [
            (&mut self.samples.cpu, cpu),
            (&mut self.samples.ram, ram),
            (&mut self.samples.disk, io),
        ] {
            series.push_back(value);
            if series.len() > SPARK_SAMPLES {
                series.pop_front();
            }
        }
        // `Math.round`: half up.
        self.cpu = format!("{}%", (f64::from(cpu) + 0.5).floor());
        self.ram = format!("{:.2} GB", data.ram_used_mb as f64 / 1024.0);
        self.disk = if io > 0.0 {
            // Through the shortest decimal, as the f32 reached JS over JSON before `toFixed(1)`.
            let io: f64 = io.to_string().parse().unwrap_or(f64::from(io));
            format!("{io:.1} MB/s")
        } else {
            "Idle".to_string()
        };
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

    fn data(cpu_pct: f32, ram_used_mb: u64, ram_pct: f32, io_mb_s: f32) -> TelemetryData {
        TelemetryData {
            cpu_pct,
            ram_used_mb,
            ram_total_mb: 16_384,
            ram_pct,
            io_mb_s,
        }
    }

    #[test]
    fn test_the_view_shows_the_texts_of_the_last_sample_and_the_newest_samples() {
        let mut view = TelemetryView::default();
        view.push(&data(37.0, 5243, 32.0, 3.44));
        assert_eq!(
            (view.cpu.as_str(), view.ram.as_str(), view.disk.as_str()),
            ("37%", "5.12 GB", "3.4 MB/s")
        );
        // Below zero or NaN reads 0; no write is idle.
        view.push(&data(-3.0, 0, f32::NAN, 0.0));
        assert_eq!(
            (view.cpu.as_str(), view.ram.as_str(), view.disk.as_str()),
            ("0%", "0.00 GB", "Idle")
        );
        view.push(&data(f32::NAN, 1024, 6.0, -1.0));
        assert_eq!((view.cpu.as_str(), view.disk.as_str()), ("0%", "Idle"));
        assert_eq!(Vec::from(view.samples.cpu.clone()), [37.0, 0.0, 0.0]);
        assert_eq!(Vec::from(view.samples.ram.clone()), [32.0, 0.0, 6.0]);
        assert_eq!(Vec::from(view.samples.disk.clone()), [3.44, 0.0, 0.0]);
        // Half up, as `Math.round` rounds.
        view.push(&data(12.5, 0, 0.0, 0.0));
        assert_eq!(view.cpu, "13%");

        for step in 0..SPARK_SAMPLES {
            view.push(&data(step as f32, 0, 0.0, 0.0));
        }
        assert_eq!(view.samples.cpu.len(), SPARK_SAMPLES);
        assert_eq!(view.samples.cpu.front(), Some(&0.0));
        assert_eq!(view.samples.cpu.back(), Some(&((SPARK_SAMPLES - 1) as f32)));
        assert_eq!(
            serde_json::to_value(&view).unwrap()["samples"]["ram"]
                .as_array()
                .unwrap()
                .len(),
            SPARK_SAMPLES
        );
    }

    #[test]
    fn test_an_infinite_cpu_reads_0() {
        let mut view = TelemetryView::default();
        view.push(&data(f32::INFINITY, 0, 0.0, 0.0));
        assert_eq!(view.cpu, "0%");
        assert_eq!(Vec::from(view.samples.cpu.clone()), [0.0]);
    }

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
