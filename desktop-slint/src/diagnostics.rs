//! Engine Diagnostics for the Slint host: `tracing` output from `mkv-core` and this crate goes to
//! `mkv-anime-batch.log` in the app log directory the Tauri host uses and to stderr (ADR-0017
//! decision 9). A port of `desktop-tauri`'s `diagnostics.rs`, which leaves with Tauri.

use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing::Dispatch;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{fmt, Layer};

const LOG_FILE_NAME: &str = "mkv-anime-batch.log";

/// The app's identifier, which names its directory under the local data directory.
const IDENTIFIER: &str = "com.mkvanimebatch.desktop";

/// A log file larger than this is emptied at startup; there is no rotation.
const MAX_LOG_BYTES: u64 = 1_048_576;

/// Installs the app's subscriber as the global default. A subscriber that is already set is left
/// alone. Never fails: with no usable log directory the output is stderr only.
pub fn init(log_dir: Option<&Path>) {
    let _ = tracing::dispatcher::set_global_default(build_dispatch(
        log_dir,
        env_filter(),
        io::stderr,
        io::stderr().is_terminal(),
    ));
}

/// The log directory, as Tauri's `app_log_dir` computes it: `<local data>/<identifier>/logs`
/// (`$XDG_DATA_HOME` on Linux, `%LOCALAPPDATA%` on Windows). `None` when the OS names none.
pub fn log_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(IDENTIFIER).join("logs"))
}

/// `RUST_LOG`, defaulting to `warn`.
fn env_filter() -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .from_env_lossy()
}

/// Opens the log file for appending, first emptying it when it has outgrown `MAX_LOG_BYTES`.
fn open_log_file(dir: &Path) -> io::Result<File> {
    fs::create_dir_all(dir)?;
    let path = dir.join(LOG_FILE_NAME);
    // Two opens on purpose, so truncation is a separate `File::create`: `append` cannot be
    // combined with `truncate`, and on Windows an append handle lacks the write-data right that
    // `set_len` would need.
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
        File::create(&path)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// The file layer for `dir`: UTC timestamp, level and target, no ANSI. Fails when the directory or
/// file cannot be created or opened.
fn file_layer<S>(dir: &Path) -> io::Result<Box<dyn Layer<S> + Send + Sync>>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let file = open_log_file(dir)?;
    Ok(fmt::layer()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .boxed())
}

