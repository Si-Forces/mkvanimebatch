use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

use mkv_core::media_tool::{Invocation, MediaTool, MediaToolRunner, RawOutcome, SubprocessRunner};
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

/// Returns `false` when a Media Tool is missing and the real-media tests may skip.
///
/// CI and the release gate set `MKV_REQUIRE_MEDIA_TOOLS=1`, which turns a missing tool into a
/// failure, so a release cannot ship with these tests silently skipped. Without the variable,
/// a missing tool skips them.
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

/// ffmpeg copies the source chapters into the temporary Opus file. If mkvmerge reads them
/// from both inputs, every chapter appears twice in the output.
#[tokio::test]
async fn test_real_remux_transcoded_flac_keeps_chapters_once() {
    if !media_tools_available() {
        return;
    }
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("input");
    fs::create_dir_all(&input).unwrap();
    let chapters = tmp.path().join("chapters.txt");
    fs::write(
        &chapters,
        ";FFMETADATA1\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=Opening\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Part A\n",
    )
    .unwrap();
    media_command(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:r=1:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=d=1",
                "-i",
            ])
            .arg(&chapters)
            .args([
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map_chapters",
                "2",
                "-c:v",
                "mpeg4",
                "-c:a",
                "flac",
                "-y",
            ])
            .arg(input.join("Show - 01.mkv")),
    );
    // Remux skips a video with no subtitle to add.
    fs::write(
        input.join("Show - 01.eng.srt"),
        b"1\n00:00:00,000 --> 00:00:00,900\nENGLISH\n",
    )
    .unwrap();

    let output = tmp.path().join("out");
    let summary = BatchPipeline::new(
        PipelineConfig {
            mode: PipelineMode::Remux,
            input_dir: input,
            out_dir: Some(output.clone()),
            no_cache: true,
            ..Default::default()
        },
        None,
        CancellationToken::new(),
    )
    .run()
    .await
    .unwrap();
    assert_eq!(
        (summary.completed, summary.errors),
        (1, 0),
        "{:?}",
        summary.log
    );

    let metadata = mkv_core::scan::probe_video_mkvmerge(&output.join("Show - 01.mkv"))
        .await
        .unwrap();
    assert!(metadata
        .tracks
        .iter()
        .any(|track| track.properties.codec_id.as_deref() == Some("A_OPUS")));
    let entries: u32 = metadata
        .chapters
        .iter()
        .filter_map(|chapter| chapter.num_entries)
        .sum();
    assert_eq!(entries, 2, "{:?}", summary.log);
}

/// ffmpeg arguments that keep it running in real time for an hour (`-re` stops it rendering
/// the silence at once), tagged with `marker` so a test can find the process by its command
/// line.
fn long_running_ffmpeg_args(marker: &str) -> Vec<OsString> {
    [
        "-hide_banner",
        "-re",
        "-f",
        "lavfi",
        "-i",
        "anullsrc",
        "-t",
        // Long enough to outlive every test that waits on it, short enough that a kill that
        // failed would not leave the tool running for long.
        "120",
        "-metadata",
        &format!("comment={marker}"),
        "-f",
        "null",
        "-",
    ]
    .map(OsString::from)
    .to_vec()
}

/// Whether a live process has `marker` in its command line. A zombie has an empty command
/// line, so a killed but unreaped child does not count.
#[cfg(target_os = "linux")]
fn process_with_marker_exists(marker: &str) -> bool {
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        // A process may exit between the listing and the read.
        .filter_map(|entry| fs::read(entry.path().join("cmdline")).ok())
        .any(|cmdline| String::from_utf8_lossy(&cmdline).contains(marker))
}

/// How long a process may take to appear after spawning, or to go after being killed.
#[cfg(target_os = "linux")]
const MARKER_DEADLINE: Duration = Duration::from_secs(5);

/// Polls until `process_with_marker_exists(marker) == expected`, failing after
/// `MARKER_DEADLINE`.
#[cfg(target_os = "linux")]
async fn wait_for_marker(marker: &str, expected: bool) {
    use std::time::Instant;

    let deadline = Instant::now() + MARKER_DEADLINE;
    while process_with_marker_exists(marker) != expected {
        assert!(
            Instant::now() < deadline,
            "process with marker {marker} {} within {MARKER_DEADLINE:?}",
            if expected {
                "never appeared"
            } else {
                "survived"
            }
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The timeout branch of `SubprocessRunner`: a tool still running when it elapses is killed and
/// reported as `TimedOut`.
#[tokio::test]
async fn test_real_subprocess_runner_times_out_and_kills_the_tool() {
    if !media_tools_available() {
        return;
    }
    let marker = format!("timeout-test-{}", std::process::id());
    let args = long_running_ffmpeg_args(&marker);

    let task = tokio::spawn(async move {
        SubprocessRunner
            .run(Invocation {
                tool: MediaTool::Ffmpeg,
                args: &args,
                timeout: Duration::from_secs(2),
                on_stdout_line: None,
            })
            .await
    });
    // The tool must be seen running, so its absence below proves the timeout killed it.
    #[cfg(target_os = "linux")]
    wait_for_marker(&marker, true).await;

    assert_eq!(task.await.unwrap(), RawOutcome::TimedOut);
    #[cfg(target_os = "linux")]
    wait_for_marker(&marker, false).await;
}

/// ADR-0006: dropping the runner future, as a stop does, kills the tool.
#[tokio::test]
async fn test_real_subprocess_runner_kills_the_tool_when_the_future_is_dropped() {
    if !media_tools_available() {
        return;
    }
    let marker = format!("drop-test-{}", std::process::id());
    let args = long_running_ffmpeg_args(&marker);

    let task = tokio::spawn(async move {
        SubprocessRunner
            .run(Invocation {
                tool: MediaTool::Ffmpeg,
                args: &args,
                timeout: Duration::from_secs(60),
                on_stdout_line: None,
            })
            .await
    });
    // Without /proc the drop below still runs, but nothing observes the process, so this
    // test only checks that dropping the future does not hang or panic there.
    #[cfg(target_os = "linux")]
    wait_for_marker(&marker, true).await;

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    #[cfg(target_os = "linux")]
    wait_for_marker(&marker, false).await;
}
