//! Media Tool invocation: one runner port that spawns, and one exit-code policy above it.
//!
//! [`MediaToolRunner`] reports what a tool did and nothing more. [`run`] classifies that into
//! success, success with warnings, or a failure, so every Media Tool shares one set of exit
//! rules and one error wording. This is not the runner loop ADR-0002 rejected: that was a
//! `BatchRunner` over the remux and extract pipelines, this is one tool invocation.

use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

/// Windows process creation flag that keeps child tools from opening a console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// An external program the engine runs as a child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaTool {
    Mkvmerge,
    Mkvextract,
    Ffmpeg,
}

impl MediaTool {
    /// The executable name looked up on `PATH`.
    pub fn program(self) -> &'static str {
        match self {
            Self::Mkvmerge => "mkvmerge",
            Self::Mkvextract => "mkvextract",
            Self::Ffmpeg => "ffmpeg",
        }
    }

    /// Whether the tool belongs to MKVToolNix, whose exit code 1 means "finished with warnings".
    fn is_mkvtoolnix(self) -> bool {
        matches!(self, Self::Mkvmerge | Self::Mkvextract)
    }
}

/// One run of a Media Tool.
pub struct Invocation<'a> {
    pub tool: MediaTool,
    pub args: &'a [OsString],
    pub timeout: Duration,
    /// Receives the stdout lines as the tool prints them, split on `\r` and `\n` (so the gap
    /// of a `\r\n` is an empty line), then the unterminated remainder once stdout ends, which
    /// may be empty.
    pub on_stdout_line: Option<&'a (dyn Fn(&str) + Send + Sync)>,
}

/// What a runner observed, before any exit-code policy is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawOutcome {
    /// The tool ran to the end. `code` is `None` when a signal terminated it.
    Exited {
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    /// The tool could not be spawned or waited on; carries the I/O error text.
    FailedToRun(String),
    /// The tool was still running when the timeout elapsed, and was killed.
    TimedOut,
}

/// The port that runs one Media Tool. Dropping the returned future must kill the child, so
/// cancelling the work that awaits it stops the tool (ADR-0006).
pub trait MediaToolRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        invocation: Invocation<'a>,
    ) -> Pin<Box<dyn Future<Output = RawOutcome> + Send + 'a>>;
}

/// The production runner: spawns the tool as a child process.
#[derive(Debug)]
pub struct SubprocessRunner;

impl MediaToolRunner for SubprocessRunner {
    fn run<'a>(
        &'a self,
        invocation: Invocation<'a>,
    ) -> Pin<Box<dyn Future<Output = RawOutcome> + Send + 'a>> {
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new(invocation.tool.program());
            #[cfg(windows)]
            cmd.creation_flags(CREATE_NO_WINDOW);
            cmd.kill_on_drop(true)
                .args(invocation.args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            let mut child = match cmd.spawn() {
                Ok(child) => child,
                Err(e) => return RawOutcome::FailedToRun(e.to_string()),
            };
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            // Drained concurrently, so a tool filling its stderr pipe cannot stall on it.
            let stderr_task = tokio::spawn(async move {
                let mut bytes = Vec::new();
                if let Some(mut stream) = stderr {
                    let _ = stream.read_to_end(&mut bytes).await;
                }
                bytes
            });
            let stderr_abort = stderr_task.abort_handle();

            let execution = async {
                let mut stdout_bytes = Vec::new();
                if let Some(stream) = stdout {
                    drain_stdout(stream, invocation.on_stdout_line, &mut stdout_bytes).await;
                }
                match child.wait().await {
                    Ok(status) => RawOutcome::Exited {
                        code: status.code(),
                        stdout: stdout_bytes,
                        stderr: stderr_task.await.unwrap_or_default(),
                    },
                    Err(e) => RawOutcome::FailedToRun(e.to_string()),
                }
            };

            // `child` outlives the timeout, so it is killed on drop whether the timeout
            // fires or this future is dropped first.
            match tokio::time::timeout(invocation.timeout, execution).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    stderr_abort.abort();
                    RawOutcome::TimedOut
                }
            }
        })
    }
}

/// Collects `stream` into `bytes`, handing each line to `on_line` as [`Invocation::on_stdout_line`]
/// describes.
async fn drain_stdout<R: AsyncRead + Unpin>(
    mut stream: R,
    on_line: Option<&(dyn Fn(&str) + Send + Sync)>,
    bytes: &mut Vec<u8>,
) {
    let mut buf = [0u8; 1024];
    // Raw bytes, decoded per line: a multi-byte character split across two reads stays whole.
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        bytes.extend_from_slice(&buf[..n]);
        if let Some(on_line) = on_line {
            pending.extend_from_slice(&buf[..n]);
            while let Some(pos) = pending.iter().position(|&b| b == b'\r' || b == b'\n') {
                on_line(&String::from_utf8_lossy(&pending[..pos]));
                pending.drain(..=pos);
            }
        }
    }
    if let Some(on_line) = on_line {
        on_line(&String::from_utf8_lossy(&pending));
    }
}

/// A Media Tool run that succeeded, possibly with warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub stdout: Vec<u8>,
    /// The trimmed stderr and stdout of a run an MKVToolNix tool ended with exit code 1.
    pub warning: Option<String>,
}

impl ToolOutput {
    /// The log line for the warning, if any (see [`warning_log_line`]).
    pub fn warning_log_line(&self) -> Option<String> {
        self.warning.as_deref().map(warning_log_line)
    }
}

/// The Log Channel line for a warning: `  <text>` when `text` already starts with "warning"
/// (any case), else `  Warning: <text>`.
pub fn warning_log_line(text: &str) -> String {
    if text.to_lowercase().starts_with("warning") {
        format!("  {text}")
    } else {
        format!("  Warning: {text}")
    }
}

