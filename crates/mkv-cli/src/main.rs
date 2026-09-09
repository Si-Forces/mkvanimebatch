//! CLI adapter for mkv-core batch processing engine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mkv_core::assets::{prepare_release_directory, ReleasePreparationOptions};
use mkv_core::domain::output_stem;
use mkv_core::extract::extract_mkv_file;
use mkv_core::fonts::{scan_fonts, FontManager};
use mkv_core::manifest::{generate_inspection_manifest, get_item_id};
use mkv_core::pipeline::{ItemOutcome, JobEvent, EVENT_VERSION};
use mkv_core::remux::{execute_remux_job, select_tracks, PlannedJob};
use mkv_core::scan::{
    probe_video, scan_mkv_files, InspectedVideo, ParsedFile, PersistentInspectCache,
};

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
    if args.extract {
        let files = scan_mkv_files(&input_dir, true, None);
        if files.is_empty() {
            println!("No MKV files found in '{}'", input_dir.display());
            return ExitCode::SUCCESS;
        }

        let out_dir = args.out_dir.clone();
        let fonts_dir = args
            .fonts_dir
            .clone()
            .unwrap_or_else(|| out_dir.as_ref().unwrap_or(&input_dir).join("fonts"));
        let font_manager = FontManager::new(fonts_dir, None);

        if args.events {
            let start_ev = JobEvent::BatchStart {
                version: EVENT_VERSION,
                total: files.len(),
                input_dir: input_dir.to_string_lossy().to_string(),
            };
            if let Ok(line) = start_ev.to_ndjson() {
                println!("{line}");
            }
        }

        let mut success_count = 0;
        let mut error_count = 0;

        for file in &files {
            let item_id = get_item_id(&file.path, &input_dir);
            if args.events {
                let item_start = JobEvent::ItemStart {
                    version: EVENT_VERSION,
                    id: item_id.clone(),
                    file: file.name.clone(),
                };
                if let Ok(line) = item_start.to_ndjson() {
                    println!("{line}");
                }
            }

            if args.dry_run {
                println!(
                    "[DRY-RUN] Would extract assets from: {}",
                    file.path.display()
                );
                if args.events {
                    let item_comp = JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id.clone(),
                        file: file.name.clone(),
                        status: ItemOutcome::Completed,
                        error: None,
                    };
                    if let Ok(line) = item_comp.to_ndjson() {
                        println!("{line}");
                    }
                }
                success_count += 1;
                continue;
            }

            let meta = if let Some(cached) = cache_opt.and_then(|c| c.get_path(&file.path)) {
                cached
            } else {
                match probe_video(&file.path) {
                    Ok(m) => {
                        if let Some(c) = cache_opt {
                            c.put_path(&file.path, m.clone());
                        }
                        m
                    }
                    Err(err) => {
                        eprintln!("  ERROR: Failed to probe '{}': {err}", file.path.display());
                        if args.events {
                            let item_comp = JobEvent::ItemComplete {
                                version: EVENT_VERSION,
                                id: item_id.clone(),
                                file: file.name.clone(),
                                status: ItemOutcome::Error,
                                error: Some(err),
                            };
                            if let Ok(line) = item_comp.to_ndjson() {
                                println!("{line}");
                            }
                        }
                        error_count += 1;
                        continue;
                    }
                }
            };

            let inspected = InspectedVideo::new(meta);
            match extract_mkv_file(&file.path, &inspected, &font_manager, out_dir.as_deref()).await
            {
                Ok(summary) => {
                    for line in summary.log {
                        println!("{line}");
                    }
                    if args.events {
                        let item_comp = JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id.clone(),
                            file: file.name.clone(),
                            status: ItemOutcome::Completed,
                            error: None,
                        };
                        if let Ok(line) = item_comp.to_ndjson() {
                            println!("{line}");
                        }
                    }
                    success_count += 1;
                }
                Err(err) => {
                    eprintln!(
                        "  ERROR: Extraction failed for '{}': {err}",
                        file.path.display()
                    );
                    if args.events {
                        let item_comp = JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id.clone(),
                            file: file.name.clone(),
                            status: ItemOutcome::Error,
                            error: Some(err),
                        };
                        if let Ok(line) = item_comp.to_ndjson() {
                            println!("{line}");
                        }
                    }
                    error_count += 1;
                }
            }
        }

        if let Some(c) = cache_opt {
            c.save_shared();
        }

        if args.events {
            let batch_comp = JobEvent::BatchComplete {
                version: EVENT_VERSION,
                success: error_count == 0,
                cancelled: false,
            };
            if let Ok(line) = batch_comp.to_ndjson() {
                println!("{line}");
            }
        } else {
            println!(
                "Extraction complete. Successfully processed {success_count} file(s) with {error_count} error(s)."
            );
        }

        return if error_count == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    // Remux Batch Mode
    let files = scan_mkv_files(&input_dir, true, None);
    if files.is_empty() {
        println!("No MKV files found in '{}'", input_dir.display());
        return ExitCode::SUCCESS;
    }

    let mut videos_by_dir: BTreeMap<PathBuf, Vec<ParsedFile>> = BTreeMap::new();
    for file in files {
        let parent = file.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        videos_by_dir.entry(parent).or_default().push(file);
    }

    let mut total_videos = 0;
    for v in videos_by_dir.values() {
        total_videos += v.len();
    }

    if args.events {
        let start_ev = JobEvent::BatchStart {
            version: EVENT_VERSION,
            total: total_videos,
            input_dir: input_dir.to_string_lossy().to_string(),
        };
        if let Ok(line) = start_ev.to_ndjson() {
            println!("{line}");
        }
    }

    let mut success_count = 0;
    let mut error_count = 0;

    for (mkv_dir, videos) in videos_by_dir {
        let prep_opts = ReleasePreparationOptions {
            fonts_override: args.fonts_dir.as_deref(),
            assets_dir: args.assets_dir.as_deref(),
            ..Default::default()
        };
        let prepared = prepare_release_directory(&mkv_dir, &input_dir, &videos, prep_opts);
        let font_info = scan_fonts(&prepared.fonts_dir, None);

        for video_file in videos {
            let item_id = get_item_id(&video_file.path, &input_dir);
            if args.events {
                let item_start = JobEvent::ItemStart {
                    version: EVENT_VERSION,
                    id: item_id.clone(),
                    file: video_file.name.clone(),
                };
                if let Ok(line) = item_start.to_ndjson() {
                    println!("{line}");
                }
            }

            let Some(parsed) = &video_file.parsed else {
                eprintln!(
                    "Skipping '{}': Could not extract episode number from filename.",
                    video_file.name
                );
                if args.events {
                    let item_comp = JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id.clone(),
                        file: video_file.name.clone(),
                        status: ItemOutcome::Skipped,
                        error: Some("Could not extract episode number".into()),
                    };
                    if let Ok(line) = item_comp.to_ndjson() {
                        println!("{line}");
                    }
                }
                continue;
            };

            let base = video_file
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "video".to_string());
            let out_stem = output_stem(&base, &parsed.ep_type, &parsed.ep);
            let out_dir_base = args
                .out_dir
                .clone()
                .unwrap_or_else(|| mkv_dir.join("Output"));
            let output_file = out_dir_base.join(format!("{out_stem}.mkv"));

            let meta = if let Some(cached) = cache_opt.and_then(|c| c.get_path(&video_file.path)) {
                cached
            } else {
                match probe_video(&video_file.path) {
                    Ok(m) => {
                        if let Some(c) = cache_opt {
                            c.put_path(&video_file.path, m.clone());
                        }
                        m
                    }
                    Err(err) => {
                        eprintln!(
                            "  ERROR: Failed to probe '{}': {err}",
                            video_file.path.display()
                        );
                        if args.events {
                            let item_comp = JobEvent::ItemComplete {
                                version: EVENT_VERSION,
                                id: item_id.clone(),
                                file: video_file.name.clone(),
                                status: ItemOutcome::Error,
                                error: Some(err),
                            };
                            if let Ok(line) = item_comp.to_ndjson() {
                                println!("{line}");
                            }
                        }
                        error_count += 1;
                        continue;
                    }
                }
            };

            let inspected = InspectedVideo::new(meta);
            let res_assets =
                prepared
                    .resolver
                    .resolve_for(&video_file, Some(&inspected), None, None, None);

            if !res_assets.matched_subtitle {
                let reason = res_assets
                    .skip_reason
                    .unwrap_or_else(|| "No matching subtitle found".into());
                eprintln!("Skipping '{}': {reason}", video_file.name);
                if args.events {
                    let item_comp = JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id.clone(),
                        file: video_file.name.clone(),
                        status: ItemOutcome::Skipped,
                        error: Some(reason),
                    };
                    if let Ok(line) = item_comp.to_ndjson() {
                        println!("{line}");
                    }
                }
                continue;
            }

            let job = PlannedJob {
                video: video_file.path.clone(),
                video_name: video_file.name.clone(),
                base,
                output_file,
                subtitle_file: res_assets.subtitle_file,
                subtitle_label: res_assets.subtitle_label,
                subtitle_mkv_lang: res_assets.subtitle_mkv_lang,
                chapter_file: res_assets.chapter_file,
                cover_file: res_assets.cover_file,
                embedded_sub_id: res_assets.embedded_sub_id,
                use_source_fonts: font_info.count == 0,
                release_dir: Some(mkv_dir.clone()),
                item_id: item_id.clone(),
            };

            let selection = select_tracks(&inspected);

            if args.dry_run {
                println!(
                    "[DRY-RUN] Would remux '{}' -> '{}'",
                    video_file.name,
                    job.output_file.display()
                );
                if args.events {
                    let item_comp = JobEvent::ItemComplete {
                        version: EVENT_VERSION,
                        id: item_id.clone(),
                        file: video_file.name.clone(),
                        status: ItemOutcome::Completed,
                        error: None,
                    };
                    if let Ok(line) = item_comp.to_ndjson() {
                        println!("{line}");
                    }
                }
                success_count += 1;
                continue;
            }

            let ffmpeg_threads = args.ffmpeg_threads.unwrap_or(2) as usize;
            match execute_remux_job(
                &job,
                &selection,
                &font_info,
                &args.bitrate,
                args.complexity,
                ffmpeg_threads,
                args.sub_offset,
            )
            .await
            {
                Ok(summary) => {
                    for line in summary.log {
                        println!("{line}");
                    }
                    if args.events {
                        let item_comp = JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id.clone(),
                            file: video_file.name.clone(),
                            status: ItemOutcome::Completed,
                            error: None,
                        };
                        if let Ok(line) = item_comp.to_ndjson() {
                            println!("{line}");
                        }
                    }
                    success_count += 1;
                }
                Err(err) => {
                    eprintln!("  ERROR: Remux failed for '{}': {err}", video_file.name);
                    if args.events {
                        let item_comp = JobEvent::ItemComplete {
                            version: EVENT_VERSION,
                            id: item_id.clone(),
                            file: video_file.name.clone(),
                            status: ItemOutcome::Error,
                            error: Some(err),
                        };
                        if let Ok(line) = item_comp.to_ndjson() {
                            println!("{line}");
                        }
                    }
                    error_count += 1;
                }
            }
        }
    }

    if let Some(c) = cache_opt {
        c.save_shared();
    }

    if args.events {
        let batch_comp = JobEvent::BatchComplete {
            version: EVENT_VERSION,
            success: error_count == 0,
            cancelled: false,
        };
        if let Ok(line) = batch_comp.to_ndjson() {
            println!("{line}");
        }
    } else {
        println!(
            "Remux complete. Successfully processed {success_count} file(s) with {error_count} error(s)."
        );
    }

    if error_count == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
