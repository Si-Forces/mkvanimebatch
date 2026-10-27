//! The renderer choice and the GPU fallback (ADR-0017 decision 3, 2026-10-23 renderer fallback
//! amendment). Skia is pinned to OpenGL, so an OpenGL failure leaves the window without a frame
//! (`main.rs` watches for it) instead of falling silently down Skia's own chain (wgpu, then Skia
//! software). The host then asks to restart in software mode; on yes it re-launches itself with
//! Slint's software renderer, since the platform is selected once per process, and the child
//! remembers the choice once it has drawn.
//!
//! `MKV_RENDERER=software` forces the software renderer. `MKV_RENDERER=opengl` forces an OpenGL
//! attempt over a saved software choice and, once a frame is drawn, forgets that choice: that is
//! how a user goes back to the GPU.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "linux")]
use std::process::{Child, ExitStatus, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

/// Forces a renderer: `software` or `opengl`.
pub const RENDERER_ENV: &str = "MKV_RENDERER";
/// Set on a re-launched child: a failure there exits instead of re-launching again.
pub const RELAUNCHED_ENV: &str = "MKV_RENDERER_RELAUNCHED";
/// Set on a re-launched child: save the software choice once a frame is drawn, so a restart that
/// fails too leaves nothing saved.
pub const SAVE_ENV: &str = "MKV_RENDERER_SAVE";
/// The saved choice, under `<config dir>/com.mkvanimebatch.desktop/`; it holds `software`.
const CHOICE_FILE: &str = "renderer";

pub const DIALOG_TITLE: &str = "MKV Anime Batch";
pub const DIALOG_TEXT: &str = "The GPU renderer could not start. Restart in software mode?\n\n\
    If the restart works, MKV Anime Batch will remember this. To try the GPU again, start it with MKV_RENDERER=opengl.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    OpenGl,
    Software,
}

impl Renderer {
    /// The value of `MKV_RENDERER`; `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "opengl" => Some(Self::OpenGl),
            "software" => Some(Self::Software),
            _ => None,
        }
    }

    /// Slint's winit renderer name: `skia-opengl` has no fallback of its own, `software` is
    /// Slint's software renderer (not Skia's, which the spike saw draw no frame).
    pub fn slint_name(self) -> &'static str {
        match self {
            Self::OpenGl => "skia-opengl",
            Self::Software => "software",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Choice {
    pub renderer: Renderer,
    /// A forced OpenGL start over a saved software choice: forget it once a frame is drawn.
    pub forget_saved: bool,
}

/// The renderer for this start: a forced one first, then the saved choice, then OpenGL.
pub fn choose(forced: Option<Renderer>, saved_software: bool) -> Choice {
    let renderer = forced.unwrap_or(if saved_software {
        Renderer::Software
    } else {
        Renderer::OpenGl
    });
    Choice {
        renderer,
        forget_saved: forced == Some(Renderer::OpenGl) && saved_software,
    }
}

/// How far an OpenGL start got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuStart {
    /// Still setting up before the event loop: an error here is not the GPU's.
    Setup,
    /// The first frame cannot be watched (the rendering notifier was refused): no error is taken
    /// for the GPU's, so the fallback degrades to none instead of to "every error is the GPU's".
    Unwatched,
    /// In the event loop, no frame yet.
    Pending,
    /// A frame was drawn: OpenGL works.
    Drew,
    /// No window by the deadline: the surface failed and Slint idles with the error.
    NoWindow,
}

/// Whether OpenGL failed at startup: an OpenGL event loop that drew no frame and either ended with
/// an error or never got its window. An error before the loop or after the first frame is an
/// ordinary one.
pub fn gpu_failed_at_start(renderer: Renderer, start: GpuStart, run_failed: bool) -> bool {
    renderer == Renderer::OpenGl
        && match start {
            GpuStart::Setup | GpuStart::Unwatched | GpuStart::Drew => false,
            GpuStart::Pending => run_failed,
            GpuStart::NoWindow => true,
        }
}

/// Whether a start that failed before its first frame may offer the software restart: only an
/// OpenGL start that is not itself a re-launch. Anything else exits with the error, so a failing
/// child never re-launches again.
pub fn may_offer_restart(renderer: Renderer, relaunched: bool) -> bool {
    renderer == Renderer::OpenGl && !relaunched
}

/// `<config dir>/com.mkvanimebatch.desktop`; `None` when the OS names no config directory.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join(crate::diagnostics::IDENTIFIER))
}

/// Whether `dir` holds a saved software choice. An unreadable file counts as none.
pub fn saved_software(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(CHOICE_FILE)).is_ok_and(|text| text.trim() == "software")
}

pub fn save_software(dir: &Path) -> io::Result<()> {
    save_file(dir, CHOICE_FILE, "software\n")
}

