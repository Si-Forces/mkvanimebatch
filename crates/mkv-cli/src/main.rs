//! CLI adapter for mkv-core batch processing engine.

mod args;

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;

use mkv_core::manifest::{generate_inspection_manifest_for, InspectionManifest};
use mkv_core::pipeline::{
    check_destinations, create_event_channel, BatchPipeline, CancellationToken,
};
use mkv_core::request::BatchRequest;
use mkv_core::scan::{scan_mkv_files, PersistentInspectCache};
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

/// Routes Engine Diagnostics to stderr, keeping stdout for `--plan` JSON, `--events` NDJSON and the
/// summary. `RUST_LOG` sets the filter (default `warn`); the line is the level and the message,
/// with color only on a terminal. A subscriber that is already set is left alone. The desktop's
/// stderr layer (`desktop-tauri/src-tauri/src/diagnostics.rs`) uses the same filter and format.
fn init_diagnostics() {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .from_env_lossy();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .without_time()
        .with_target(false)
        .try_init();
}

/// Builds the `--plan` manifest, restricted to and ordered by `selected_items` when set. It first
/// refuses, before any probe, the selected items a run would refuse for their destinations. The
/// input is scanned once, so the manifest lists exactly the files checked.
async fn generate_plan_manifest(
    request: &BatchRequest,
    cache: Option<Arc<PersistentInspectCache>>,
    cancel_token: Option<&CancellationToken>,
) -> Result<InspectionManifest, String> {
    let discovered = scan_mkv_files(&request.input_dir, true, None);
    check_destinations(
        request.mode,
        &discovered,
        &request.input_dir,
        request.out_dir.as_deref(),
        request.selected_items.as_deref(),
    )?;
    let manifest =
        generate_inspection_manifest_for(&request.input_dir, &discovered, cache, cancel_token)
            .await?;
    match &request.selected_items {
        Some(selected) => manifest.select_items(selected),
        None => Ok(manifest),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    init_diagnostics();
    match run().await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode, String> {
    let args = args::parse_args().inspect_err(|_| args::print_help())?;

    if args.help {
        args::print_help();
        return Ok(ExitCode::SUCCESS);
    }

    if args.clear_cache {
        // A disabled instance deletes the file without parsing it first.
        PersistentInspectCache::new(None, false).clear();
        println!("Persistent inspection cache cleared.");
        return Ok(ExitCode::SUCCESS);
    }

    if !args.has_input_dir {
        args::print_help();
        return Err("--input-dir is required.".into());
    }

    // Resolves before any work starts, so a bad request fails before `--plan` or a run begins.
    // `--plan` still reads the request, so it resolves a copy.
    let config = args.request.clone().resolve()?;

    let cancel_token = CancellationToken::new();
    let signal_token = cancel_token.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            match signal(SignalKind::terminate()) {
                Ok(mut sigterm) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = sigterm.recv() => {}
                    }
                }
                Err(e) => {
                    eprintln!("Warning: Failed to bind SIGTERM listener: {e}");
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
            signal_token.cancel();
        }
        #[cfg(not(unix))]
        {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal_token.cancel();
            }
        }
    });

    if args.plan {
        // Only `--plan` uses this cache; a batch run loads its own unless `--no-cache` is set.
        let cache_opt = (!args.request.no_cache.unwrap_or(false))
            .then(|| Arc::new(PersistentInspectCache::default()));
        let manifest =
            generate_plan_manifest(&args.request, cache_opt, Some(&cancel_token)).await?;
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| format!("Failed to serialize manifest: {e}"))?;
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

    let pipeline = BatchPipeline::new(config, event_sender, cancel_token);

    let result = pipeline.run().await;
    // Dropping the pipeline closes the event channel so the forwarder can drain and exit.
    drop(pipeline);
    if let Some(forwarder) = event_forwarder {
        let _ = forwarder.await;
    }

    match result {
        Ok(summary) => {
            if !args.events {
                for line in summary.log {
                    println!("{}", line.text);
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
    use crate::args::test_support::{parse_args_from, path_arg, write_files};
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_cli_plan_selected_items_filters_manifest() {
        let dir = tempdir().unwrap();
        write_files(dir.path(), &["ep01.mkv", "ep02.mkv"]);

        let parsed = parse_args_from([
            "--input-dir",
            path_arg(dir.path()),
            "--plan",
            "--selected-item",
            "ep02.mkv",
            "--selected-item",
            "ep01.mkv",
        ])
        .unwrap();

        assert!(parsed.plan);
        assert_eq!(
            parsed.request.selected_items.as_deref(),
            Some(&["ep02.mkv".to_string(), "ep01.mkv".to_string()][..])
        );

        let manifest = generate_plan_manifest(&parsed.request, None, None)
            .await
            .unwrap();
        let ids: Vec<&str> = manifest.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["ep02.mkv", "ep01.mkv"]);
    }

    // Linux only: the colliding sources differ only by case, which takes a case-sensitive file
    // system; the default Windows and macOS ones are not.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn test_cli_plan_refuses_colliding_remux_destinations() {
        let dir = tempdir().unwrap();
        write_files(
            dir.path(),
            &["Show - 01.mkv", "Show - 01.MKV", "Show - 02.mkv"],
        );
        let plan = |extra: &[&str]| {
            let mut argv = vec!["--input-dir", path_arg(dir.path()), "--plan"];
            argv.extend_from_slice(extra);
            let parsed = parse_args_from(argv).unwrap();
            async move { generate_plan_manifest(&parsed.request, None, None).await }
        };

        // The two IDs tie in natural order, so they keep the directory's listing order.
        let err = plan(&[]).await.unwrap_err();
        let destination = dir.path().join("Output/Show - 01.mkv");
        let expected = |ids: &str| {
            format!(
                "Multiple items would write the same remux output: {ids} -> '{}'",
                destination.display()
            )
        };
        assert!(
            err == expected("'Show - 01.MKV', 'Show - 01.mkv'")
                || err == expected("'Show - 01.mkv', 'Show - 01.MKV'"),
            "{err}"
        );

        let manifest = plan(&["--selected-item", "Show - 01.MKV"]).await.unwrap();
        let ids: Vec<&str> = manifest.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["Show - 01.MKV"]);
    }

    // Linux only: the colliding sources differ only by case, which takes a case-sensitive file
    // system; the default Windows and macOS ones are not.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn test_cli_plan_refuses_extract_items_whose_sidecars_share_names() {
        let dir = tempdir().unwrap();
        write_files(
            dir.path(),
            &["Show - 01.mkv", "Show - 01.MKV", "Show - 02.mkv"],
        );
        let plan = |extra: &[&str]| {
            let mut argv = vec!["--input-dir", path_arg(dir.path()), "--plan", "--extract"];
            argv.extend_from_slice(extra);
            let parsed = parse_args_from(argv).unwrap();
            async move { generate_plan_manifest(&parsed.request, None, None).await }
        };

        // The two IDs tie in natural order, so they keep the directory's listing order.
        let err = plan(&[]).await.unwrap_err();
        let destination = dir.path().join("Show - 01");
        let expected = |ids: &str| {
            format!(
                "Multiple items would extract files with the same names: {ids} -> '{}'",
                destination.display()
            )
        };
        assert!(
            err == expected("'Show - 01.MKV', 'Show - 01.mkv'")
                || err == expected("'Show - 01.mkv', 'Show - 01.MKV'"),
            "{err}"
        );

        let manifest = plan(&["--selected-item", "Show - 01.MKV"]).await.unwrap();
        let ids: Vec<&str> = manifest.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["Show - 01.MKV"]);
    }

    /// `out/A` is a dangling link to `B`, which extracting `B/Show - 01.mkv` would create, so
    /// `A/Show - 01.mkv` would then extract over B's files: the plan fails as the run would.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_cli_plan_refuses_an_extract_output_dir_through_a_dangling_symlink() {
        let dir = tempdir().unwrap();
        let out = tempdir().unwrap();
        write_files(dir.path(), &["A/Show - 01.mkv", "B/Show - 01.mkv"]);
        std::os::unix::fs::symlink("B", out.path().join("A")).unwrap();
        let parsed = parse_args_from([
            "--input-dir",
            path_arg(dir.path()),
            "--out-dir",
            path_arg(out.path()),
            "--plan",
            "--extract",
            "--no-cache",
        ])
        .unwrap();

        let err = generate_plan_manifest(&parsed.request, None, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            format!(
                "Cannot determine the extract destination of 'A/Show - 01.mkv', '{}': '{}' is a symlink whose target does not exist",
                out.path().join("A").join("Show - 01").display(),
                out.path().canonicalize().unwrap().join("A").display()
            )
        );
        assert!(!out.path().join("B").exists());
    }

    #[tokio::test]
    async fn test_cli_plan_selected_items_unmatched_fails() {
        let dir = tempdir().unwrap();
        write_files(dir.path(), &["ep01.mkv", "notes.txt", "Output/hidden.mkv"]);

        let mut parsed = parse_args_from([
            "--input-dir",
            path_arg(dir.path()),
            "--plan",
            "--selected-item",
            "ep01.mkv",
            "--selected-item",
            "notes.txt",
            "--selected-item",
            "Output/hidden.mkv",
        ])
        .unwrap();
        assert!(parsed
            .request
            .clone()
            .resolve()
            .unwrap_err()
            .contains("notes.txt"));
        let err = generate_plan_manifest(&parsed.request, None, None)
            .await
            .unwrap_err();
        assert!(
            err.contains("notes.txt") && err.contains("Output/hidden.mkv"),
            "{err}"
        );

        parsed.request.selected_items = Some(vec!["Output/hidden.mkv".into()]);
        assert!(parsed.request.clone().resolve().is_ok());
        assert!(generate_plan_manifest(&parsed.request, None, None)
            .await
            .unwrap_err()
            .contains("Output/hidden.mkv"));

        parsed.request.selected_items = None;
        std::fs::remove_file(dir.path().join("ep01.mkv")).unwrap();
        let plan = generate_plan_manifest(&parsed.request, None, None)
            .await
            .unwrap();
        assert!(plan.items.is_empty());
    }
}
