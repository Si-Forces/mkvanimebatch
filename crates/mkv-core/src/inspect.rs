//! Bulk video inspection: the one cancellable probe fan-out behind preflight Inspect and remux
//! planning.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::{Id, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::cpu::probe_concurrency;
use crate::scan::{get_or_probe_video, InspectionResult, ParsedFile, PersistentInspectCache};

/// Probes every file in `files` through `cache`, at most `probe_concurrency(workers)` at a time.
///
/// Returns exactly one [`InspectionResult`] per file, in input order: a file that cannot be
/// probed, or whose probe task panics, gets a failure result of its own. Returns `None` once
/// `cancel_token` fires, after aborting the outstanding probes. `cache.save_shared()` runs on
/// every exit; pruning is left to the caller, because only a full scan may prune.
pub(crate) async fn inspect_videos(
    files: &[ParsedFile],
    cache: Option<&Arc<PersistentInspectCache>>,
    workers: usize,
    cancel_token: &CancellationToken,
) -> Option<Vec<InspectionResult>> {
    let probe_sem = Arc::new(Semaphore::new(probe_concurrency(workers)));
    let mut probe_set = JoinSet::new();
    // Task ID to input index, so a panicked task is attributed to its own file.
    let mut index_by_task: HashMap<Id, usize> = HashMap::with_capacity(files.len());

    // The cache lookup and store run inside each probe task, so they overlap with probing and
    // are aborted with it on cancel.
    for (index, file) in files.iter().enumerate() {
        let path = file.path.clone();
        let cache = cache.cloned();
        let sem = Arc::clone(&probe_sem);
        let task = probe_set.spawn(async move {
            // The semaphore is local to this call and never closed, so `acquire` cannot fail.
            let _permit = sem.acquire().await.ok();
            get_or_probe_video(&path, cache.as_deref()).await
        });
        index_by_task.insert(task.id(), index);
    }

    let mut results: Vec<(usize, InspectionResult)> = Vec::with_capacity(files.len());
    let inspections = loop {
        let (id, result) = tokio::select! {
            // A token cancelled before the call wins over probes that already finished.
            biased;
            _ = cancel_token.cancelled() => {
                probe_set.abort_all();
                break None;
            }
            joined = probe_set.join_next_with_id() => match joined {
                Some(Ok((id, Ok(meta)))) => (id, InspectionResult::success(meta)),
                Some(Ok((id, Err(err)))) => (id, InspectionResult::failure(err)),
                Some(Err(join_err)) => {
                    let id = join_err.id();
                    warn!(
                        "Probe task failed for {}: {join_err}",
                        files[index_by_task[&id]].path.display()
                    );
                    (id, InspectionResult::failure(format!("Probe task failed: {join_err}")))
                }
                None => {
                    // Every spawned task reported once, so `results` holds one entry per file.
                    results.sort_unstable_by_key(|(index, _)| *index);
                    break Some(results.into_iter().map(|(_, result)| result).collect());
                }
            },
        };
        results.push((index_by_task[&id], result));
    };

    if let Some(cache) = cache {
        cache.save_shared();
    }
    inspections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{MkvTrack, TrackProperties};
    use crate::scan::{create_parsed_file, MkvmergeMetadata};

    /// Metadata that tells files apart: one track whose ID is `id`.
    fn metadata_with_track_id(id: u32) -> MkvmergeMetadata {
        MkvmergeMetadata {
            tracks: vec![MkvTrack {
                id: Some(id),
                track_type: "video".to_string(),
                codec: None,
                properties: TrackProperties::default(),
            }],
            ..Default::default()
        }
    }

    /// Writes `name` as a non-Matroska file under `dir`, so only a cache hit can inspect it.
    fn junk_file(dir: &std::path::Path, name: &str) -> ParsedFile {
        let path = dir.join(name);
        std::fs::write(&path, b"not matroska").unwrap();
        create_parsed_file(path)
    }

    #[tokio::test]
    async fn test_cancelled_token_returns_none_and_flushes_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir.path().join("inspect_cache.json");
        let cache = Arc::new(PersistentInspectCache::new(Some(cache_file.clone()), true));
        let files = [junk_file(dir.path(), "ep01.mkv")];
        cache.put_path(&files[0].path, MkvmergeMetadata::default());
        let token = CancellationToken::new();
        token.cancel();

        assert!(inspect_videos(&files, Some(&cache), 2, &token)
            .await
            .is_none());
        assert!(
            cache_file.exists(),
            "a cancelled call still saves the cache"
        );
    }

    #[tokio::test]
    async fn test_seeded_cache_answers_every_file_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir.path().join("inspect_cache.json");
        let cache = Arc::new(PersistentInspectCache::new(Some(cache_file.clone()), true));
        // Input order differs from name order, so a result sorted by name fails. Cache hits finish
        // in spawn order, so completion order cannot differ here: the next test pins that.
        let files: Vec<ParsedFile> = ["ep3.mkv", "ep1.mkv", "ep4.mkv", "ep2.mkv"]
            .iter()
            .map(|name| junk_file(dir.path(), name))
            .collect();
        for (id, file) in (1u32..).zip(&files) {
            cache.put_path(&file.path, metadata_with_track_id(id));
        }

        let results = inspect_videos(&files, Some(&cache), 2, &CancellationToken::new())
            .await
            .unwrap();

        // The files are not Matroska, so only the cache can have produced these.
        let expected: Vec<InspectionResult> = (1..=4)
            .map(|id| InspectionResult::success(metadata_with_track_id(id)))
            .collect();
        assert_eq!(results, expected);
        assert!(cache_file.exists(), "a completed call saves the cache");
    }

    #[tokio::test]
    async fn test_unprobeable_files_fail_alone_in_input_order() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(PersistentInspectCache::new(
            Some(dir.path().join("inspect_cache.json")),
            true,
        ));
        let cached_first = junk_file(dir.path(), "ep1.mkv");
        let not_media = junk_file(dir.path(), "ep2.mkv");
        let missing = create_parsed_file(dir.path().join("ep3.mkv"));
        let cached_last = junk_file(dir.path(), "ep4.mkv");
        cache.put_path(&cached_first.path, metadata_with_track_id(1));
        cache.put_path(&cached_last.path, metadata_with_track_id(4));
        let files = [cached_first, not_media, missing, cached_last];

        let results = inspect_videos(&files, Some(&cache), 2, &CancellationToken::new())
            .await
            .unwrap();

        // The unprobeable files finish after both cached ones, so a completion-ordered result
        // would put the last cached file at index 1.
        assert_eq!(results.len(), 4);
        assert_eq!(
            results[0],
            InspectionResult::success(metadata_with_track_id(1))
        );
        assert_eq!(
            results[3],
            InspectionResult::success(metadata_with_track_id(4))
        );
        for failed in &results[1..3] {
            assert!(
                failed.video.is_none() && failed.error.is_some(),
                "{failed:?}"
            );
        }
    }
}
