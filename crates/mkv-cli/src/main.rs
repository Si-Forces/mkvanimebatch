//! CLI adapter for mkv-core batch processing engine.

use std::path::PathBuf;
use std::process::ExitCode;

use mkv_core::manifest::generate_inspection_manifest;
use mkv_core::pipeline::{
    create_event_channel, BatchPipeline, CancellationToken, PipelineConfig, PipelineMode,
};
use mkv_core::scan::PersistentInspectCache;

#[derive(Debug, Clone, Default)]
struct CliArgs {
    config: PipelineConfig,
    plan: bool,
    events: bool,
    clear_cache: bool,
    help: bool,
    has_input_dir: bool,
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
        args.config.mode = PipelineMode::Extract;
    }

    Ok(args)
}

fn parse_args_from<I, T>(iter: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
{
    let raw_args: Vec<String> = iter.into_iter().map(|s| s.as_ref().to_string()).collect();

    let mut plan = false;
    let mut events = false;
    let mut clear_cache = false;
    let mut help = false;
    let mut has_input_dir = false;

    let mut it = raw_args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--plan" => plan = true,
            "--events" => events = true,
            "--clear-cache" => clear_cache = true,
            "-h" | "--help" => help = true,
            "--input-dir" => {
                has_input_dir = true;
                let _ = it.next();
            }
            "--out-dir" | "--fonts-dir" | "--assets-dir" | "--sub-offset" | "--workers"
            | "--io-jobs" | "--ffmpeg-threads" | "--bitrate" | "--complexity"
            | "--sub-assignments" => {
                let _ = it.next();
            }
            _ => {}
        }
    }

    let config = PipelineConfig::from_args(&raw_args)?;

    Ok(CliArgs {
        config,
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

    if !args.config.input_dir.exists() {
        return Err(format!(
            "Input directory does not exist: {}",
            args.config.input_dir.display()
        ));
    }

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

    let cache_opt = if args.config.no_cache {
        None
    } else {
        Some(&cache)
    };

    if args.plan {
        let manifest =
            generate_inspection_manifest(&args.config.input_dir, cache_opt, Some(&cancel_token))?;
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

    let pipeline = BatchPipeline::new(args.config, event_sender, cancel_token.clone());

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
        assert!(parse_args_from(["--complexity", "11"]).is_err());
        assert!(parse_args_from(["--complexity", "0"]).is_ok());
        assert!(parse_args_from(["--complexity", "10"]).is_ok());
    }

    #[test]
    fn test_workers_validation() {
        assert!(parse_args_from(["--workers", "0"]).is_err());
        assert!(parse_args_from(["--workers", "65"]).is_err());
        assert!(parse_args_from(["--workers", "4"]).is_ok());
        assert!(parse_args_from(["--workers", "64"]).is_ok());
    }

    #[test]
    fn test_io_jobs_validation() {
        assert!(parse_args_from(["--io-jobs", "65"]).is_err());
        assert!(parse_args_from(["--io-jobs", "0"]).is_ok());
        assert!(parse_args_from(["--io-jobs", "64"]).is_ok());
    }

    #[test]
    fn test_ffmpeg_threads_validation() {
        assert!(parse_args_from(["--ffmpeg-threads", "0"]).is_err());
        assert!(parse_args_from(["--ffmpeg-threads", "33"]).is_err());
        assert!(parse_args_from(["--ffmpeg-threads", "1"]).is_ok());
        assert!(parse_args_from(["--ffmpeg-threads", "32"]).is_ok());
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
        assert_eq!(parsed.config.input_dir, PathBuf::from("/tmp/anime"));
        assert!(parsed.plan);
        assert!(parsed.events);
        assert!(parsed.clear_cache);
        assert_eq!(parsed.config.mode, PipelineMode::Extract);
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
}
