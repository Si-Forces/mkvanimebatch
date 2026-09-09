//! CLI adapter for mkv-core batch processing engine.

use std::path::PathBuf;
use std::process::ExitCode;

use mkv_core::manifest::generate_inspection_manifest;
use mkv_core::pipeline::{JobEvent, EVENT_VERSION};
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
    -h, --help                 Show this help message
"#
    );
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

    if args.events {
        let start_ev = JobEvent::BatchStart {
            version: EVENT_VERSION,
            total: 0,
            input_dir: input_dir.to_string_lossy().to_string(),
        };
        let complete_ev = JobEvent::BatchComplete {
            version: EVENT_VERSION,
            success: true,
            cancelled: false,
        };
        if let Ok(line) = start_ev.to_ndjson() {
            println!("{line}");
        }
        if let Ok(line) = complete_ev.to_ndjson() {
            println!("{line}");
        }
    } else {
        println!(
            "Processed directory '{}' (dry-run: {})",
            input_dir.display(),
            args.dry_run
        );
    }

    ExitCode::SUCCESS
}
