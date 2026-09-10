//! CLI adapter for mkv-core batch processing engine.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mkv_core::manifest::generate_inspection_manifest;
use mkv_core::pipeline::{
    create_event_channel, BatchPipeline, CancellationToken, PipelineConfig, PipelineMode,
};
use mkv_core::scan::PersistentInspectCache;

#[derive(Debug, Default)]
struct CliArgs {
    input_dir: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    sub_assignments: Option<PathBuf>,
    fonts_dir: Option<PathBuf>,
    assets_dir: Option<PathBuf>,
    sub_offset: f64,
    workers: Option<u32>,
    io_jobs: Option<u32>,
    ffmpeg_threads: Option<u32>,
    bitrate: String,
    complexity: u32,
    keep_fonts: bool,
    no_cache: bool,
    clear_cache: bool,
    plan: bool,
    events: bool,
    extract: bool,
    dry_run: bool,
}

fn parse_args() -> Result<CliArgs, String> {
    let mut args = CliArgs {
        bitrate: "192k".to_string(),
        complexity: 10,
        ..Default::default()
    };

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--input-dir" => {
                let val = iter.next().ok_or("Missing value for --input-dir")?;
                args.input_dir = Some(PathBuf::from(val));
            }
            "--out-dir" => {
                let val = iter.next().ok_or("Missing value for --out-dir")?;
                args.out_dir = Some(PathBuf::from(val));
            }
            "--sub-assignments" => {
                let val = iter.next().ok_or("Missing value for --sub-assignments")?;
                args.sub_assignments = Some(PathBuf::from(val));
            }
            "--fonts-dir" => {
                let val = iter.next().ok_or("Missing value for --fonts-dir")?;
                args.fonts_dir = Some(PathBuf::from(val));
            }
            "--assets-dir" => {
                let val = iter.next().ok_or("Missing value for --assets-dir")?;
                args.assets_dir = Some(PathBuf::from(val));
            }
            "--plan" => args.plan = true,
            "--events" => args.events = true,
            "--extract" => args.extract = true,
            "--dry-run" => args.dry_run = true,
            "--no-cache" => args.no_cache = true,
            "--clear-cache" => args.clear_cache = true,
            "--keep-fonts" => args.keep_fonts = true,
            "--sub-offset" => {
                let val = iter.next().ok_or("Missing value for --sub-offset")?;
                args.sub_offset = val.parse().map_err(|_| "Invalid number for --sub-offset")?;
            }
            "--workers" => {
                let val = iter.next().ok_or("Missing value for --workers")?;
                args.workers = Some(val.parse().map_err(|_| "Invalid integer for --workers")?);
            }
            "--io-jobs" => {
                let val = iter.next().ok_or("Missing value for --io-jobs")?;
                args.io_jobs = Some(val.parse().map_err(|_| "Invalid integer for --io-jobs")?);
            }
            "--ffmpeg-threads" => {
                let val = iter.next().ok_or("Missing value for --ffmpeg-threads")?;
                args.ffmpeg_threads = Some(
                    val.parse()
                        .map_err(|_| "Invalid integer for --ffmpeg-threads")?,
                );
            }
            "--bitrate" => {
                args.bitrate = iter.next().ok_or("Missing value for --bitrate")?;
            }
            "--complexity" => {
                let val = iter.next().ok_or("Missing value for --complexity")?;
                args.complexity = val
                    .parse()
                    .map_err(|_| "Invalid integer for --complexity")?;
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => {
                return Err(format!("Unknown argument: {other}"));
            }
        }
    }

    let exe_name = std::env::args()
        .next()
        .and_then(|p| {
            PathBuf::from(p)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    if exe_name.contains("mkv-extract") {
        args.extract = true;
    }

    Ok(args)
}

fn print_help() {
    println!(
        r#"mkv-cli - High performance batch anime fansub processing suite

USAGE:
    mkv-cli --input-dir <DIR> [OPTIONS]

OPTIONS:
    --input-dir <DIR>          Source directory containing anime video releases
    --out-dir <DIR>            Destination output directory for processed files
    --sub-assignments <FILE>   Path to JSON file mapping episode IDs to subtitle paths
    --fonts-dir <DIR>          Explicit fonts directory override
    --assets-dir <DIR>         Fallback asset directory
    --plan                     Generate structured inspection manifest JSON for queue hydration
    --events                   Emit machine-readable NDJSON events prefixed with '@event: '
    --extract                  Extract best-match subtitles, fonts, chapters, and covers
    --dry-run                  Simulate batch processing without executing remux commands
    --bitrate <RATE>           Opus audio transcoding bitrate (default: 192k)
    --complexity <1-10>        Opus compression complexity level (default: 10)
    --workers <N>              Number of concurrent worker tasks (default: 4)
    --io-jobs <N>              Bounded disk I/O jobs concurrency limit (default: 2)
    --ffmpeg-threads <N>       CPU threads allocated per audio transcode job (default: 2)
    --sub-offset <SEC>         Subtitle timestamp offset in seconds
    --keep-fonts               Retain extracted fonts directory
    --no-cache                 Bypass persistent inspect cache
    --clear-cache              Clear persistent inspection cache and exit
    -h, --help                 Show this help message
"#
    );
}

fn load_sub_assignments(path: &Path, input_dir: &Path) -> Result<HashMap<String, PathBuf>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read assignments file '{}': {e}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Invalid assignments JSON in '{}': {e}", path.display()))?;
    let mut map = HashMap::new();
    let insert_entry = |map: &mut HashMap<String, PathBuf>, k: String, v: &str| {
        let p = Path::new(v);
        let resolved = if p.is_relative() {
            input_dir.join(p)
        } else {
            p.to_path_buf()
        };
        map.insert(k, resolved);
    };

    if let Some(obj) = json.get("assignments").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(p) = v.as_str() {
                insert_entry(&mut map, k.clone(), p);
            }
        }
    } else if let Some(obj) = json.as_object() {
        for (k, v) in obj {
            if let Some(p) = v.as_str() {
                insert_entry(&mut map, k.clone(), p);
            }
        }
    }
    Ok(map)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {e}");
            print_help();
            return ExitCode::FAILURE;
        }
    };

    let cache = PersistentInspectCache::default();

    if args.clear_cache {
        cache.clear();
        println!("Persistent inspection cache cleared.");
        return ExitCode::SUCCESS;
    }

    let Some(input_dir) = args.input_dir else {
        eprintln!("Error: --input-dir is required.");
        print_help();
        return ExitCode::FAILURE;
    };

    if !input_dir.exists() {
        eprintln!(
            "Error: Input directory does not exist: {}",
            input_dir.display()
        );
        return ExitCode::FAILURE;
    }

    let cache_opt = if args.no_cache { None } else { Some(&cache) };

    if args.plan {
        let manifest = generate_inspection_manifest(&input_dir, cache_opt);
        match serde_json::to_string_pretty(&manifest) {
            Ok(json) => {
                println!("{json}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("Failed to serialize manifest: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let sub_assignments = match &args.sub_assignments {
        Some(path) => match load_sub_assignments(path, &input_dir) {
            Ok(m) => Some(m),
            Err(err) => {
                eprintln!("Error loading subtitle assignments: {err}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let config = PipelineConfig {
        input_dir,
        out_dir: args.out_dir,
        fonts_dir: args.fonts_dir,
        assets_dir: args.assets_dir,
        mode: if args.extract {
            PipelineMode::Extract
        } else {
            PipelineMode::Remux
        },
        dry_run: args.dry_run,
        no_cache: args.no_cache,
        cache: None,
        cache_file: None,
        workers: args.workers.unwrap_or(4) as usize,
        io_jobs: args.io_jobs.unwrap_or(2) as usize,
        bitrate: args.bitrate,
        complexity: args.complexity,
        ffmpeg_threads: args.ffmpeg_threads.unwrap_or(2) as usize,
        sub_offset: args.sub_offset,
        keep_fonts: args.keep_fonts,
        sub_assignments,
    };

    let cancel_token = CancellationToken::new();
    let cancel_token_clone = cancel_token.clone();

    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("Warning: Failed to bind SIGTERM listener: {e}");
                    None
                }
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    cancel_token_clone.cancel();
                }
                _ = async {
                    if let Some(s) = sigterm.as_mut() {
                        s.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    cancel_token_clone.cancel();
                }
            }
        }
        #[cfg(not(unix))]
        {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel_token_clone.cancel();
            }
        }
    });
    let (event_sender, event_forwarder) = if args.events {
        let (tx, mut rx) = create_event_channel(128);
        let forwarder = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Ok(line) = event.to_ndjson() {
                    println!("{line}");
                }
            }
        });
        (Some(tx), Some(forwarder))
    } else {
        (None, None)
    };

    let pipeline = BatchPipeline::new(config, event_sender, cancel_token.clone());

    let result = pipeline.run().await;
    drop(pipeline);
    if let Some(forwarder) = event_forwarder {
        let _ = forwarder.await;
    }

    match result {
        Ok(summary) => {
            if !args.events {
                for line in summary.log {
                    println!("{line}");
                }
                println!(
                    "Finished. Processed {} file(s) ({} completed, {} skipped, {} errors).",
                    summary.total, summary.completed, summary.skipped, summary.errors
                );
            }
            if summary.cancelled {
                ExitCode::from(130)
            } else if summary.errors == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(err) => {
            eprintln!("Fatal error: {err}");
            ExitCode::FAILURE
        }
    }
}
