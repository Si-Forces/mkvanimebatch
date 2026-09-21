//! CLI adapter for mkv-core batch processing engine.

use std::path::PathBuf;
use std::process::ExitCode;

use mkv_core::manifest::generate_inspection_manifest;
use mkv_core::pipeline::{create_event_channel, BatchPipeline, CancellationToken, PipelineMode};
use mkv_core::request::{parse_assignments_json, BatchRequest};
use mkv_core::scan::PersistentInspectCache;

#[derive(Debug, Clone, PartialEq)]
struct CliArgs {
    request: BatchRequest,
    plan: bool,
    events: bool,
    clear_cache: bool,
    help: bool,
    has_input_dir: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for CliArgs {
    fn default() -> Self {
        Self {
            request: BatchRequest::default(),
            plan: false,
            events: false,
            clear_cache: false,
            help: false,
            has_input_dir: false,
        }
    }
}

fn parse_args() -> Result<CliArgs, String> {
    let mut args = parse_args_from(std::env::args().skip(1))?;

    let exe_name = std::env::args()
        .next()
        .and_then(|p| {
            PathBuf::from(p)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    if exe_name.contains("mkv-extract") {
        args.request.mode = PipelineMode::Extract;
    }

    Ok(args)
}

fn parse_args_from<I, T>(iter: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
{
    let raw_args: Vec<String> = iter.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut request = BatchRequest::default();

    let mut plan = false;
    let mut events = false;
    let mut clear_cache = false;
    let mut help = false;
    let mut has_input_dir = false;
    let mut assignments_path: Option<PathBuf> = None;

    let mut it = raw_args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--plan" => plan = true,
            "--events" => events = true,
            "--clear-cache" => clear_cache = true,
            "-h" | "--help" => help = true,
            "--input-dir" => {
                let val = it.next().ok_or("Missing value for --input-dir")?;
                request.input_dir = PathBuf::from(val);
                has_input_dir = true;
            }
            "--out-dir" => {
                let val = it.next().ok_or("Missing value for --out-dir")?;
                request.out_dir = Some(PathBuf::from(val));
            }
            "--fonts-dir" => {
                let val = it.next().ok_or("Missing value for --fonts-dir")?;
                request.fonts_dir = Some(PathBuf::from(val));
            }
            "--assets-dir" => {
                let val = it.next().ok_or("Missing value for --assets-dir")?;
                request.assets_dir = Some(PathBuf::from(val));
            }
            "--preset" => {
                let val = it.next().ok_or("Missing value for --preset")?;
                if mkv_core::preset::lookup_preset(val).is_none() {
                    return Err(format!("Unknown preset profile: '{val}'"));
                }
                request.preset = Some(val.clone());
            }
            "--assignments" => {
                let val = it.next().ok_or("Missing value for --assignments")?;
                assignments_path = Some(PathBuf::from(val));
            }
            "--sub-assignments" => {
                eprintln!("Warning: '--sub-assignments' is deprecated and will be removed in a future release. Use '--assignments' instead.");
                let val = it.next().ok_or("Missing value for --sub-assignments")?;
                assignments_path = Some(PathBuf::from(val));
            }
            "--extract" => {
                request.mode = PipelineMode::Extract;
            }
            "--remux" => {
                request.mode = PipelineMode::Remux;
            }
            "--dry-run" => {
                request.dry_run = Some(true);
            }
            "--no-cache" => {
                request.no_cache = Some(true);
            }
            "--keep-fonts" => {
                request.keep_fonts = Some(true);
            }
            "--sub-offset" => {
                let val = it.next().ok_or("Missing value for --sub-offset")?;
                let offset: f64 = val.parse().map_err(|_| "Invalid number for --sub-offset")?;
                request.sub_offset = Some(offset);
            }
            "--workers" => {
                let val = it.next().ok_or("Missing value for --workers")?;
                let workers: usize = val.parse().map_err(|_| "Invalid integer for --workers")?;
                request.workers = Some(workers);
            }
            "--io-jobs" => {
                let val = it.next().ok_or("Missing value for --io-jobs")?;
                let io_jobs: usize = val.parse().map_err(|_| "Invalid integer for --io-jobs")?;
                request.io_jobs = Some(io_jobs);
            }
            "--ffmpeg-threads" => {
                let val = it.next().ok_or("Missing value for --ffmpeg-threads")?;
                let threads: usize = val
                    .parse()
                    .map_err(|_| "Invalid integer for --ffmpeg-threads")?;
                request.ffmpeg_threads = Some(threads);
            }
            "--bitrate" => {
                let val = it.next().ok_or("Missing value for --bitrate")?;
                let rate = val.as_str();
                let valid_rate = if let Some(digits) = rate.strip_suffix(['k', 'M']) {
                    digits.parse::<u32>().is_ok_and(|n| n > 0)
                } else {
                    rate.parse::<u32>().is_ok_and(|n| n > 0)
                };
                if !valid_rate {
                    return Err(format!(
                        "Invalid bitrate format: '{val}'. Must be a positive value like 128k, 192k, 1M."
                    ));
                }
                request.bitrate = Some(val.clone());
            }
            "--complexity" => {
                let val = it.next().ok_or("Missing value for --complexity")?;
                let complexity: u32 = val
                    .parse()
                    .map_err(|_| "Invalid integer for --complexity")?;
                request.complexity = Some(complexity);
            }
            other => {
                return Err(format!("Unknown argument: {other}"));
            }
        }
    }

    if let Some(path) = assignments_path {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read assignments file '{}': {e}", path.display()))?;
        let assignments = parse_assignments_json(&content)?;
        request.assignments = Some(assignments);
    }

    Ok(CliArgs {
        request,
        plan,
        events,
        clear_cache,
        help,
        has_input_dir,
    })
}

fn print_help() {
    println!(
        r#"mkv-cli - High performance batch anime fansub processing suite

USAGE:
    mkv-cli --input-dir <DIR> [OPTIONS]

OPTIONS:
    --input-dir <DIR>          Source directory containing anime video releases
    --out-dir <DIR>            Destination output directory for processed files
    --preset <PROFILE>         Preset profile (balanced, fast, hifi, custom)
    --assignments <FILE>       Path to JSON file mapping episode IDs to track paths
    --sub-assignments <FILE>   [Deprecated: use --assignments] Path to TrackAssignment JSON
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

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode, String> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            print_help();
            return Err(e);
        }
    };

    if args.help {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let cache = PersistentInspectCache::default();

    if args.clear_cache {
        cache.clear();
        println!("Persistent inspection cache cleared.");
        return Ok(ExitCode::SUCCESS);
    }

    if !args.has_input_dir {
        print_help();
        return Err("--input-dir is required.".into());
    }

    args.request.validate()?;

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

    let cache_opt = if args.request.no_cache.unwrap_or(false) {
        None
    } else {
        Some(&cache)
    };

    if args.plan {
        let manifest =
            generate_inspection_manifest(&args.request.input_dir, cache_opt, Some(&cancel_token))
                .await?;
        match serde_json::to_string_pretty(&manifest) {
            Ok(json) => {
                println!("{json}");
                return Ok(ExitCode::SUCCESS);
            }
            Err(e) => {
                return Err(format!("Failed to serialize manifest: {e}"));
            }
        }
    }

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

    let pipeline = BatchPipeline::from_request(args.request, event_sender, cancel_token.clone())?;

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
                Ok(ExitCode::from(130))
            } else if summary.errors == 0 {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::FAILURE)
            }
        }
        Err(err) => {
            eprintln!("Fatal error: {err}");
            Ok(ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitrate_validation() {
        assert!(parse_args_from(["--bitrate", "0"]).is_err());
        assert!(parse_args_from(["--bitrate", "0k"]).is_err());
        assert!(parse_args_from(["--bitrate", "invalid"]).is_err());

        assert!(parse_args_from(["--bitrate", "128k"]).is_ok());
        assert!(parse_args_from(["--bitrate", "192k"]).is_ok());
        assert!(parse_args_from(["--bitrate", "1M"]).is_ok());
    }

    #[test]
    fn test_complexity_validation() {
        assert!(parse_args_from(["--complexity", "invalid"]).is_err());
        assert_eq!(
            parse_args_from(["--complexity", "11"])
                .unwrap()
                .request
                .complexity,
            Some(11)
        );
        assert_eq!(
            parse_args_from(["--complexity", "0"])
                .unwrap()
                .request
                .complexity,
            Some(0)
        );
        assert_eq!(
            parse_args_from(["--complexity", "10"])
                .unwrap()
                .request
                .complexity,
            Some(10)
        );
    }

    #[test]
    fn test_workers_validation() {
        assert!(parse_args_from(["--workers", "invalid"]).is_err());
        assert_eq!(
            parse_args_from(["--workers", "0"]).unwrap().request.workers,
            Some(0)
        );
        assert_eq!(
            parse_args_from(["--workers", "65"])
                .unwrap()
                .request
                .workers,
            Some(65)
        );
        assert_eq!(
            parse_args_from(["--workers", "4"]).unwrap().request.workers,
            Some(4)
        );
        assert_eq!(
            parse_args_from(["--workers", "64"])
                .unwrap()
                .request
                .workers,
            Some(64)
        );
    }

    #[test]
    fn test_io_jobs_validation() {
        assert!(parse_args_from(["--io-jobs", "invalid"]).is_err());
        assert_eq!(
            parse_args_from(["--io-jobs", "65"])
                .unwrap()
                .request
                .io_jobs,
            Some(65)
        );
        assert_eq!(
            parse_args_from(["--io-jobs", "0"]).unwrap().request.io_jobs,
            Some(0)
        );
        assert_eq!(
            parse_args_from(["--io-jobs", "64"])
                .unwrap()
                .request
                .io_jobs,
            Some(64)
        );
    }

    #[test]
    fn test_ffmpeg_threads_validation() {
        assert!(parse_args_from(["--ffmpeg-threads", "invalid"]).is_err());
        assert_eq!(
            parse_args_from(["--ffmpeg-threads", "0"])
                .unwrap()
                .request
                .ffmpeg_threads,
            Some(0)
        );
        assert_eq!(
            parse_args_from(["--ffmpeg-threads", "33"])
                .unwrap()
                .request
                .ffmpeg_threads,
            Some(33)
        );
        assert_eq!(
            parse_args_from(["--ffmpeg-threads", "1"])
                .unwrap()
                .request
                .ffmpeg_threads,
            Some(1)
        );
        assert_eq!(
            parse_args_from(["--ffmpeg-threads", "32"])
                .unwrap()
                .request
                .ffmpeg_threads,
            Some(32)
        );
    }

    #[test]
    fn test_cli_request_boundary_validation_deferred() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_bounds_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);

        let mut args =
            parse_args_from(["--input-dir", temp_dir.to_str().unwrap(), "--workers", "0"]).unwrap();
        assert_eq!(
            args.request.validate().unwrap_err(),
            "Worker thread count must be between 1 and 64"
        );

        args = parse_args_from(["--input-dir", temp_dir.to_str().unwrap(), "--workers", "65"])
            .unwrap();
        assert_eq!(
            args.request.validate().unwrap_err(),
            "Worker thread count must be between 1 and 64"
        );

        args = parse_args_from(["--input-dir", temp_dir.to_str().unwrap(), "--io-jobs", "65"])
            .unwrap();
        assert_eq!(
            args.request.validate().unwrap_err(),
            "I/O job count must be between 0 and 64"
        );

        args = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--ffmpeg-threads",
            "0",
        ])
        .unwrap();
        assert_eq!(
            args.request.validate().unwrap_err(),
            "FFmpeg thread count must be between 1 and 32"
        );

        args = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--ffmpeg-threads",
            "33",
        ])
        .unwrap();
        assert_eq!(
            args.request.validate().unwrap_err(),
            "FFmpeg thread count must be between 1 and 32"
        );

        args = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--complexity",
            "11",
        ])
        .unwrap();
        assert!(args
            .request
            .validate()
            .unwrap_err()
            .contains("Encoder complexity must be between 0 and 10"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_cli_flags_and_extract() {
        let parsed = parse_args_from([
            "--input-dir",
            "/tmp/anime",
            "--plan",
            "--events",
            "--clear-cache",
            "--extract",
        ])
        .unwrap();

        assert!(parsed.has_input_dir);
        assert_eq!(parsed.request.input_dir, PathBuf::from("/tmp/anime"));
        assert!(parsed.plan);
        assert!(parsed.events);
        assert!(parsed.clear_cache);
        assert_eq!(parsed.request.mode, PipelineMode::Extract);
    }

    #[test]
    fn test_help_flag() {
        let parsed = parse_args_from(["-h"]).unwrap();
        assert!(parsed.help);

        let parsed2 = parse_args_from(["--help"]).unwrap();
        assert!(parsed2.help);
    }

    #[test]
    fn test_unknown_flag() {
        assert!(parse_args_from(["--unknown-flag"]).is_err());
    }

    #[test]
    fn test_preset_parsing() {
        let parsed = parse_args_from(["--preset", "balanced"]).unwrap();
        assert_eq!(parsed.request.preset.as_deref(), Some("balanced"));

        let parsed_fast = parse_args_from(["--preset", "fast"]).unwrap();
        assert_eq!(parsed_fast.request.preset.as_deref(), Some("fast"));

        assert!(parse_args_from(["--preset", "unknown_profile"]).is_err());
    }

    #[test]
    fn test_assignments_parsing() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_assign_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let assign_file = temp_dir.join("assignments.json");
        std::fs::write(
            &assign_file,
            r#"[{"target_video": "ep01.mkv", "asset_path": "ep01.ass"}]"#,
        )
        .unwrap();

        let parsed = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--assignments",
            assign_file.to_str().unwrap(),
        ])
        .unwrap();

        let assignments = parsed.request.assignments.expect("should have assignments");
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].target_video, "ep01.mkv");
        assert_eq!(assignments[0].asset_path, PathBuf::from("ep01.ass"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_sub_assignments_legacy_flag_parsing() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_sub_assign_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let assign_file = temp_dir.join("subs.json");
        std::fs::write(&assign_file, r#"{"ep01.mkv": "ep01.ass"}"#).unwrap();

        let parsed = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--sub-assignments",
            assign_file.to_str().unwrap(),
        ])
        .unwrap();

        let assignments = parsed.request.assignments.expect("should have assignments");
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].target_video, "ep01.mkv");
        assert_eq!(assignments[0].asset_path, PathBuf::from("ep01.ass"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_request_validate_preflight() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_preflight_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let video_file = temp_dir.join("ep01.mkv");
        std::fs::write(&video_file, b"dummy video").unwrap();
        let sub_file = temp_dir.join("ep01.ass");
        std::fs::write(&sub_file, b"dummy subtitle").unwrap();

        let assign_file = temp_dir.join("assignments.json");
        std::fs::write(
            &assign_file,
            r#"[{"target_video": "ep01.mkv", "asset_path": "ep01.ass"}]"#,
        )
        .unwrap();

        let parsed = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--assignments",
            assign_file.to_str().unwrap(),
            "--preset",
            "fast",
            "--workers",
            "4",
        ])
        .unwrap();

        assert!(parsed.request.validate().is_ok());

        // If assigned target file is missing, validation fails fast
        let assign_missing_file = temp_dir.join("assignments_missing.json");
        std::fs::write(
            &assign_missing_file,
            r#"[{"target_video": "missing.mkv", "asset_path": "ep01.ass"}]"#,
        )
        .unwrap();

        let parsed_missing = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--assignments",
            assign_missing_file.to_str().unwrap(),
        ])
        .unwrap();

        assert!(parsed_missing.request.validate().is_err());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
