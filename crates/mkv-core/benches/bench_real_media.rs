//! Real-media remux bench for the execution phase (issue #31). It measures two audit
//! observations: ffmpeg reads the source outside the io governor, so up to `workers` readers hit
//! the disk at once; and no-transcode items hold a worker slot while they wait on `io_jobs`.
//! CI never runs a cell: it needs a real corpus and takes minutes per cell. CI only runs the bench
//! with no corpus (`cargo test -p mkv-core --bench bench_real_media`), so the self-check asserts
//! run, then it skips.
//!
//! # How to run
//!
//! Linux only, with `ffmpeg`, `mkvmerge` and `mkvextract` on `PATH`:
//!
//! ```text
//! MKV_BENCH_CORPUS=/mnt/hdd/corpus MKV_BENCH_DISK=hdd cargo bench -p mkv-core --bench bench_real_media
//! MKV_BENCH_CORPUS=/mnt/ssd/corpus MKV_BENCH_DISK=ssd cargo bench -p mkv-core --bench bench_real_media
//! ```
//!
//! - `MKV_BENCH_CORPUS`: the corpus directory. When it is unset the bench skips.
//! - `MKV_BENCH_DISK`: `hdd` or `ssd`, the disk the corpus sits on. It labels each row and picks
//!   the grid. `hdd` runs all-FLAC with `workers` in {2, 4, 8}. `ssd` runs the same all-FLAC cells
//!   as a control, plus a mixed batch with `workers` in {d/2, d, 2 x d}, where d is the engine's
//!   default worker count (physical cores capped at the available parallelism, clamped to 1-16).
//!   Every cell uses `io_jobs = 2`, and each batch kind ends with a default cell (`workers` unset,
//!   so the engine's own default applies), which replaces any explicit cell, all-FLAC or mixed,
//!   equal to d. The HDD cells only mean something when the corpus is on the HDD, so copy it there
//!   and run again with `hdd`.
//! - `MKV_BENCH_BATCH` (optional): `flac` or `mixed`, to run only that batch kind. `mixed` with
//!   `hdd` selects no cells, so the bench exits with an error.
//!
//! Corpus layout: 6 real releases, one subdirectory each (`corpus/<release>/<episode>.mkv`), some
//! with FLAC audio and some with AAC. Every episode needs a subtitle, embedded or beside it, or
//! remux skips it. All-FLAC runs every episode whose selected audio track is FLAC. Mixed alternates
//! FLAC and passthrough (AAC or another non-FLAC codec) episodes, with as many of each as the
//! smaller group has.
//!
//! Each cell evicts every corpus file from the page cache (`dd iflag=nocache count=0`, no root
//! needed; it does not clear the drive's own cache, and on a network filesystem it may not clear
//! the client's cache either, without failing, so check that the first cell reads cold). It then
//! runs the remux into the default `<release>/Output/`, on the same disk as the sources, and
//! deletes everything the cell created. Remux writes only `Output/` (the episodes and their
//! `.tmp_remux_*` staging); the cleanup still removes any other new entry, so it does not depend
//! on that. The bench refuses to start if any release already has an `Output/`
//! directory, because remux would overwrite files there; if the corpus contains a symlink, because
//! sources behind one would not be evicted and outputs written behind one would not be deleted; or
//! if anything under the corpus cannot be read (such as a root-owned `lost+found` at the top of a
//! mount), because the bench could not tell its outputs from the sources. A run killed mid-cell
//! leaves `Output/` behind; remove it before you run again.
//!
//! `ffmpeg_threads` follows `workers` through the ADR-0013 governor (the `ffmpeg_t` column), so
//! the all-FLAC cells change both at once.

fn main() {
    #[cfg(target_os = "linux")]
    real_media::main();
    #[cfg(not(target_os = "linux"))]
    println!("Skipping real-media bench: it needs Linux (/proc/stat, dd iflag=nocache)");
}

#[cfg(target_os = "linux")]
mod real_media {
    use std::collections::{BTreeMap, HashSet};
    use std::ffi::OsString;
    use std::fs::{self, FileType};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Instant;

    use mkv_core::manifest::get_item_id;
    use mkv_core::pipeline::{BatchPipeline, CancellationToken, PipelineMode, PipelineSummary};
    use mkv_core::remux::select_tracks;
    use mkv_core::request::{BatchRequest, WORKERS_RANGE};
    use mkv_core::scan::{probe_video, scan_mkv_files, InspectedVideo};
    use tokio::runtime::Runtime;