/// Parses mkvmerge/mkvextract "Progress: XX%" output lines.
fn parse_progress_percentage(line: &str) -> Option<f64> {
    let rest = line.trim().strip_prefix("Progress:")?;
    rest.trim().trim_end_matches('%').trim().parse::<f64>().ok()
}

/// Joins a Media Tool's trimmed stderr and stdout, omitting whichever is empty.
fn combined_output(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    match (stderr.trim(), stdout.trim()) {
        ("", out) => out.to_owned(),
        (err, "") => err.to_owned(),
        (err, out) => format!("{err}\n{out}"),
    }
}

/// Runs `tool` through `runner` and applies its exit-code policy.
///
/// MKVToolNix tools exit 0 on success and 1 on success with warnings; anything else fails.
/// `ffmpeg` exits 0 on success; anything else fails. `on_progress` receives each "Progress: N%"
/// stdout line as a percentage.
pub async fn run(
    runner: &dyn MediaToolRunner,
    tool: MediaTool,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    on_progress: Option<&(dyn Fn(f64) + Send + Sync)>,
) -> Result<ToolOutput, String> {
    // Owned so a path that is not valid UTF-8 reaches the tool as it is.
    let args: Vec<OsString> = args.iter().map(|arg| arg.as_ref().to_owned()).collect();
    let report = |line: &str| {
        if let (Some(cb), Some(pct)) = (on_progress, parse_progress_percentage(line)) {
            cb(pct);
        }
    };
    let on_stdout_line: Option<&(dyn Fn(&str) + Send + Sync)> =
        on_progress.is_some().then_some(&report);

    let program = tool.program();
    let outcome = runner
        .run(Invocation {
            tool,
            args: &args,
            timeout,
            on_stdout_line,
        })
        .await;

    match outcome {
        RawOutcome::FailedToRun(e) => Err(format!("Failed to run {program}: {e}")),
        RawOutcome::TimedOut => Err(format!(
            "{program} timed out after {} seconds",
            timeout.as_secs()
        )),
        RawOutcome::Exited {
            code: Some(0),
            stdout,
            ..
        } => Ok(ToolOutput {
            stdout,
            warning: None,
        }),
        RawOutcome::Exited {
            code: Some(1),
            stdout,
            stderr,
        } if tool.is_mkvtoolnix() => {
            let warning = combined_output(&stderr, &stdout);
            Ok(ToolOutput {
                stdout,
                warning: (!warning.is_empty()).then_some(warning),
            })
        }
        RawOutcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            let code_text = code
                .map(|c| format!("exit code {c}"))
                .unwrap_or_else(|| "terminated by signal".into());
            let output = combined_output(&stderr, &stdout);
            Err(if output.is_empty() {
                format!("{program} failed ({code_text})")
            } else {
                format!("{program} failed ({code_text}): {output}")
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::AsyncWriteExt;

    /// Drains `stream` with a sink and returns the collected bytes with every line it saw.
    async fn drain<R: AsyncRead + Unpin>(stream: R) -> (Vec<u8>, Vec<String>) {
        let lines = Mutex::new(Vec::new());
        let sink = |line: &str| lines.lock().unwrap().push(line.to_string());
        let mut bytes = Vec::new();
        drain_stdout(stream, Some(&sink), &mut bytes).await;
        (bytes, lines.into_inner().unwrap())
    }

    #[tokio::test]
    async fn splits_lines_on_cr_and_lf_and_flushes_the_unterminated_tail() {
        let (bytes, lines) = drain(&b"Progress: 1%\rProgress: 2%\nProgress: 3%"[..]).await;
        assert_eq!(bytes, b"Progress: 1%\rProgress: 2%\nProgress: 3%");
        assert_eq!(lines, ["Progress: 1%", "Progress: 2%", "Progress: 3%"]);
    }

    #[tokio::test]
    async fn a_crlf_gap_is_an_empty_line_and_a_terminated_end_has_an_empty_tail() {
        let (_, lines) = drain(&b"a\r\nb\n"[..]).await;
        assert_eq!(lines, ["a", "", "b", ""]);
    }

    #[tokio::test]
    async fn collects_bytes_without_a_sink() {
        let mut bytes = Vec::new();
        drain_stdout(&b"Progress: 1%\n\xff"[..], None, &mut bytes).await;
        assert_eq!(bytes, b"Progress: 1%\n\xff");
    }

    #[tokio::test]
    async fn a_line_and_a_multibyte_character_split_across_reads_stay_whole() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let write = async move {
            // "é" is 0xC3 0xA9: the first write ends between its two bytes.
            for chunk in [&b"Progress: 5"[..], b"0%\r caf\xC3", b"\xA9\nend"] {
                writer.write_all(chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
        };
        let (_, (bytes, lines)) = tokio::join!(write, drain(reader));
        assert_eq!(bytes, "Progress: 50%\r café\nend".as_bytes());
        assert_eq!(lines, ["Progress: 50%", " café", "end"]);
    }

    #[tokio::test]
    async fn an_invalid_utf8_line_is_decoded_lossily() {
        let (_, lines) = drain(&b"a\xffb\n"[..]).await;
        assert_eq!(lines, ["a\u{fffd}b", ""]);
    }

    #[test]
    fn parses_progress_lines_only() {
        assert_eq!(parse_progress_percentage("Progress: 42%"), Some(42.0));
        assert_eq!(parse_progress_percentage("  Progress: 7.5 % "), Some(7.5));
        assert_eq!(parse_progress_percentage("Progress: n/a"), None);
        assert_eq!(parse_progress_percentage("Warning: 42%"), None);
    }
}
