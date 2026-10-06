//! The Media Tool exit-code policy and error wording, driven through `media_tool::run` with a
//! scripted runner so no tool is spawned.

mod common;

use std::time::Duration;

use common::{ScriptedRunner, Step};
use mkv_core::media_tool::{self, MediaTool, RawOutcome, ToolOutput};
use parking_lot::Mutex;

const MKVTOOLNIX: [MediaTool; 2] = [MediaTool::Mkvmerge, MediaTool::Mkvextract];
const ALL_TOOLS: [MediaTool; 3] = [
    MediaTool::Mkvmerge,
    MediaTool::Mkvextract,
    MediaTool::Ffmpeg,
];
const NO_ARGS: &[String] = &[];
const TIMEOUT: Duration = Duration::from_secs(30);

async fn run_step(tool: MediaTool, step: Step) -> Result<ToolOutput, String> {
    let runner = ScriptedRunner::new([step]);
    media_tool::run(&runner, tool, NO_ARGS, TIMEOUT, None).await
}

fn signalled(tool: MediaTool, stdout: &str, stderr: &str) -> Step {
    Step::new(
        tool,
        RawOutcome::Exited {
            code: None,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        },
    )
}

#[tokio::test]
async fn exit_0_succeeds_without_a_warning_and_returns_stdout() {
    for tool in ALL_TOOLS {
        let output = run_step(tool, Step::exited(tool, 0, "{\"ok\":true}", "noise"))
            .await
            .unwrap();
        assert_eq!(output.stdout, b"{\"ok\":true}", "{tool:?}");
        assert_eq!(output.warning, None, "{tool:?}");
        assert_eq!(output.warning_log_line(), None, "{tool:?}");
    }
}

#[tokio::test]
async fn mkvtoolnix_exit_1_succeeds_with_the_combined_output_as_warning() {
    for tool in MKVTOOLNIX {
        let output = run_step(tool, Step::exited(tool, 1, " out line \n", "\n err line "))
            .await
            .unwrap();
        assert_eq!(output.warning.as_deref(), Some("err line\nout line"));
        assert_eq!(output.stdout, b" out line \n");
    }
}

#[tokio::test]
async fn mkvtoolnix_exit_1_without_output_has_no_warning() {
    for tool in MKVTOOLNIX {
        let output = run_step(tool, Step::exited(tool, 1, "  \n", ""))
            .await
            .unwrap();
        assert_eq!(output.warning, None, "{tool:?}");
    }
}

#[tokio::test]
async fn warning_log_line_prefixes_unless_the_text_already_says_warning() {
    let plain = run_step(
        MediaTool::Mkvmerge,
        Step::exited(MediaTool::Mkvmerge, 1, "odd stream\n", ""),
    )
    .await
    .unwrap();
    assert_eq!(
        plain.warning_log_line().as_deref(),
        Some("  Warning: odd stream")
    );

    for text in ["Warning: odd stream", "WARNING: odd stream"] {
        let output = run_step(
            MediaTool::Mkvextract,
            Step::exited(MediaTool::Mkvextract, 1, text, ""),
        )
        .await
        .unwrap();
        assert_eq!(
            output.warning_log_line(),
            Some(format!("  {text}")),
            "text that starts with `warning` is logged as is"
        );
    }
}

#[tokio::test]
async fn ffmpeg_exit_1_is_a_failure() {
    let err = run_step(
        MediaTool::Ffmpeg,
        Step::exited(MediaTool::Ffmpeg, 1, "", "Invalid data\n"),
    )
    .await
    .unwrap_err();
    assert_eq!(err, "ffmpeg failed (exit code 1): Invalid data");
}

#[tokio::test]
async fn exit_2_fails_for_every_tool_with_the_combined_output() {
    for tool in ALL_TOOLS {
        let err = run_step(
            tool,
            Step::exited(tool, 2, "stdout text\n", "stderr text\n"),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            format!(
                "{} failed (exit code 2): stderr text\nstdout text",
                tool.program()
            )
        );
    }
}

#[tokio::test]
async fn a_failure_without_output_names_only_the_exit_code() {
    let err = run_step(
        MediaTool::Mkvmerge,
        Step::exited(MediaTool::Mkvmerge, 3, "", ""),
    )
    .await
    .unwrap_err();
    assert_eq!(err, "mkvmerge failed (exit code 3)");
}

#[tokio::test]
async fn a_signal_fails_for_every_tool() {
    for tool in ALL_TOOLS {
        let err = run_step(tool, signalled(tool, "", "")).await.unwrap_err();
        let program = tool.program();
        assert_eq!(err, format!("{program} failed (terminated by signal)"));

        let err = run_step(tool, signalled(tool, "", "killed\n"))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            format!("{program} failed (terminated by signal): killed")
        );
    }
}

#[tokio::test]
async fn a_timeout_reports_the_invocation_timeout_in_seconds() {
    let runner = ScriptedRunner::new([Step::new(MediaTool::Ffmpeg, RawOutcome::TimedOut)]);
    let err = media_tool::run(
        &runner,
        MediaTool::Ffmpeg,
        NO_ARGS,
        Duration::from_secs(1800),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err, "ffmpeg timed out after 1800 seconds");
}

#[tokio::test]
async fn a_failure_to_run_reports_the_io_error() {
    let err = run_step(
        MediaTool::Mkvextract,
        Step::new(
            MediaTool::Mkvextract,
            RawOutcome::FailedToRun("No such file or directory".into()),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(err, "Failed to run mkvextract: No such file or directory");
}

#[tokio::test]
async fn the_runner_receives_the_tool_and_arguments() {
    let runner = ScriptedRunner::new([Step::exited(MediaTool::Mkvmerge, 0, "", "")]);
    let args = vec!["-J".to_string(), "/anime/Show - 01.mkv".to_string()];
    media_tool::run(&runner, MediaTool::Mkvmerge, &args, TIMEOUT, None)
        .await
        .unwrap();
    assert_eq!(runner.calls(), [(MediaTool::Mkvmerge, args)]);
}

#[tokio::test]
async fn progress_lines_reach_on_progress_as_percentages() {
    let runner = ScriptedRunner::new([Step::exited(MediaTool::Mkvextract, 0, "", "")
        .stdout_lines(&[
            "Progress: 10%",
            "mkvextract v85 starting",
            "  Progress: 55.5% ",
            "Progress: nonsense",
            "Progress: 100%",
        ])]);
    let seen = Mutex::new(Vec::new());
    let on_progress = |pct: f64| seen.lock().push(pct);

    media_tool::run(
        &runner,
        MediaTool::Mkvextract,
        NO_ARGS,
        TIMEOUT,
        Some(&on_progress),
    )
    .await
    .unwrap();
    assert_eq!(*seen.lock(), [10.0, 55.5, 100.0]);
}

#[tokio::test]
async fn progress_lines_without_a_callback_are_ignored() {
    let runner = ScriptedRunner::new([
        Step::exited(MediaTool::Mkvmerge, 0, "", "").stdout_lines(&["Progress: 10%"])
    ]);
    media_tool::run(&runner, MediaTool::Mkvmerge, NO_ARGS, TIMEOUT, None)
        .await
        .unwrap();
}

#[tokio::test]
#[should_panic(expected = "mkvmerge invocation answered by a step scripted for ffmpeg")]
async fn a_step_scripted_for_another_tool_panics() {
    let _ = run_step(
        MediaTool::Mkvmerge,
        Step::exited(MediaTool::Ffmpeg, 0, "", ""),
    )
    .await;
}