/// Writes `text` to `dir/name` through a temporary file renamed over it, so the app crashing or
/// closing mid-write leaves the old file whole, never an empty or partial one (nothing is synced
/// to disk, so a power loss may not). The temporary file is this process's
/// own, so two instances saving at once never write into each other's; the last rename wins.
pub fn save_file(dir: &Path, name: &str, text: &str) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let temporary = dir.join(format!("{name}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, text)?;
    std::fs::rename(temporary, dir.join(name))
}

pub fn forget_saved(dir: &Path) -> io::Result<()> {
    match std::fs::remove_file(dir.join(CHOICE_FILE)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The re-launch: the same program and arguments, the inherited environment plus the software
/// renderer, the guard and the save request. Under an AppImage the program is `$APPIMAGE` (the
/// running binary sits in a mount that ends with this process) and `argv[0]` stays `$ARGV0`,
/// which the AppImage runtime sets from it. `$APPIMAGE` counts only when `exe` is under
/// `$APPDIR`: one inherited from another AppImage (a terminal, an IDE) names that one instead.
pub fn relaunch_command(
    exe: &Path,
    appimage: Option<OsString>,
    appdir: Option<OsString>,
    argv0: Option<OsString>,
    args: impl IntoIterator<Item = OsString>,
) -> Command {
    // `exe` is canonical (/proc/self/exe); `$APPDIR` may not be, and an empty one is under every
    // path.
    let ours = appdir.filter(|dir| !dir.is_empty()).is_some_and(|dir| {
        exe.starts_with(std::fs::canonicalize(&dir).unwrap_or_else(|_| PathBuf::from(dir)))
    });
    let appimage = appimage.filter(|_| ours);
    let mut command = Command::new(appimage.as_deref().unwrap_or(exe.as_os_str()));
    #[cfg(unix)]
    if let (Some(_), Some(argv0)) = (&appimage, argv0) {
        std::os::unix::process::CommandExt::arg0(&mut command, argv0);
    }
    #[cfg(not(unix))]
    let _ = argv0;
    command
        .args(args)
        .env(RENDERER_ENV, "software")
        .env(RELAUNCHED_ENV, "1")
        .env(SAVE_ENV, "1");
    command
}

#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// No dialog could be shown; the reason, for the log.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "rfd's MessageDialog always answers; only the Linux tools can fail"
        )
    )]
    Unavailable(String),
}

