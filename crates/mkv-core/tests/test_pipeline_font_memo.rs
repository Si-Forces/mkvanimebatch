//! Proves that an extract run hands its font hash memo to the Font Registry, so a seeded font
//! hashed lazily during the run (ADR-0013 Amendment 9) is memoized and persisted.
//!
//! A scripted runner stands in for `mkvextract`, so the run needs no real tool.

mod common;

use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use common::{seeded_cache, video_metadata, ScriptedRunner, Step};
use mkv_core::fonts::{hash_file, FontHashMemo};
use mkv_core::media_tool::MediaTool;
use mkv_core::pipeline::{BatchPipeline, CancellationToken, PipelineConfig, PipelineMode};
use mkv_core::scan::{FileSignature, MkvAttachment};

#[tokio::test]
async fn extract_run_memoizes_a_seeded_font_hashed_by_a_same_size_promote() {
    const SEEDED: &[u8] = b"seeded font data";
    const STAGED: &[u8] = b"staged font data";
    assert_eq!(SEEDED.len(), STAGED.len(), "precondition: one size bucket");

    let in_tmp = tempdir().unwrap();
    let out_tmp = tempdir().unwrap();

    let ep = in_tmp.path().join("Show - 01.mkv");
    fs::write(&ep, b"dummy mkv content").unwrap();
    let fonts_dir = out_tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();
    let seeded_path = fonts_dir.join("Seeded.ttf");
    fs::write(&seeded_path, SEEDED).unwrap();

    // Writes STAGED to every `<id>:<path>` destination it is given, as mkvextract writes each
    // requested attachment.
    let runner = Arc::new(ScriptedRunner::new([Step::exited(
        MediaTool::Mkvextract,
        0,
        "",
        "",
    )
    .writes_extract_targets(STAGED)]));

    // One font attachment under another name, so the run claims, stages and promotes it.
    // `promote` buckets by the staged file's real length, which the precondition above makes
    // the seeded font's size; the declared `size` only has to be free for `claim`.
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

    let summary = BatchPipeline::new(config, None, CancellationToken::new())
        .with_media_tool_runner(runner.clone())
        .run()
        .await
        .unwrap();
    assert_eq!((summary.completed, summary.errors), (1, 0));
    assert!(matches!(
        runner.calls().as_slice(),
        [(MediaTool::Mkvextract, _)]
    ));
    assert_eq!(fs::read(fonts_dir.join("Incoming.ttf")).unwrap(), STAGED);

    let reloaded = FontHashMemo::new(memo_file);
    let sig = FileSignature::of(&seeded_path).unwrap();
    assert_eq!(
        reloaded.get(&seeded_path, sig),
        Some(hash_file(&seeded_path).unwrap()),
        "the seeded font, hashed by the same-size promote, must reach the run's memo"
    );
}
