//! CLI adapter for mkv-core batch processing engine.

mod args;

use std::process::ExitCode;
use std::sync::Arc;

use mkv_core::manifest::{generate_inspection_manifest, InspectionManifest};
use mkv_core::pipeline::{create_event_channel, BatchPipeline, CancellationToken};
use mkv_core::request::BatchRequest;
use mkv_core::scan::PersistentInspectCache;

/// Builds the `--plan` manifest, restricted to and ordered by `selected_items` when set.
async fn generate_plan_manifest(
    request: &BatchRequest,
    cache: Option<Arc<PersistentInspectCache>>,
    cancel_token: Option<&CancellationToken>,
) -> Result<InspectionManifest, String> {
    let manifest = generate_inspection_manifest(&request.input_dir, cache, cancel_token).await?;
    match &request.selected_items {
        Some(selected) => manifest.select_items(selected),
        None => Ok(manifest),
    }
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