/// Builds the two-layer subscriber without installing it globally, so tests can scope it with
/// `tracing::dispatcher::with_default`.
///
/// - File layer: see [`file_layer`]. Skipped, with a warning, when the file cannot be opened.
/// - Stderr layer: level and message only, written to `stderr` and colored only when `ansi`.
fn build_dispatch<W>(log_dir: Option<&Path>, filter: EnvFilter, stderr: W, ansi: bool) -> Dispatch
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let (file_layer, file_error) = match log_dir.map(|dir| (dir, file_layer(dir))) {
        Some((_, Ok(layer))) => (Some(layer), None),
        Some((dir, Err(error))) => (None, Some((dir, error))),
        None => (None, None),
    };
    let stderr_layer = fmt::layer()
        .with_writer(stderr)
        .with_ansi(ansi)
        .without_time()
        .with_target(false);

    let dispatch = Dispatch::new(
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .with(stderr_layer),
    );
    if let Some((dir, error)) = file_error {
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::warn!(
                "Cannot open the log file in {}: {error}; logging to stderr only",
                dir.display()
            );
        });
    }
    dispatch
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A stderr stand-in that keeps what is written to it.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Captured;

        fn make_writer(&'a self) -> Captured {
            self.clone()
        }
    }

    /// Logs one `warn!` through a fresh subscriber for `dir`; returns what reached stderr.
    fn log_in(dir: Option<&Path>, line: &str) -> String {
        let stderr = Captured::default();
        let dispatch = build_dispatch(dir, EnvFilter::new("warn"), stderr.clone(), false);
        tracing::dispatcher::with_default(&dispatch, || tracing::warn!("{line}"));
        stderr.text()
    }

    #[test]
    fn test_the_log_dir_is_the_tauri_hosts() {
        let dir = log_dir().expect("a local data directory");
        assert_eq!(
            dir,
            dirs::data_local_dir()
                .unwrap()
                .join("com.mkvanimebatch.desktop")
                .join("logs")
        );
    }

    #[test]
    fn test_warn_lands_in_the_file_with_timestamp_level_and_target() {
        let dir = tempfile::tempdir().unwrap();
        log_in(Some(dir.path()), "font memo unreadable");

        let log = fs::read_to_string(dir.path().join(LOG_FILE_NAME)).unwrap();
        let line = log.lines().next().expect("one line");
        let (timestamp, rest) = line.split_once(' ').unwrap();
        // UTC RFC 3339, e.g. 2026-10-09T08:12:03.123456Z.
        let bytes = timestamp.as_bytes();
        assert!(
            timestamp.ends_with('Z')
                && bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes[10] == b'T'
                && bytes[13] == b':',
            "{line}"
        );
        assert!(!log.contains('\u{1b}'), "no ANSI in the file: {log:?}");
        assert!(
            rest.trim_start().starts_with("WARN ")
                && rest.contains("diagnostics::tests: font memo unreadable"),
            "{line}"
        );
    }

    #[test]
    fn test_stderr_gets_the_short_format_alongside_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let stderr = log_in(Some(dir.path()), "both sinks");

        // Level and message only: no timestamp, no module path.
        assert_eq!(stderr.trim_start(), "WARN both sinks\n");
    }

    #[test]
    fn test_filter_hides_events_below_it() {
        let dir = tempfile::tempdir().unwrap();
        let stderr = Captured::default();
        let dispatch = build_dispatch(
            Some(dir.path()),
            EnvFilter::new("warn"),
            stderr.clone(),
            false,
        );
        tracing::dispatcher::with_default(&dispatch, || tracing::info!("chatter"));

        let log = fs::read_to_string(dir.path().join(LOG_FILE_NAME)).unwrap();
        assert!(log.is_empty(), "{log:?}");
        assert!(stderr.text().is_empty());
    }

    #[test]
    fn test_a_log_file_over_the_limit_is_truncated_at_setup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE_NAME);
        fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize + 1]).unwrap();

        let _dispatch = build_dispatch(
            Some(dir.path()),
            EnvFilter::new("warn"),
            Captured::default(),
            false,
        );

        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn test_a_log_file_within_the_limit_is_appended_to() {
        // A small file, and one exactly at the limit (only a larger one is emptied).
        for size in [1024, MAX_LOG_BYTES as usize] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(LOG_FILE_NAME);
            fs::write(&path, vec![b'x'; size]).unwrap();

            log_in(Some(dir.path()), "after restart");

            let log = fs::read(&path).unwrap();
            assert!(log.len() > size, "size {size}");
            assert!(log.starts_with(&vec![b'x'; size]), "size {size}");
            assert!(String::from_utf8_lossy(&log).contains("after restart"));
        }
    }

    #[test]
    fn test_an_unusable_log_dir_falls_back_to_stderr_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        fs::write(&file, b"regular file").unwrap();
        let bad_dir = file.join("logs");

        let stderr = log_in(Some(&bad_dir), "still logged");

        assert!(stderr.contains("Cannot open the log file"), "{stderr}");
        assert!(stderr.contains("logging to stderr only"), "{stderr}");
        assert!(stderr.contains("still logged"), "{stderr}");
        // No directory at all behaves the same, minus the warning.
        let stderr = log_in(None, "still logged");
        assert!(!stderr.contains("Cannot open"), "{stderr}");
        assert!(stderr.contains("still logged"), "{stderr}");
    }
}
