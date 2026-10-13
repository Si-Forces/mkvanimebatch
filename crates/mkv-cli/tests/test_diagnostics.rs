//! Engine Diagnostics reach stderr, filtered by `RUST_LOG`, while stdout stays the `--plan` JSON.

use std::path::Path;
use std::process::{Command, Output};

/// Runs `mkv-cli --plan` with the inspect cache redirected into `home`.
///
/// `get_default_cache_dir` honors `LOCALAPPDATA` first and `HOME` second; the cache file sits in
/// `$LOCALAPPDATA/mkvanimebatch`, so setting it points the cache at `home` on every platform.
/// `HOME` is set too so that no fallback can reach the real user cache.
fn run_plan(input: &Path, home: &Path, rust_log: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mkv-cli"));
    command
        .args(["--plan", "--input-dir"])
        .arg(input)
        .env("HOME", home)
        .env("LOCALAPPDATA", home)
        .env_remove("RUST_LOG");
    if let Some(filter) = rust_log {
        command.env("RUST_LOG", filter);
    }
    command.output().expect("mkv-cli runs")
}

/// A release directory with one junk `.mkv` (the probe may fail; `--plan` still lists it) and a
/// home directory whose inspect cache is not valid JSON.
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let input = tempfile::tempdir().unwrap();
    std::fs::write(input.path().join("ep01.mkv"), b"not a matroska file").unwrap();

    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("mkvanimebatch");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("inspect_cache.json"), b"{ corrupt").unwrap();
    (input, home)
}

#[test]
fn test_corrupt_inspect_cache_warns_on_stderr_and_keeps_stdout_json() {
    let (input, home) = fixture();
    let output = run_plan(input.path(), home.path(), None);

    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WARN"), "{stderr}");
    assert!(stderr.contains("Failed to parse inspect cache"), "{stderr}");
    let manifest: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is the manifest JSON only");
    assert_eq!(manifest["version"], 2);
}

#[test]
fn test_rust_log_error_hides_the_warning() {
    let (input, home) = fixture();
    let output = run_plan(input.path(), home.path(), Some("error"));

    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Failed to parse inspect cache"),
        "{stderr}"
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("stdout is JSON");
}