    const IO_JOBS: usize = 2;
    const FLAC_WORKERS: [usize; 3] = [2, 4, 8];
    const MEDIA_TOOLS: [&str; 3] = ["ffmpeg", "mkvmerge", "mkvextract"];

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BatchKind {
        AllFlac,
        Mixed,
    }

    impl BatchKind {
        fn label(self) -> &'static str {
            match self {
                Self::AllFlac => "all-flac",
                Self::Mixed => "mixed",
            }
        }
    }

    /// One grid cell. `workers: None` leaves the engine's default worker count in place.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Cell {
        kind: BatchKind,
        workers: Option<usize>,
    }

    /// The cells for `disk` (`hdd` or `ssd`), restricted to `only` when set. `default_workers` is
    /// the engine's default; the default cell stands in for an explicit cell of that size.
    fn grid(
        disk: &str,
        only: Option<BatchKind>,
        default_workers: usize,
    ) -> Result<Vec<Cell>, String> {
        let kinds: &[BatchKind] = match disk {
            "hdd" => &[BatchKind::AllFlac],
            "ssd" => &[BatchKind::AllFlac, BatchKind::Mixed],
            other => {
                return Err(format!(
                    "MKV_BENCH_DISK must be 'hdd' or 'ssd', got '{other}'"
                ))
            }
        };
        let mut cells = Vec::new();
        for &kind in kinds.iter().filter(|&&k| only.is_none_or(|o| o == k)) {
            let mut workers = match kind {
                BatchKind::AllFlac => FLAC_WORKERS.to_vec(),
                BatchKind::Mixed => vec![default_workers / 2, default_workers, default_workers * 2],
            };
            workers
                .iter_mut()
                .for_each(|w| *w = (*w).clamp(*WORKERS_RANGE.start(), *WORKERS_RANGE.end()));
            workers.dedup();
            workers.retain(|&w| w != default_workers);
            cells.extend(workers.into_iter().map(|w| Cell {
                kind,
                workers: Some(w),
            }));
            cells.push(Cell {
                kind,
                workers: None,
            });
        }
        if cells.is_empty() {
            return Err(format!(
                "MKV_BENCH_DISK={disk} has no {} cells",
                only.map_or("", BatchKind::label)
            ));
        }
        Ok(cells)
    }

    /// Alternates FLAC and passthrough items, as many of each as the smaller list holds.
    fn interleave(flac: &[String], passthrough: &[String]) -> Vec<String> {
        flac.iter()
            .zip(passthrough)
            .flat_map(|(f, p)| [f.clone(), p.clone()])
            .collect()
    }

    /// Cumulative jiffies of the aggregate `cpu` line of `/proc/stat`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct CpuTimes {
        total: u64,
        idle: u64,
        iowait: u64,
    }

    fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
        let line = text.lines().find(|l| l.starts_with("cpu "))?;
        let fields: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .map(|v| v.parse().ok())
            .collect::<Option<_>>()?;
        if fields.len() < 5 {
            return None;
        }
        // user nice system idle iowait irq softirq steal. `guest` and `guest_nice` (fields 9 and 10)
        // are already counted in user and nice, so summing them would count that time twice.
        Some(CpuTimes {
            total: fields.iter().take(8).sum(),
            idle: fields[3],
            iowait: fields[4],
        })
    }

    fn read_cpu_times() -> Option<CpuTimes> {
        parse_proc_stat(&fs::read_to_string("/proc/stat").ok()?)
    }

    /// Mean busy and iowait percentages over all logical CPUs between two samples.
    fn cpu_use(before: CpuTimes, after: CpuTimes) -> Option<(f64, f64)> {
        let total = after.total.checked_sub(before.total).filter(|&t| t > 0)?;
        let idle = (after.idle + after.iowait).saturating_sub(before.idle + before.iowait);
        let iowait = after.iowait.saturating_sub(before.iowait);
        let pct = |v: u64| v as f64 * 100.0 / total as f64;
        Some((pct(total.saturating_sub(idle)), pct(iowait)))
    }

    /// The pure helpers above, checked on every run: a `harness = false` bench has no test runner.
    fn self_check() {
        let before = parse_proc_stat("cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4 5\n").unwrap();
        assert_eq!(
            before,
            CpuTimes {
                total: 1000,
                idle: 800,
                iowait: 50
            }
        );
        let after = parse_proc_stat("cpu  400 0 150 1100 150 0 0 0 99 99\n").unwrap();
        assert_eq!(cpu_use(before, after), Some((50.0, 12.5)));
        assert_eq!(cpu_use(before, before), None);
        assert_eq!(parse_proc_stat("intr 1 2 3\n"), None);

        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            interleave(&ids(&["f1", "f2", "f3"]), &ids(&["a1", "a2"])),
            ids(&["f1", "a1", "f2", "a2"])
        );

        let workers = |cells: Vec<Cell>, kind| -> Vec<Option<usize>> {
            cells
                .into_iter()
                .filter(|c| c.kind == kind)
                .map(|c| c.workers)
                .collect()
        };
        let ssd = grid("ssd", None, 12).unwrap();
        assert_eq!(
            workers(ssd.clone(), BatchKind::AllFlac),
            [Some(2), Some(4), Some(8), None]
        );
        assert_eq!(workers(ssd, BatchKind::Mixed), [Some(6), Some(24), None]);
        let ssd = grid("ssd", None, 4).unwrap();
        assert_eq!(
            workers(ssd.clone(), BatchKind::AllFlac),
            [Some(2), Some(8), None]
        );
        assert_eq!(workers(ssd, BatchKind::Mixed), [Some(2), Some(8), None]);
        assert_eq!(
            workers(grid("ssd", None, 1).unwrap(), BatchKind::Mixed),
            [Some(2), None]
        );
        assert!(grid("hdd", None, 12)
            .unwrap()
            .iter()
            .all(|c| c.kind == BatchKind::AllFlac));
        assert!(grid("hdd", Some(BatchKind::Mixed), 12).is_err());
        assert!(grid("nas", None, 12).is_err());
    }

    /// The entries under a directory, symlinks not followed.
    struct Walk {
        entries: Vec<(PathBuf, FileType)>,
        /// Directories or entries that could not be read, and so are missing from `entries`.
        unreadable: usize,
    }

    /// Walks `root`, counting what it cannot read instead of failing.
    fn walk(root: &Path) -> Walk {
        let mut entries = Vec::new();
        let mut unreadable = 0;
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(read) = fs::read_dir(&dir) else {
                unreadable += 1;
                continue;
            };
            for entry in read {
                let Some((path, file_type)) = entry
                    .ok()
                    .and_then(|e| e.file_type().ok().map(|t| (e.path(), t)))
                else {
                    unreadable += 1;
                    continue;
                };
                if file_type.is_dir() {
                    stack.push(path.clone());
                }
                entries.push((path, file_type));
            }
        }
        Walk {
            entries,
            unreadable,
        }
    }

    /// Records the corpus before a cell; on drop, deletes every entry that appeared since.
    /// Entries present before the cell are never touched, so the sources are safe, and the
    /// drop runs whether the cell succeeds, fails or panics.
    struct CellOutputs {
        root: PathBuf,
        before: HashSet<PathBuf>,
    }

    impl CellOutputs {
        /// Snapshots `root` and returns its regular files for eviction. The snapshot must be
        /// complete: an entry it missed would look created at drop time and be deleted, so any
        /// unreadable directory or entry refuses the cell instead.
        fn snapshot(root: &Path) -> Result<(Self, Vec<PathBuf>), String> {
            let Walk {
                entries,
                unreadable,
            } = walk(root);
            if unreadable > 0 {
                return Err(format!(
                    "{unreadable} unreadable directories or entries under '{}'",
                    root.display()
                ));
            }
            let files = entries
                .iter()
                .filter(|(_, t)| t.is_file())
                .map(|(p, _)| p.clone())
                .collect();
            let before = entries.into_iter().map(|(p, _)| p).collect();
            let guard = Self {
                root: root.to_path_buf(),
                before,
            };
            Ok((guard, files))
        }
    }

    impl Drop for CellOutputs {
        fn drop(&mut self) {
            // The unreadable count is ignored: an entry this walk cannot read is only left in
            // place, never wrongly deleted.
            let created: Vec<(PathBuf, FileType)> = walk(&self.root)
                .entries
                .into_iter()
                .filter(|(p, _)| !self.before.contains(p))
                .collect();
            let created_paths: HashSet<&Path> = created.iter().map(|(p, _)| p.as_path()).collect();
            for (path, file_type) in &created {
                // A created directory goes in one `remove_dir_all`, which also takes its contents,
                // all of them created too; it does not follow symlinks.
                if path.parent().is_some_and(|p| created_paths.contains(p)) {
                    continue;
                }
                let removed = if file_type.is_dir() {
                    fs::remove_dir_all(path)
                } else {
                    fs::remove_file(path)
                };
                if let Err(e) = removed {
                    eprintln!("  warning: could not delete '{}': {e}", path.display());
                }
            }
        }
    }

    /// Drops each file from the page cache; the drive's own cache is untouched.
    fn evict_page_cache(files: &[PathBuf]) {
        let failed = files
            .iter()
            .filter(|file| {
                let mut input = OsString::from("if=");
                input.push(file.as_os_str());
                !Command::new("dd")
                    .arg(input)
                    .args(["iflag=nocache", "count=0", "status=none"])
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            })
            .count();
        if failed > 0 {
            eprintln!("  warning: page-cache eviction failed for {failed} file(s)");
        }
    }

    struct Corpus {
        root: PathBuf,
        /// Item IDs whose selected audio track is FLAC, so remux transcodes them.
        flac: Vec<String>,
        /// Item IDs with a non-FLAC selected audio track, which remux copies.
        passthrough: Vec<String>,
    }

    /// Probes every episode and sorts it by the audio track the engine selects for remux.
    fn classify(rt: &Runtime, root: &Path) -> Corpus {
        let mut corpus = Corpus {
            root: root.to_path_buf(),
            flac: Vec::new(),
            passthrough: Vec::new(),
        };
        let mut codecs: BTreeMap<String, usize> = BTreeMap::new();
        for file in scan_mkv_files(root, true, None) {
            let id = get_item_id(&file.path, root);
            let selection = match rt.block_on(probe_video(&file.path)) {
                Ok(meta) => select_tracks(&InspectedVideo::new(meta)),
                Err(e) => {
                    eprintln!("  warning: probe failed, leaving out '{id}': {e}");
                    continue;
                }
            };
            if selection.audio_id.is_none() {
                eprintln!("  warning: no audio track, leaving out '{id}'");
                continue;
            }
            *codecs.entry(selection.audio_codec.clone()).or_default() += 1;
            if selection.audio_codec == "A_FLAC" {
                corpus.flac.push(id);
            } else {
                corpus.passthrough.push(id);
            }
        }
        println!("Corpus: {} ({codecs:?})", root.display());
        corpus
    }

    /// The first release directory that already has an `Output/` the bench would write into.
    fn existing_output_dir(root: &Path) -> Option<PathBuf> {
        scan_mkv_files(root, true, None)
            .iter()
            .filter_map(|f| f.path.parent().map(|p| p.join("Output")))
            .find(|out| out.symlink_metadata().is_ok())
    }

    fn run_cell(rt: &Runtime, corpus: &Corpus, disk: &str, cell: Cell, items: &[String]) {
        let request = BatchRequest {
            mode: PipelineMode::Remux,
            input_dir: corpus.root.clone(),
            workers: cell.workers,
            io_jobs: Some(IO_JOBS),
            // The persistent cache lives in the user's home; each cell probes its sources anew.
            no_cache: Some(true),
            selected_items: Some(items.to_vec()),
            ..Default::default()
        };
        let config = match request.resolve() {
            Ok(config) => config,
            Err(e) => {
                eprintln!("  cell skipped, invalid request: {e}");
                return;
            }
        };
        let workers = match cell.workers {
            Some(w) => w.to_string(),
            None => format!("{} (default)", config.workers),
        };
        let ffmpeg_threads = config.ffmpeg_threads;

        // Declared before the run, so the outputs go even if the run fails or panics.
        let (_outputs, files) = match CellOutputs::snapshot(&corpus.root) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                eprintln!("  cell skipped, corpus snapshot incomplete: {e}");
                return;
            }
        };
        evict_page_cache(&files);

        let pipeline = BatchPipeline::new(config, None, CancellationToken::new());
        let cpu_before = read_cpu_times();
        let start = Instant::now();
        let result = rt.block_on(pipeline.run());
        let wall = start.elapsed().as_secs_f64();
        let cpu = cpu_before
            .zip(read_cpu_times())
            .and_then(|(b, a)| cpu_use(b, a));

        let PipelineSummary {
            completed,
            skipped,
            errors,
            ..
        } = match result {
            Ok(summary) => summary,
            Err(e) => {
                eprintln!("  cell failed: {e}");
                return;
            }
        };
        let (busy, iowait) = cpu.map_or(("n/a".into(), "n/a".into()), |(b, w)| {
            (format!("{b:.1}"), format!("{w:.1}"))
        });
        println!(
            "{disk:<4} {:<8} {:>5} {workers:>13} {IO_JOBS:>7} {ffmpeg_threads:>7} {wall:>9.1} {:>8.3} {busy:>6} {iowait:>7}   {completed}/{skipped}/{errors}",
            cell.kind.label(),
            items.len(),
            completed as f64 / wall,
        );
        if skipped + errors > 0 {
            eprintln!("  warning: {skipped} skipped, {errors} errors; items/s counts completed items only");
        }
    }

    pub fn main() {
        self_check();

        let Some(root) = std::env::var_os("MKV_BENCH_CORPUS").map(PathBuf::from) else {
            println!("Skipping real-media bench: set MKV_BENCH_CORPUS (see the header of benches/bench_real_media.rs)");
            return;
        };
        let missing: Vec<&str> = MEDIA_TOOLS
            .iter()
            .copied()
            .filter(|tool| Command::new(tool).arg("--version").output().is_err())
            .collect();
        if !missing.is_empty() {
            println!(
                "Skipping real-media bench: install {} to run it",
                missing.join(", ")
            );
            return;
        }

        let disk = std::env::var("MKV_BENCH_DISK").unwrap_or_default();
        let only = match std::env::var("MKV_BENCH_BATCH").as_deref() {
            Err(_) | Ok("") => None,
            Ok("flac") => Some(BatchKind::AllFlac),
            Ok("mixed") => Some(BatchKind::Mixed),
            Ok(other) => {
                eprintln!("MKV_BENCH_BATCH must be 'flac' or 'mixed', got '{other}'");
                std::process::exit(2);
            }
        };
        if !root.is_dir() {
            eprintln!("MKV_BENCH_CORPUS is not a directory: '{}'", root.display());
            std::process::exit(2);
        }
        // The engine's default worker count (physical cores from the crate-private `cpu`, clamped
        // to 1-16), read through the public request API. The second observation in the header is
        // about filling those default slots.
        let default_workers = match (BatchRequest {
            mode: PipelineMode::Remux,
            input_dir: root.clone(),
            ..Default::default()
        })
        .resolve()
        {
            Ok(config) => config.workers,
            Err(e) => {
                eprintln!("Could not read the engine's default worker count: {e}");
                std::process::exit(2);
            }
        };
        let cells = grid(&disk, only, default_workers).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(2);
        });
        let corpus_walk = walk(&root);
        if corpus_walk.unreadable > 0 {
            eprintln!(
                "Refusing to run: {} unreadable directories or entries under '{}'",
                corpus_walk.unreadable,
                root.display()
            );
            std::process::exit(2);
        }
        if let Some((link, _)) = corpus_walk.entries.iter().find(|(_, t)| t.is_symlink()) {
            eprintln!(
                "Refusing to run: '{}' is a symlink; copy the sources into the corpus instead",
                link.display()
            );
            std::process::exit(2);
        }
        if let Some(out) = existing_output_dir(&root) {
            eprintln!(
                "Refusing to run: '{}' already exists and remux would overwrite files in it",
                out.display()
            );
            std::process::exit(2);
        }

        let rt = Runtime::new().expect("tokio runtime");
        let corpus = classify(&rt, &root);
        let mixed = interleave(&corpus.flac, &corpus.passthrough);
        println!(
            "Default workers: {default_workers}; FLAC items: {}; passthrough items: {}; mixed batch: {} items",
            corpus.flac.len(),
            corpus.passthrough.len(),
            mixed.len()
        );
        println!();
        println!(
            "disk batch    items       workers io_jobs ffmpeg_t    wall_s  items/s   cpu% iowait%   done/skip/err"
        );

        for cell in cells {
            let items = match cell.kind {
                BatchKind::AllFlac => &corpus.flac,
                BatchKind::Mixed => &mixed,
            };
            if items.is_empty() {
                eprintln!(
                    "  {} cell skipped: the corpus has no items for it",
                    cell.kind.label()
                );
                continue;
            }
            run_cell(&rt, &corpus, &disk, cell, items);
        }
    }
}
