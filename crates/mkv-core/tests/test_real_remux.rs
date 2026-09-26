use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

use mkv_core::pipeline::{BatchPipeline, CancellationToken, PipelineConfig, PipelineMode};

fn media_command(command: &mut Command) {
    let output = command.output().expect("media tool must be installed");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Media binaries this suite drives for real.
const MEDIA_TOOLS: [&str; 3] = ["ffmpeg", "mkvmerge", "mkvextract"];

/// Returns `false` when the real-media regression may be skipped.
///
/// Required verification (CI and the release gate) sets
/// `MKV_REQUIRE_MEDIA_TOOLS=1`, which turns a missing binary into a failure so
/// a release cannot publish with this regression silently skipped. Developer-local
/// runs without the variable keep the documented skip behaviour.
fn media_tools_available() -> bool {
    let missing: Vec<&str> = MEDIA_TOOLS
        .iter()
        .copied()
        .filter(|binary| Command::new(binary).arg("--version").output().is_err())
        .collect();
    if missing.is_empty() {
        return true;
    }
    assert_ne!(
        env::var("MKV_REQUIRE_MEDIA_TOOLS").as_deref(),
        Ok("1"),
        "required media tools are unavailable: {}",
        missing.join(", ")
    );
    eprintln!(
        "Skipping real-media regression: install {} to run it",
        missing.join(", ")
    );
    false
}

async fn remux_media_case(input: &Path, assets: &Path, output: &Path) -> (String, String, Vec<u8>) {
    let pipeline = BatchPipeline::new(
        PipelineConfig {
            mode: PipelineMode::Remux,
            input_dir: input.to_path_buf(),
            out_dir: Some(output.to_path_buf()),
            assets_dir: Some(assets.to_path_buf()),
            no_cache: true,
            ..Default::default()
        },
        None,
        CancellationToken::new(),
    );
    let summary = pipeline.run().await.unwrap();
    assert_eq!(
        (summary.completed, summary.errors),
        (1, 0),
        "{:?}",
        summary.log
    );
    let mkv = output.join("Season 01/Show - 01.mkv");
    let metadata = mkv_core::scan::probe_video_mkvmerge(&mkv).await.unwrap();
    let subtitle = metadata
        .tracks
        .iter()
        .find(|track| track.track_type == "subtitles")
        .unwrap();
    let subtitle_id = subtitle.id.unwrap();
    let subtitle_path = output.join("selected.srt");
    media_command(
        Command::new("mkvextract")
            .arg("tracks")
            .arg(&mkv)
            .arg(format!("{subtitle_id}:{}", subtitle_path.display())),
    );
    let cover = metadata
        .attachments
        .iter()
        .find(|attachment| {
            attachment
                .file_name
                .as_deref()
                .is_some_and(|name| name.starts_with("cover."))
        })
        .unwrap();
    let extracted = output.join("selected-cover");
    media_command(
        Command::new("mkvextract")
            .arg("attachments")
            .arg(&mkv)
            .arg(format!("{}:{}", cover.id.unwrap(), extracted.display())),
    );
    (
        subtitle.properties.language.clone().unwrap(),
        fs::read_to_string(subtitle_path).unwrap(),
        fs::read(extracted).unwrap(),
    )
}

#[tokio::test]
async fn test_real_remux_subtitle_ladder_and_advertised_cover_names() {
    if !media_tools_available() {
        return;
    }
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("input");
    let season = input.join("Season 01");
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&season).unwrap();
    fs::create_dir_all(&assets).unwrap();
    let video = season.join("Show - 01.mkv");
    let embedded = tmp.path().join("embedded.srt");
    fs::write(
        &embedded,
        b"1\n00:00:00,000 --> 00:00:00,900\nEMBEDDED INDONESIAN\n",
    )
    .unwrap();
    let raw = tmp.path().join("raw.mkv");
    media_command(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:r=1:d=1",
                "-i",
            ])
            .arg(&embedded)
            .args([
                "-map",
                "0:v",
                "-map",
                "1:s",
                "-c:v",
                "mpeg4",
                "-c:s",
                "srt",
                "-metadata:s:s:0",
                "language=ind",
                "-y",
            ])
            .arg(&raw),
    );
    let source_art = tmp.path().join("source.png");
    fs::write(&source_art, b"SOURCE COVER").unwrap();
    let source_font = tmp.path().join("source.ttf");
    fs::write(&source_font, b"SOURCE FONT").unwrap();
    media_command(
        Command::new("mkvmerge")
            .arg("-o")
            .arg(&video)
            .args([
                "--attachment-name",
                "cover.png",
                "--attachment-mime-type",
                "image/png",
                "--attach-file",
            ])
            .arg(&source_art)
            .args([
                "--attachment-name",
                "Test.ttf",
                "--attachment-mime-type",
                "application/x-truetype-font",
                "--attach-file",
            ])
            .arg(&source_font)
            .arg(&raw),
    );

    let local = season.join("Show - 01.eng.srt");
    let parent = input.join("Show - 01.eng.srt");
    fs::write(&local, b"1\n00:00:00,000 --> 00:00:00,900\nLOCAL ENGLISH\n").unwrap();
    fs::write(
        &parent,
        b"1\n00:00:00,000 --> 00:00:00,900\nPARENT ENGLISH\n",
    )
    .unwrap();
    fs::write(assets.join("cover.jpg"), b"FALLBACK COVER").unwrap();
    fs::write(
        assets.join("Show - 01.eng.srt"),
        b"1\n00:00:00,000 --> 00:00:00,900\nFALLBACK ENGLISH\n",
    )
    .unwrap();

    for (idx, name) in [
        "cover.jpg",
        "folder.jpg",
        "Show - 01_cover.jpg",
        "Show - 01.cover.jpg",
        "Show - 01.jpg",
    ]
    .iter()
    .enumerate()
    {
        let cover = season.join(name);
        let expected = format!("LOCAL COVER {idx}");
        fs::write(&cover, expected.as_bytes()).unwrap();
        let output = tmp.path().join(format!("out-{idx}"));
        let (language, subtitle, attachment) = remux_media_case(&input, &assets, &output).await;
        assert_eq!(language, "eng");
        assert!(subtitle.contains("LOCAL ENGLISH"));
        assert_eq!(attachment, expected.as_bytes());
        if idx == 0 {
            let metadata =
                mkv_core::scan::probe_video_mkvmerge(&output.join("Season 01/Show - 01.mkv"))
                    .await
                    .unwrap();
            assert!(metadata
                .attachments
                .iter()
                .any(|att| att.file_name.as_deref() == Some("Test.ttf")));
        }
        fs::remove_file(cover).unwrap();
    }

    let extract_out = tmp.path().join("extracted");
    let font_out = extract_out.join("Season 01/fonts");
    fs::create_dir_all(&font_out).unwrap();
    let user_font = font_out.join("User.ttf");
    fs::write(&user_font, b"USER FONT").unwrap();
    let extract = BatchPipeline::new(
        PipelineConfig {
            mode: PipelineMode::Extract,
            input_dir: input.clone(),
            out_dir: Some(extract_out),
            no_cache: true,
            ..Default::default()
        },
        None,
        CancellationToken::new(),
    );
    let extracted = extract.run().await.unwrap();
    assert_eq!(
        (extracted.completed, extracted.errors),
        (1, 0),
        "{:?}",
        extracted.log
    );
    assert_eq!(fs::read(font_out.join("Test.ttf")).unwrap(), b"SOURCE FONT");
    assert_eq!(fs::read(user_font).unwrap(), b"USER FONT");

    fs::remove_file(&local).unwrap();
    let ancestor_cover = input.join("folder.jpg");
    fs::write(&ancestor_cover, b"ANCESTOR COVER").unwrap();
    let (language, subtitle, attachment) =
        remux_media_case(&input, &assets, &tmp.path().join("ancestor")).await;
    assert_eq!(language, "ind");
    assert!(subtitle.contains("EMBEDDED INDONESIAN"));
    assert_eq!(attachment, b"ANCESTOR COVER");

    fs::remove_file(ancestor_cover).unwrap();
    let (language, subtitle, attachment) =
        remux_media_case(&input, &assets, &tmp.path().join("embedded")).await;
    assert_eq!(language, "ind");
    assert!(subtitle.contains("EMBEDDED INDONESIAN"));
    assert_eq!(attachment, b"SOURCE COVER");

    let no_embedded = tmp.path().join("no-embedded.mkv");
    media_command(
        Command::new("mkvmerge")
            .arg("-o")
            .arg(&no_embedded)
            .args(["--no-subtitles", "--no-attachments"])
            .arg(&video),
    );
    fs::copy(no_embedded, &video).unwrap();
    let (language, subtitle, attachment) =
        remux_media_case(&input, &assets, &tmp.path().join("fallback")).await;
    assert_eq!(language, "eng");
    assert!(subtitle.contains("PARENT ENGLISH"));
    assert_eq!(attachment, b"FALLBACK COVER");
}
