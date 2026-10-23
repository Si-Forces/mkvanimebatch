//! The renderer choice and the GPU fallback (ADR-0017 decision 3, 2026-10-23 renderer fallback
//! amendment). Skia is pinned to OpenGL, so an OpenGL failure leaves the window without a frame
//! (`main.rs` watches for it) instead of falling silently down Skia's own chain (wgpu, then Skia
//! software). The host then asks to restart in software mode; on yes it remembers the choice and
//! re-launches itself with Slint's software renderer, since the platform is selected once per
//! process.
//!
//! `MKV_RENDERER=software` forces the software renderer. `MKV_RENDERER=opengl` forces an OpenGL
//! attempt over a saved software choice and, once a frame is drawn, forgets that choice: that is
//! how a user goes back to the GPU.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Forces a renderer: `software` or `opengl`.
pub const RENDERER_ENV: &str = "MKV_RENDERER";
/// Set on a re-launched child: a failure there exits instead of re-launching again.
pub const RELAUNCHED_ENV: &str = "MKV_RENDERER_RELAUNCHED";
/// The saved choice, under `<config dir>/com.mkvanimebatch.desktop/`; it holds `software`.
const CHOICE_FILE: &str = "renderer";

pub const DIALOG_TITLE: &str = "MKV Anime Batch";
pub const DIALOG_TEXT: &str = "The GPU renderer could not start. Restart in software mode?\n\n\
    MKV Anime Batch will remember this. To try the GPU again, start it with MKV_RENDERER=opengl.";

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
pub enum Start {
    Pending,
    /// A frame was drawn: OpenGL works.
    Drew,
    /// No window by the deadline: the surface failed and Slint idles with the error.
    NoWindow,
}

/// Whether OpenGL failed at startup: an OpenGL run that drew no frame and either ended with an
/// error or never got its window. An error after the first frame is an ordinary one.
pub fn gpu_failed_at_start(renderer: Renderer, start: Start, run_failed: bool) -> bool {
    renderer == Renderer::OpenGl
        && match start {
            Start::Pending => run_failed,
            Start::Drew => false,
            Start::NoWindow => true,
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
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(CHOICE_FILE), "software\n")
}

pub fn forget_saved(dir: &Path) -> io::Result<()> {
    match std::fs::remove_file(dir.join(CHOICE_FILE)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The re-launch: the same program and arguments, the inherited environment plus the software
/// renderer and the guard. Under an AppImage the program is `$APPIMAGE` (the running binary sits
/// in a mount that ends with this process) and `argv[0]` stays `$ARGV0`, which the AppImage
/// runtime sets from it.
pub fn relaunch_command(
    exe: &Path,
    appimage: Option<OsString>,
    argv0: Option<OsString>,
    args: impl IntoIterator<Item = OsString>,
) -> Command {
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
        .env(RELAUNCHED_ENV, "1");
    command
}

#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// No dialog could be shown; the reason, for the log.
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
        match Command::new(tool).args(args).status() {
            // Both answer 0 for yes and 1 for no or a closed dialog.
            Ok(status) if status.code() == Some(0) => return Answer::Yes,
            Ok(status) if status.code() == Some(1) => return Answer::No,
            Ok(status) => reasons.push(format!("{tool} ended with {status}")),
            Err(error) => reasons.push(format!("{tool}: {error}")),
        }
    }
    Answer::Unavailable(reasons.join("; "))
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
        assert!(gpu_failed_at_start(OpenGl, Start::Pending, true));
        assert!(gpu_failed_at_start(OpenGl, Start::NoWindow, false));
        assert!(gpu_failed_at_start(OpenGl, Start::NoWindow, true));
        assert!(!gpu_failed_at_start(OpenGl, Start::Pending, false));
        assert!(!gpu_failed_at_start(OpenGl, Start::Drew, true));
        assert!(!gpu_failed_at_start(Software, Start::Pending, true));
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
        let command = relaunch_command(Path::new("/opt/app/bin"), None, None, args());
        assert_eq!(command.get_program(), "/opt/app/bin");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["--flag", "a b"]);
        let mut envs = envs(&command);
        envs.sort();
        assert_eq!(
            envs,
            [
                (RENDERER_ENV.into(), "software".into()),
                (RELAUNCHED_ENV.into(), "1".into())
            ]
        );

        let command = relaunch_command(
            Path::new("/tmp/.mount_x/usr/bin/app"),
            Some("/home/u/App.AppImage".into()),
            Some("App.AppImage".into()),
            args(),
        );
        assert_eq!(command.get_program(), "/home/u/App.AppImage");
        assert_eq!(command.get_args().count(), 2);
    }
}