/// Asks [`DIALOG_TEXT`], blocking. rfd's message dialog on Linux only runs `zenity` and reports a
/// missing `zenity` as Cancel, which reads as "no"; KDE often has `kdialog` instead. So on Linux
/// the host runs them itself, `zenity` first, and knows when neither could be shown.
#[cfg(target_os = "linux")]
pub fn ask_restart() -> Answer {
    let tools: [(&str, &[&str]); 2] = [
        (
            "zenity",
            &["--question", "--title", DIALOG_TITLE, "--text", DIALOG_TEXT],
        ),
        (
            "kdialog",
            &["--title", DIALOG_TITLE, "--yesno", DIALOG_TEXT],
        ),
    ];
    let mut reasons = Vec::new();
    for (tool, args) in tools {
        // The C locale keeps stderr untranslated for read_answer; the dialog text stays English.
        // gettext still reads LANGUAGE unless the locale is exactly "C", so drop it too.
        let child = Command::new(tool)
            .args(args)
            .env("LC_ALL", "C.UTF-8")
            .env_remove("LANGUAGE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let (status, stderr) = match child.and_then(|child| wait_for_dialog(child, DIALOG_TIMEOUT))
        {
            Ok(Some(ended)) => ended,
            Ok(None) => return Answer::Unavailable(format!("{tool} timed out after 5 minutes")),
            Err(error) => {
                reasons.push(format!("{tool}: {error}"));
                continue;
            }
        };
        match read_answer(status.code(), &stderr) {
            Some(answer) => return answer,
            None => reasons.push(format!(
                "{tool} ended with {status}: {}",
                stderr.trim().lines().last().unwrap_or_default()
            )),
        }
    }
    Answer::Unavailable(reasons.join("; "))
}

/// How long the dialog may stay unanswered: a hung zenity or kdialog must not hold a process that
/// has no window to close.
#[cfg(target_os = "linux")]
const DIALOG_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Waits for the dialog, polling every 100 ms; past `timeout` it kills it and returns `None`.
/// stderr is read after the exit.
// ponytail: a tool writing over a pipe buffer (64 KiB) of stderr would block until the timeout;
// read stderr on a thread if one ever does.
#[cfg(target_os = "linux")]
fn wait_for_dialog(
    mut child: Child,
    timeout: Duration,
) -> io::Result<Option<(ExitStatus, String)>> {
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stderr.take() {
        io::Read::read_to_end(&mut pipe, &mut stderr)?;
    }
    Ok(Some((
        status,
        String::from_utf8_lossy(&stderr).into_owned(),
    )))
}

/// zenity and kdialog answer 0 for yes and 1 for no or a closed dialog, but both can also end
/// with 1 when they cannot reach the display, which they say on stderr: GTK's "Failed to open
/// display", Qt's "could not connect to display" or "cannot connect to X server". Any other
/// stderr is no sign of failure: Mesa warns there on a healthy start. `None`: not shown.
#[cfg(any(target_os = "linux", test))]
fn read_answer(code: Option<i32>, stderr: &str) -> Option<Answer> {
    let stderr = stderr.to_ascii_lowercase();
    let no_display = ["open display", "could not connect", "cannot connect"]
        .iter()
        .any(|sign| stderr.contains(sign));
    match code {
        Some(0) => Some(Answer::Yes),
        Some(1) if !no_display => Some(Answer::No),
        _ => None,
    }
}

/// Asks [`DIALOG_TEXT`], blocking: on Windows rfd shows a `MessageBoxW`.
#[cfg(not(target_os = "linux"))]
pub fn ask_restart() -> Answer {
    match rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(DIALOG_TITLE)
        .set_description(DIALOG_TEXT)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
    {
        rfd::MessageDialogResult::Yes => Answer::Yes,
        _ => Answer::No,
    }
}

/// `MKV_RENDERER`'s value, or `None` when unset; an unknown value is ignored with a warning.
pub fn forced_from_env(value: Option<&OsStr>) -> Option<Renderer> {
    let value = value?;
    let renderer = value.to_str().and_then(Renderer::parse);
    if renderer.is_none() {
        tracing::warn!(
            "Ignoring {RENDERER_ENV}={}: expected software or opengl",
            value.to_string_lossy()
        );
    }
    renderer
}

#[cfg(test)]
mod tests {
    use super::*;
    use Renderer::{OpenGl, Software};

    #[test]
    fn the_choice_takes_the_forced_renderer_then_the_saved_one_then_opengl() {
        let choice = |renderer, forget_saved| Choice {
            renderer,
            forget_saved,
        };
        assert_eq!(choose(None, false), choice(OpenGl, false));
        assert_eq!(choose(None, true), choice(Software, false));
        assert_eq!(choose(Some(Software), false), choice(Software, false));
        assert_eq!(choose(Some(Software), true), choice(Software, false));
        assert_eq!(choose(Some(OpenGl), false), choice(OpenGl, false));
        // The undo: a forced OpenGL start over a saved software choice forgets it.
        assert_eq!(choose(Some(OpenGl), true), choice(OpenGl, true));
    }

    #[test]
    fn only_a_first_opengl_start_may_offer_the_restart() {
        assert!(may_offer_restart(OpenGl, false));
        // The guard: a re-launched child that fails exits instead of re-launching again.
        assert!(!may_offer_restart(OpenGl, true));
        assert!(!may_offer_restart(Software, false));
        assert!(!may_offer_restart(Software, true));
    }

    #[test]
    fn only_an_opengl_run_that_drew_nothing_is_a_gpu_failure() {
        assert!(gpu_failed_at_start(OpenGl, GpuStart::Pending, true));
        // A setup error before the event loop (runtime, component) is not the GPU's.
        assert!(!gpu_failed_at_start(OpenGl, GpuStart::Setup, true));
        // With the first frame unwatched, no error is taken for the GPU's.
        assert!(!gpu_failed_at_start(OpenGl, GpuStart::Unwatched, true));
        assert!(gpu_failed_at_start(OpenGl, GpuStart::NoWindow, false));
        assert!(gpu_failed_at_start(OpenGl, GpuStart::NoWindow, true));
        assert!(!gpu_failed_at_start(OpenGl, GpuStart::Pending, false));
        assert!(!gpu_failed_at_start(OpenGl, GpuStart::Drew, true));
        assert!(!gpu_failed_at_start(Software, GpuStart::Pending, true));
    }

    #[test]
    fn the_slint_renderer_names_are_pinned() {
        // A typo here only fails at runtime, inside BackendSelector::select().
        assert_eq!(OpenGl.slint_name(), "skia-opengl");
        assert_eq!(Software.slint_name(), "software");
    }

    #[test]
    fn the_env_value_parses_loosely_and_ignores_anything_else() {
        assert_eq!(Renderer::parse(" Software "), Some(Software));
        assert_eq!(Renderer::parse("OPENGL"), Some(OpenGl));
        assert_eq!(Renderer::parse("gpu"), None);
        assert_eq!(forced_from_env(None), None);
        assert_eq!(forced_from_env(Some(OsStr::new("vulkan"))), None);
        assert_eq!(
            forced_from_env(Some(OsStr::new("software"))),
            Some(Software)
        );
    }

    #[test]
    fn the_saved_choice_is_written_read_and_forgotten() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("com.mkvanimebatch.desktop");
        assert!(!saved_software(&dir));
        forget_saved(&dir).unwrap();
        save_software(&dir).unwrap();
        assert!(saved_software(&dir));
        forget_saved(&dir).unwrap();
        assert!(!saved_software(&dir));
        std::fs::write(dir.join(CHOICE_FILE), "opengl").unwrap();
        assert!(!saved_software(&dir));
    }

    fn envs(command: &Command) -> Vec<(String, String)> {
        command
            .get_envs()
            .map(|(key, value)| {
                let text = |s: &OsStr| s.to_string_lossy().into_owned();
                (text(key), value.map(text).unwrap_or_default())
            })
            .collect()
    }

    #[test]
    fn the_relaunch_runs_the_same_program_in_software_mode_with_the_guard() {
        let args = || ["--flag".into(), "a b".into()];
        let command = relaunch_command(Path::new("/opt/app/bin"), None, None, None, args());
        assert_eq!(command.get_program(), "/opt/app/bin");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["--flag", "a b"]);
        let mut sorted = envs(&command);
        sorted.sort();
        assert_eq!(
            sorted,
            [
                (RENDERER_ENV.into(), "software".into()),
                (RELAUNCHED_ENV.into(), "1".into()),
                (SAVE_ENV.into(), "1".into()),
            ]
        );

        let command = relaunch_command(
            Path::new("/tmp/.mount_x/usr/bin/app"),
            Some("/home/u/App.AppImage".into()),
            Some("/tmp/.mount_x".into()),
            Some("App.AppImage".into()),
            args(),
        );
        assert_eq!(command.get_program(), "/home/u/App.AppImage");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["--flag", "a b"]);
        assert_eq!(envs(&command).len(), 3);
        // std's Debug shows an argv[0] that differs from the program.
        #[cfg(unix)]
        assert!(format!("{command:?}").contains("\"App.AppImage\""));

        // `$ARGV0` without `$APPIMAGE` is not an AppImage run: ignored.
        let command = relaunch_command(
            Path::new("/opt/app/bin"),
            None,
            None,
            Some("App.AppImage".into()),
            args(),
        );
        assert_eq!(command.get_program(), "/opt/app/bin");
        assert!(!format!("{command:?}").contains("App.AppImage"));
    }

    #[test]
    fn an_appimage_inherited_from_another_app_is_not_ours() {
        let args = || ["--flag".into()];
        let foreign = |appdir: Option<&str>| {
            relaunch_command(
                Path::new("/opt/app/bin"),
                Some("/home/u/Code.AppImage".into()),
                appdir.map(Into::into),
                Some("Code.AppImage".into()),
                args(),
            )
        };
        // Launched from another AppImage's terminal: its $APPDIR does not hold our binary.
        // An empty $APPDIR would be under every path.
        for command in [
            foreign(Some("/tmp/.mount_code")),
            foreign(None),
            foreign(Some("")),
        ] {
            assert_eq!(command.get_program(), "/opt/app/bin");
            assert!(!format!("{command:?}").contains("Code.AppImage"));
        }
    }

    /// A dialog tool that never answers is killed at the deadline and reaped.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_dialog_that_never_answers_times_out() {
        let child = Command::new("sleep")
            .arg("30")
            .stderr(Stdio::piped())
            .spawn()
            .expect("sleep runs");
        let pid = child.id();
        let started = Instant::now();
        let waited = wait_for_dialog(child, Duration::from_millis(200)).unwrap();
        assert!(waited.is_none(), "timed out");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "killed and reaped"
        );
    }

    #[test]
    fn a_dialog_tool_that_could_not_open_the_display_did_not_answer() {
        assert_eq!(read_answer(Some(0), ""), Some(Answer::Yes));
        // A healthy "no" can still carry Mesa's warnings on stderr.
        let mesa = "MESA-INTEL: warning: Ivy Bridge Vulkan support is incomplete\n";
        assert_eq!(read_answer(Some(1), mesa), Some(Answer::No));
        let gtk = "(zenity:1): Gtk-WARNING **: 13:35:51.009: Failed to open display\n";
        assert_eq!(read_answer(Some(1), gtk), None);
        // Qt says it its own way, and the match ignores case.
        let qt = "qt.qpa.xcb: could not connect to display :0\n";
        assert_eq!(read_answer(Some(1), qt), None);
        let x11 = "kdialog: Cannot connect to X server :0\n";
        assert_eq!(read_answer(Some(1), x11), None);
        // kdialog aborts without a display.
        assert_eq!(read_answer(None, ""), None);
        assert_eq!(read_answer(Some(255), ""), None);
    }
}
