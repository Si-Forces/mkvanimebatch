//! CLI adapter for mkv-core batch processing engine.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mkv_core::manifest::generate_inspection_manifest;
use mkv_core::pipeline::{create_event_channel, BatchPipeline, CancellationToken, PipelineMode};
use mkv_core::request::{clean_relative_path, parse_assignments_json, BatchRequest};
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

    let mut raw_selected_items: Vec<String> = Vec::new();
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
            "--selected-item" => {
                let val = it.next().ok_or("Missing value for --selected-item")?;
                if val.trim().is_empty() {
                    return Err("Cannot specify empty --selected-item".to_string());
                }
                raw_selected_items.push(val.to_string());
            }
            "--selected-items" => {
                let val = it.next().ok_or("Missing value for --selected-items")?;
                let segments: Vec<String> = val
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if segments.is_empty() {
                    return Err("Cannot specify empty --selected-items".to_string());
                }
                raw_selected_items.extend(segments);
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
                if val.eq_ignore_ascii_case("auto") {
                    request.ffmpeg_threads = None;
                } else {
                    let threads: usize = val
                        .parse()
                        .map_err(|_| "Invalid integer for --ffmpeg-threads")?;
                    request.ffmpeg_threads = Some(threads);
                }
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
    if !raw_selected_items.is_empty() {
        if !has_input_dir {
            return Err(
                "Cannot specify --selected-item or --selected-items without --input-dir"
                    .to_string(),
            );
        }
        let mut normalized_items: Vec<String> = Vec::new();
        for raw in &raw_selected_items {
            if raw.trim().is_empty() {
                continue;
            }
            if let Some(norm) = normalize_selected_item(raw, &request.input_dir)? {
                normalized_items.push(norm);
            }
        }
        request.selected_items = Some(normalized_items);
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
fn escapes_input_dir(input_dir: &Path, rel: &Path) -> bool {
    if let (Ok(canon_in), Ok(canon_target)) = (
        std::fs::canonicalize(input_dir),
        std::fs::canonicalize(input_dir.join(rel)),
    ) {
        return canon_target.strip_prefix(&canon_in).is_err();
    }

    let mut depth: isize = 0;
    for comp in rel.components() {
        match comp {
            std::path::Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            std::path::Component::Normal(_) => {
                depth += 1;
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return true;
            }
            std::path::Component::CurDir => {}
        }
    }
    false
}

fn normalize_selected_item(raw: &str, input_dir: &Path) -> Result<Option<String>, String> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let path = Path::new(raw);

    // Direct prefix match: covers both absolute paths and relative paths matching input_dir
    if let Ok(rel) = path.strip_prefix(input_dir) {
        return Ok(Some(clean_relative_path(&rel.to_string_lossy())));
    }

    let canon_in = std::fs::canonicalize(input_dir).ok();
    if let Some(canon_in) = &canon_in {
        if let Ok(rel) = path.strip_prefix(canon_in) {
            return Ok(Some(clean_relative_path(&rel.to_string_lossy())));
        }
    }

    if path.is_absolute() {
        if let Some(canon_in) = &canon_in {
            if let Ok(canon_path) = std::fs::canonicalize(path) {
                if let Ok(rel) = canon_path.strip_prefix(canon_in) {
                    return Ok(Some(clean_relative_path(&rel.to_string_lossy())));
                }
            } else if let Some(parent) = path.parent() {
                if let Ok(canon_parent) = std::fs::canonicalize(parent) {
                    if let Ok(rel_parent) = canon_parent.strip_prefix(canon_in) {
                        let file_name = path.file_name().unwrap_or_default();
                        let clean =
                            clean_relative_path(&rel_parent.join(file_name).to_string_lossy());
                        return Ok(Some(clean));
                    }
                }
            }
        }
        return Err(format!(
            "Selected item '{raw}' is outside input directory '{}'",
            input_dir.display()
        ));
    }

    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
        && escapes_input_dir(input_dir, path)
    {
        return Err(format!(
            "Selected item '{raw}' is outside input directory '{}'",
            input_dir.display()
        ));
    }

    let target = input_dir.join(path);
    if target.exists() {
        if let Some(canon_in) = &canon_in {
            if let Ok(canon_target) = std::fs::canonicalize(&target) {
                if let Ok(rel) = canon_target.strip_prefix(canon_in) {
                    return Ok(Some(clean_relative_path(&rel.to_string_lossy())));
                }
                return Err(format!(
                    "Selected item '{raw}' is outside input directory '{}'",
                    input_dir.display()
                ));
            }
        }
        Ok(Some(clean_relative_path(
            &path.to_string_lossy().replace('\\', "/"),
        )))
    } else if let Ok(cwd) = std::env::current_dir() {
        let cwd_path = cwd.join(path);
        if cwd_path.exists() {
            if let (Ok(canon_cwd), Some(canon_in)) = (std::fs::canonicalize(&cwd_path), &canon_in) {
                if let Ok(rel) = canon_cwd.strip_prefix(canon_in) {
                    return Ok(Some(clean_relative_path(&rel.to_string_lossy())));
                }
            }
            Err(format!(
                "Selected item '{raw}' is outside input directory '{}'",
                input_dir.display()
            ))
        } else {
            Ok(Some(clean_relative_path(
                &path.to_string_lossy().replace('\\', "/"),
            )))
        }
    } else {
        Ok(Some(clean_relative_path(
            &path.to_string_lossy().replace('\\', "/"),
        )))
    }
}

async fn generate_plan_manifest(
    args: &CliArgs,
    cache: Option<&PersistentInspectCache>,
    cancel_token: Option<&CancellationToken>,
) -> Result<mkv_core::manifest::InspectionManifest, String> {
    let mut manifest =
        generate_inspection_manifest(&args.request.input_dir, cache, cancel_token).await?;
    if let Some(selected) = &args.request.selected_items {
        manifest = manifest.filter_selected(selected);
        if manifest.items.is_empty() && !selected.is_empty() {
            return Err(
                "None of the selected items matched any discovered MKV files in the input directory"
                    .into(),
            );
        }
    }
    Ok(manifest)
}

async fn generate_plan_json(
    args: &CliArgs,
    cache: Option<&PersistentInspectCache>,
    cancel_token: Option<&CancellationToken>,
) -> Result<String, String> {
    let manifest = generate_plan_manifest(args, cache, cancel_token).await?;
    serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {e}"))
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
    --selected-item <PATH>     Include a single release item (repeatable, preserves commas)
    --selected-items <LIST>    Include comma-separated release items (repeatable)
    --assignments <FILE>       Path to JSON file mapping episode IDs to track paths
    --sub-assignments <FILE>   [Deprecated: use --assignments] Path to TrackAssignment JSON
    --fonts-dir <DIR>          Explicit fonts directory override
    --assets-dir <DIR>         Fallback asset directory
    --plan                     Generate structured inspection manifest JSON for queue hydration
    --events                   Emit machine-readable NDJSON events prefixed with '@event: '
    --extract                  Extract best-match subtitles, fonts, chapters, and covers
    --dry-run                  Simulate batch processing without executing remux commands
    --bitrate <RATE>           Opus audio transcoding bitrate (default: 192k)
    --complexity <0-10>        Opus compression complexity level (default: 5)
    --workers <N>              Number of concurrent worker tasks (default: physical cores clamped 1-16)
    --io-jobs <N>              Bounded disk I/O jobs concurrency limit (default: 2)
    --ffmpeg-threads <N|auto>  CPU threads allocated per audio transcode job (default: auto)
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
        let json = generate_plan_json(&args, cache_opt, Some(&cancel_token)).await?;
        println!("{json}");
        return Ok(ExitCode::SUCCESS);
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

    #[test]
    fn test_parse_args_ffmpeg_threads_auto() {
        let parsed_auto =
            parse_args_from(["--input-dir", ".", "--ffmpeg-threads", "auto"]).unwrap();
        assert_eq!(parsed_auto.request.ffmpeg_threads, None);

        let parsed_zero = parse_args_from(["--input-dir", ".", "--ffmpeg-threads", "0"]).unwrap();
        assert_eq!(parsed_zero.request.ffmpeg_threads, Some(0));

        let parsed_fixed = parse_args_from(["--input-dir", ".", "--ffmpeg-threads", "4"]).unwrap();
        assert_eq!(parsed_fixed.request.ffmpeg_threads, Some(4));

        let parsed_invalid = parse_args_from(["--input-dir", ".", "--ffmpeg-threads", "invalid"]);
        assert!(parsed_invalid.is_err());
    }
    #[test]
    fn test_cli_selected_item_literal_comma_preserved() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_literal_comma_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let filename = "[Group] Show - 01 [1080p, AAC].mkv";
        let file_path = temp_dir.join(filename);
        std::fs::write(&file_path, b"dummy").unwrap();

        let parsed = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--selected-item",
            filename,
        ])
        .unwrap();

        let items = parsed
            .request
            .selected_items
            .expect("should have selected_items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], filename);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_cli_selected_items_comma_separated() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_comma_sep_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let ep1 = temp_dir.join("ep01.mkv");
        let ep2 = temp_dir.join("ep02.mkv");
        let ep3 = temp_dir.join("ep03.mkv");
        std::fs::write(&ep1, b"").unwrap();
        std::fs::write(&ep2, b"").unwrap();
        std::fs::write(&ep3, b"").unwrap();

        let parsed = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--selected-items",
            "ep01.mkv,  ep02.mkv ,ep03.mkv ",
        ])
        .unwrap();

        let items = parsed
            .request
            .selected_items
            .expect("should have selected_items");
        assert_eq!(items, vec!["ep01.mkv", "ep02.mkv", "ep03.mkv"]);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_cli_selected_item_missing_input_dir_fails() {
        let err = parse_args_from(["--selected-item", "ep01.mkv"]).unwrap_err();
        assert_eq!(
            err,
            "Cannot specify --selected-item or --selected-items without --input-dir"
        );

        let err2 = parse_args_from(["--selected-items", "ep01.mkv,ep02.mkv"]).unwrap_err();
        assert_eq!(
            err2,
            "Cannot specify --selected-item or --selected-items without --input-dir"
        );
    }

    #[test]
    fn test_cli_selected_item_outside_input_dir_fails() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_outside_{}", std::process::id()));
        let input_dir = temp_dir.join("input");
        let outside_dir = temp_dir.join("outside");
        let _ = std::fs::create_dir_all(&input_dir);
        let _ = std::fs::create_dir_all(&outside_dir);

        let outside_file = outside_dir.join("secret.mkv");
        std::fs::write(&outside_file, b"secret").unwrap();

        // 1. Traversal relative path escapes input_dir
        let err_traversal = parse_args_from([
            "--input-dir",
            input_dir.to_str().unwrap(),
            "--selected-item",
            "../outside/secret.mkv",
        ])
        .unwrap_err();
        assert!(
            err_traversal.contains("is outside input directory"),
            "Error: {err_traversal}"
        );

        // 2. Absolute path outside input_dir
        let err_abs = parse_args_from([
            "--input-dir",
            input_dir.to_str().unwrap(),
            "--selected-item",
            outside_file.to_str().unwrap(),
        ])
        .unwrap_err();
        assert!(
            err_abs.contains("is outside input directory"),
            "Error: {err_abs}"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_cli_selected_item_cwd_relative_resolution() {
        let rel_test_dir =
            PathBuf::from("target").join(format!("test_cli_cwd_res_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&rel_test_dir);
        let file_name = "ep01.mkv";
        let file_path = rel_test_dir.join(file_name);
        std::fs::write(&file_path, b"video").unwrap();

        // Pass path relative to CWD: target/test_cli_cwd_res_.../ep01.mkv
        let rel_path_from_cwd = file_path.to_string_lossy().to_string();

        let parsed = parse_args_from([
            "--input-dir",
            rel_test_dir.to_str().unwrap(),
            "--selected-item",
            &rel_path_from_cwd,
        ])
        .unwrap();

        let items = parsed
            .request
            .selected_items
            .expect("should have selected_items");
        assert_eq!(items, vec!["ep01.mkv"]);

        let _ = std::fs::remove_dir_all(&rel_test_dir);
    }
    #[test]
    fn test_cli_selected_item_relative_input_dir_prefix_stripped() {
        let parsed = parse_args_from([
            "--input-dir",
            "media/anime",
            "--selected-item",
            "media/anime/Season 1/S01E01.mkv",
        ])
        .unwrap();

        let items = parsed
            .request
            .selected_items
            .expect("should have selected_items");
        assert_eq!(items, vec!["Season 1/S01E01.mkv"]);
    }

    #[tokio::test]
    async fn test_cli_plan_selected_items_filters_manifest() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_plan_filter_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let ep1 = temp_dir.join("ep01.mkv");
        let ep2 = temp_dir.join("ep02.mkv");
        std::fs::write(&ep1, b"video 1").unwrap();
        std::fs::write(&ep2, b"video 2").unwrap();

        let parsed = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--plan",
            "--selected-item",
            "ep02.mkv",
        ])
        .unwrap();

        assert!(parsed.plan);
        assert_eq!(
            parsed.request.selected_items.as_deref(),
            Some(&["ep02.mkv".to_string()][..])
        );

        let json = generate_plan_json(&parsed, None, None).await.unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        let items = val["items"].as_array().expect("manifest should have items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], "ep02.mkv");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_cli_selected_item_duplicate_fails_validation() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_dup_sel_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let ep1 = temp_dir.join("ep01.mkv");
        std::fs::write(&ep1, b"video").unwrap();

        let parsed = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--selected-item",
            "ep01.mkv",
            "--selected-item",
            "ep01.mkv",
        ])
        .unwrap();

        let err = parsed.request.validate().unwrap_err();
        assert!(err.contains("Duplicate entry in selected_items: 'ep01.mkv'"));
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_cli_selected_item_empty_flag_fails() {
        assert_eq!(
            parse_args_from(["--input-dir", ".", "--selected-item", ""]).unwrap_err(),
            "Cannot specify empty --selected-item"
        );
        assert_eq!(
            parse_args_from(["--input-dir", ".", "--selected-items", "  ,  "]).unwrap_err(),
            "Cannot specify empty --selected-items"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_cli_selected_item_symlink_escape_fails() {
        let outside_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_sym_out_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&outside_dir);
        let secret = outside_dir.join("secret.mkv");
        std::fs::write(&secret, b"secret video").unwrap();

        let input_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_sym_in_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&input_dir);
        let symlink_path = input_dir.join("leak.mkv");
        std::os::unix::fs::symlink(&secret, &symlink_path).unwrap();

        let err = parse_args_from([
            "--input-dir",
            input_dir.to_str().unwrap(),
            "--selected-item",
            "leak.mkv",
        ])
        .unwrap_err();

        assert!(err.contains("is outside input directory"));
        let _ = std::fs::remove_dir_all(&outside_dir);
        let _ = std::fs::remove_dir_all(&input_dir);
    }

    #[test]
    fn test_cli_selected_item_absolute_nonexistent_inside_reports_not_found() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_abs_non_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let ghost = temp_dir.join("ghost.mkv");

        let parsed = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--selected-item",
            ghost.to_str().unwrap(),
        ])
        .unwrap();

        assert_eq!(
            parsed.request.selected_items.as_deref(),
            Some(&["ghost.mkv".to_string()][..])
        );

        let err = parsed.request.validate().unwrap_err();
        assert!(
            err.contains("Selected item does not exist:"),
            "unexpected error message: {err}"
        );
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_cli_plan_selected_items_unmatched_fails() {
        let temp_dir =
            std::env::temp_dir().join(format!("mkv_cli_test_unmatched_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let ep1 = temp_dir.join("ep01.mkv");
        std::fs::write(&ep1, b"video 1").unwrap();

        let ep_txt = temp_dir.join("readme.txt");
        std::fs::write(&ep_txt, b"text").unwrap();

        let parsed = parse_args_from([
            "--input-dir",
            temp_dir.to_str().unwrap(),
            "--plan",
            "--selected-item",
            "readme.txt",
        ])
        .unwrap();

        let err = generate_plan_manifest(&parsed, None, None)
            .await
            .unwrap_err();
        assert!(err.contains("None of the selected items matched any discovered MKV files"));
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
