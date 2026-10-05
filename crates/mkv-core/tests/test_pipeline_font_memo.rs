//! Proves that an extract run hands its font hash memo to the Font Registry, so a seeded font
//! hashed lazily during the run (ADR-0013 Amendment 9) is memoized and persisted.
//!
//! This lives in its own integration binary for the reason `test_pipeline_assignment_args`
//! does: the stub `mkvextract` goes ahead of the real one on `PATH`, and `PATH` is
//! process-global, so one test per process keeps the mutation from racing other tests. It is
//! Unix-only because the stub is a POSIX shell script.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tempfile::tempdir;

use common::{run_batch, seeded_cache, video_metadata};
use mkv_core::fonts::{hash_file, FontHashMemo};
use mkv_core::pipeline::{CancellationToken, PipelineConfig, PipelineMode};
use mkv_core::scan::{FileSignature, MkvAttachment};

/// Restores the `PATH` it displaced, so a failing assertion cannot leak the stub directory
/// into the rest of the run.
struct EnvReset {
    path: String,
}

impl Drop for EnvReset {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.path);
    }
}

#[tokio::test]
async fn extract_run_memoizes_a_seeded_font_hashed_by_a_same_size_promote() {
    const SEEDED: &[u8] = b"seeded font data";
    const STAGED: &[u8] = b"staged font data";
    assert_eq!(SEEDED.len(), STAGED.len(), "precondition: one size bucket");

    let in_tmp = tempdir().unwrap();
    let out_tmp = tempdir().unwrap();
    let bin_dir = tempdir().unwrap();

    let ep = in_tmp.path().join("Show - 01.mkv");
    fs::write(&ep, b"dummy mkv content").unwrap();
    let fonts_dir = out_tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();
    let seeded = fonts_dir.join("Seeded.ttf");
    fs::write(&seeded, SEEDED).unwrap();

    // Writes STAGED to every `<id>:<path>` destination it is given, as mkvextract writes
    // each requested attachment.
    let script = bin_dir.path().join("mkvextract");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    [0-9]*:*) printf '%s' '{}' > \"${{arg#*:}}\" ;;\n  esac\ndone\n",
            std::str::from_utf8(STAGED).unwrap()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    let _env_guard = EnvReset {
        path: old_path.clone(),
    };
    std::env::set_var("PATH", format!("{}:{old_path}", bin_dir.path().display()));

    // One font attachment of the seeded font's size under another name, so the run claims
    // it, stages it, and reaches `promote` with that size.
    let mut metadata = video_metadata("A_AAC");
    metadata.attachments = vec![MkvAttachment {
        id: Some(1),
        file_name: Some("Incoming.ttf".into()),
        content_type: Some("application/x-truetype-font".into()),
        size: Some(STAGED.len() as u64),
    }];

    let memo_file = out_tmp.path().join("font_memo.json");
    let memo = Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
        memo_file.clone(),
    )));
    let config = PipelineConfig {
        mode: PipelineMode::Extract,
        input_dir: in_tmp.path().to_path_buf(),
        out_dir: Some(out_tmp.path().join("out")),
        fonts_dir: Some(fonts_dir.clone()),
        cache: Some(seeded_cache(in_tmp.path(), &[&ep], &metadata)),
        font_hash_memo: Some(memo),
        workers: 1,
        io_jobs: 1,
        ..Default::default()
    };

    let (summary, _events) = run_batch(config, CancellationToken::new()).await;
    let summary = summary.unwrap();
    assert_eq!((summary.completed, summary.errors), (1, 0));
    assert_eq!(fs::read(fonts_dir.join("Incoming.ttf")).unwrap(), STAGED);

    let reloaded = FontHashMemo::new(memo_file);
    let sig = FileSignature::of(&seeded).unwrap();
    assert_eq!(
        reloaded.get(&seeded, sig),
        Some(hash_file(&seeded).unwrap()),
        "the seeded font, hashed by the same-size promote, must reach the run's memo"
    );
}
