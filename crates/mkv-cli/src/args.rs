//! Command-line argument parsing for mkv-cli: flags into a `BatchRequest`, `--selected-item`
//! normalization against `--input-dir`, and the help text.

use std::fmt::Display;
use std::ops::RangeInclusive;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use mkv_core::pipeline::PipelineMode;
use mkv_core::preset::lookup_preset;
use mkv_core::request::{
    clean_relative_path, parse_assignments_json, BatchRequest, COMPLEXITY_RANGE, DEFAULT_IO_JOBS,
    DEFAULT_PRESET_ID, FFMPEG_THREADS_RANGE, IO_JOBS_RANGE, WORKERS_RANGE,
};

#[derive(Debug)]
pub(crate) struct CliArgs {
    pub(crate) request: BatchRequest,
    pub(crate) plan: bool,
    pub(crate) events: bool,
    pub(crate) clear_cache: bool,
    pub(crate) help: bool,
    pub(crate) has_input_dir: bool,
}

fn is_mkv_extract_exe(exe: &str) -> bool {
    let file_name = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    let lower = file_name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    stem == "mkv-extract"
}

pub(crate) fn parse_args() -> Result<CliArgs, String> {
    let mut env_args = std::env::args();
    let exe = env_args.next();
    parse_args_with_exe(exe.as_deref(), env_args)
}

fn next_value<T: AsRef<str>>(
    args: &mut impl Iterator<Item = T>,
    flag: &str,
) -> Result<String, String> {
    args.next()
        .map(|value| value.as_ref().to_owned())
        .ok_or_else(|| format!("Missing value for {flag}"))
}

/// Parses `raw` as `N`. The only place that formats the numeric parse error; `kind` names
/// the expected value in it ("integer", "number").
fn parse_number<N: FromStr>(raw: &str, flag: &str, kind: &str) -> Result<N, String> {
    raw.parse()
        .map_err(|_| format!("Invalid {kind} for {flag}"))
}

/// Takes the next argument as the value of `flag` and parses it as `kind` with `parse_number`.
fn next_number<T: AsRef<str>, N: FromStr>(
    args: &mut impl Iterator<Item = T>,
    flag: &str,
    kind: &str,
) -> Result<N, String> {
    parse_number(&next_value(args, flag)?, flag, kind)
}

/// Converts `--sub-offset` seconds to the engine's whole milliseconds, rounding half away
/// from zero. Rejects a value that is not finite or whose milliseconds do not fit `i64`.
/// The range error quotes `raw` as typed, since the parsed `f64` may already be rounded.
fn parse_seconds_as_ms(flag: &str, raw: &str) -> Result<i64, String> {
    let seconds: f64 = parse_number(raw, flag, "number")?;
    let ms = (seconds * 1000.0).round();
    // `as` saturates silently, so check the range first; 2^63 itself does not fit.
    if ms >= i64::MIN as f64 && ms < i64::MAX as f64 {
        Ok(ms as i64)
    } else {
        Err(format!(
            "Invalid {flag}: {raw} seconds is not a finite offset that fits in whole milliseconds"
        ))
    }
}

/// Parses CLI arguments (excluding the executable). An `exe` named `mkv-extract`
/// defaults the mode to Extract. Only the syntax of a value is checked here, including that
/// `--sub-offset` converts to whole milliseconds; ranges, the preset, the bitrate and paths are
/// left to `BatchRequest::resolve`.
fn parse_args_with_exe<I, T>(exe: Option<&str>, iter: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
{
    let mut request = BatchRequest::default();
    if exe.is_some_and(is_mkv_extract_exe) {
        request.mode = PipelineMode::Extract;
    }

    let mut plan = false;
    let mut events = false;
    let mut clear_cache = false;
    let mut help = false;
    let mut has_input_dir = false;
    let mut assignments_path: Option<PathBuf> = None;
    let mut raw_selected_items: Vec<String> = Vec::new();

    let mut it = iter.into_iter();
    while let Some(arg) = it.next() {
        let flag = arg.as_ref();
        match flag {
            "--plan" => plan = true,
            "--events" => events = true,
            "--clear-cache" => clear_cache = true,
            "-h" | "--help" => help = true,
            "--input-dir" => {
                request.input_dir = PathBuf::from(next_value(&mut it, flag)?);
                has_input_dir = true;
            }
            "--selected-item" => {
                let val = next_value(&mut it, flag)?;
                if val.trim().is_empty() {
                    return Err("Cannot specify empty --selected-item".to_string());
                }
                raw_selected_items.push(val);
            }
            "--selected-items" => {
                let val = next_value(&mut it, flag)?;
                let segments: Vec<&str> = val
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                if segments.is_empty() {
                    return Err("Cannot specify empty --selected-items".to_string());
                }
                raw_selected_items.extend(segments.into_iter().map(str::to_owned));
            }
            "--out-dir" => request.out_dir = Some(PathBuf::from(next_value(&mut it, flag)?)),
            "--fonts-dir" => request.fonts_dir = Some(PathBuf::from(next_value(&mut it, flag)?)),
            "--assets-dir" => {
                request.assets_dir = Some(PathBuf::from(next_value(&mut it, flag)?));
            }
            "--preset" => request.preset = Some(next_value(&mut it, flag)?),
            "--assignments" => assignments_path = Some(PathBuf::from(next_value(&mut it, flag)?)),
            "--extract" => request.mode = PipelineMode::Extract,
            "--remux" => request.mode = PipelineMode::Remux,
            "--dry-run" => request.dry_run = Some(true),
            "--no-cache" => request.no_cache = Some(true),
            "--sub-offset" => {
                let raw = next_value(&mut it, flag)?;
                request.default_sub_offset_ms = Some(parse_seconds_as_ms(flag, &raw)?);
            }
            "--workers" => request.workers = Some(next_number(&mut it, flag, "integer")?),
            "--io-jobs" => request.io_jobs = Some(next_number(&mut it, flag, "integer")?),
            "--complexity" => request.complexity = Some(next_number(&mut it, flag, "integer")?),
            "--ffmpeg-threads" => {
                let val = next_value(&mut it, flag)?;
                request.ffmpeg_threads = if val.eq_ignore_ascii_case("auto") {
                    None
                } else {
                    Some(parse_number(&val, flag, "integer")?)
                };
            }
            "--bitrate" => request.bitrate = Some(next_value(&mut it, flag)?),
            other => return Err(format!("Unknown argument: {other}")),
        }
    }

    if let Some(path) = assignments_path {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read assignments file '{}': {e}", path.display()))?;
        request.assignments = Some(parse_assignments_json(&content)?);
    }
    if !raw_selected_items.is_empty() {
        if !has_input_dir {
            return Err(
                "Cannot specify --selected-item or --selected-items without --input-dir"
                    .to_string(),
            );
        }
        let items = raw_selected_items
            .iter()
            .map(|raw| normalize_selected_item(raw, &request.input_dir))
            .collect::<Result<Vec<_>, _>>()?;
        request.selected_items = Some(items);
    }

    Ok(CliArgs {
        request,
        plan,
        events,
        clear_cache,
        help,
        has_input_dir,
    })
}

fn escapes_input_dir(input_dir: &Path, rel: &Path) -> bool {
    if let (Ok(canon_in), Ok(canon_target)) = (
        std::fs::canonicalize(input_dir),
        std::fs::canonicalize(input_dir.join(rel)),
    ) {
        return canon_target.strip_prefix(&canon_in).is_err();
    }

    let mut depth: isize = 0;
    for comp in rel.components() {
        match comp {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            Component::Normal(_) => depth += 1,
            Component::RootDir | Component::Prefix(_) => return true,
            Component::CurDir => {}
        }
    }
    false
}

/// Resolves a selected-item argument to an ID relative to `input_dir`.
///
/// Accepts paths relative to `input_dir`, absolute paths, and paths relative to
/// the working directory; rejects anything that resolves outside `input_dir`.
fn normalize_selected_item(raw: &str, input_dir: &Path) -> Result<String, String> {
    let outside = || {
        format!(
            "Selected item '{raw}' is outside input directory '{}'",
            input_dir.display()
        )
    };
    let relative_id = |rel: &Path| clean_relative_path(&rel.to_string_lossy());
    let path = Path::new(raw);

    if let Ok(rel) = path.strip_prefix(input_dir) {
        return Ok(relative_id(rel));
    }
    let canon_in = std::fs::canonicalize(input_dir).ok();
    if let Some(canon_in) = &canon_in {
        if let Ok(rel) = path.strip_prefix(canon_in) {
            return Ok(relative_id(rel));
        }
    }

    if path.is_absolute() {
        if let Some(canon_in) = &canon_in {
            if let Ok(canon_path) = std::fs::canonicalize(path) {
                if let Ok(rel) = canon_path.strip_prefix(canon_in) {
                    return Ok(relative_id(rel));
                }
            } else if let Some(parent) = path.parent() {
                // Nonexistent file under input_dir: resolve through its parent so
                // `BatchRequest::resolve` reports it as missing, not as outside.
                if let Ok(canon_parent) = std::fs::canonicalize(parent) {
                    if let Ok(rel_parent) = canon_parent.strip_prefix(canon_in) {
                        let file_name = path.file_name().unwrap_or_default();
                        return Ok(relative_id(rel_parent.join(file_name).as_path()));
                    }
                }
            }
        }
        return Err(outside());
    }

    if path.components().any(|c| matches!(c, Component::ParentDir))
        && escapes_input_dir(input_dir, path)
    {
        return Err(outside());
    }

    let target = input_dir.join(path);
    if target.exists() {
        if let (Some(canon_in), Ok(canon_target)) = (&canon_in, std::fs::canonicalize(&target)) {
            return canon_target
                .strip_prefix(canon_in)
                .map(relative_id)
                .map_err(|_| outside());
        }
    } else if let Ok(cwd) = std::env::current_dir() {
        let cwd_path = cwd.join(path);
        if cwd_path.exists() {
            if let (Some(canon_in), Ok(canon_cwd)) = (&canon_in, std::fs::canonicalize(&cwd_path)) {
                if let Ok(rel) = canon_cwd.strip_prefix(canon_in) {
                    return Ok(relative_id(rel));
                }
            }
            return Err(outside());
        }
    }
    Ok(clean_relative_path(raw))
}

/// Words a range for the help text, e.g. `1-64`.
fn range_text<T: Display>(range: &RangeInclusive<T>) -> String {
    format!("{}-{}", range.start(), range.end())
}

/// The `--help` text; its ranges, the `--io-jobs` default and the bitrate and complexity
/// fallbacks come from the core constants and the default Preset Profile that
/// `BatchRequest::resolve` applies.
fn help_text() -> String {
    let default_preset = lookup_preset(DEFAULT_PRESET_ID).expect("the default preset is built in");
    let default_bitrate = default_preset.bitrate;
    let default_complexity = default_preset.complexity;
    let complexity = range_text(&COMPLEXITY_RANGE);
    let workers = range_text(&WORKERS_RANGE);
    let io_jobs = range_text(&IO_JOBS_RANGE);
    let ffmpeg_threads = range_text(&FFMPEG_THREADS_RANGE);
    format!(
        r#"mkv-cli - Batch MKV remux and extraction for anime releases

USAGE:
    mkv-cli --input-dir <DIR> [OPTIONS]

OPTIONS:
    --input-dir <DIR>          Source directory containing anime video releases
    --out-dir <DIR>            Destination output directory for processed files
    --preset <PROFILE>         Preset profile (balanced, fast, hifi, custom; also default, high_quality/high-quality)
    --selected-item <PATH>     Include a single release item (repeatable, preserves commas)
    --selected-items <LIST>    Include comma-separated release items (repeatable)
    --assignments <FILE>       Path to JSON file mapping episode IDs to track paths
    --fonts-dir <DIR>          Explicit fonts directory override
    --assets-dir <DIR>         Fallback asset directory
    --plan                     Generate structured inspection manifest JSON for queue hydration
    --events                   Emit machine-readable NDJSON events prefixed with '@event: '
    --remux                    Process batch in remux mode (default for mkv-cli)
    --extract                  Extract best-match subtitles, fonts, chapters, and covers
    --dry-run                  Simulate batch processing without executing remux commands
    --bitrate <RATE>           Opus audio transcoding bitrate with optional k/K/M suffix (default: preset bitrate, else {default_bitrate})
    --complexity <{complexity}>        Opus compression complexity level, {complexity} (default: preset complexity, else {default_complexity})
    --workers <N>              Number of concurrent worker tasks, {workers} (default: physical cores clamped 1-16)
    --io-jobs <N>              Disk I/O jobs concurrency limit, {io_jobs} (0 uses the worker count) (default: {DEFAULT_IO_JOBS})
    --ffmpeg-threads <N|auto>  FLAC decoder threads per audio transcode job, {ffmpeg_threads} (default: auto; Opus encoding is single-threaded)
    --sub-offset <SEC>         Subtitle timestamp offset in seconds, rounded half away from zero to whole milliseconds (default: 0)
    --no-cache                 Bypass persistent inspect cache
    --clear-cache              Clear persistent inspection cache and exit
    -h, --help                 Show this help message
"#
    )
}

pub(crate) fn print_help() {
    println!("{}", help_text());
}

/// Test helpers shared by this module's tests and `main.rs`'s plan-manifest tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{parse_args_with_exe, CliArgs};
    use std::path::Path;

    pub(crate) fn parse_args_from<I, T>(iter: I) -> Result<CliArgs, String>
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        parse_args_with_exe(None, iter)
    }

    pub(crate) fn path_arg(path: &Path) -> &str {
        path.to_str().expect("test paths are UTF-8")
    }

    /// Creates each file (and its parent directories) under `dir`, with its name as content.
    pub(crate) fn write_files(dir: &Path, names: &[&str]) {
        for name in names {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, name.as_bytes()).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use mkv_core::pipeline::PipelineConfig;
    use tempfile::tempdir;

    fn write_assignments(dir: &Path, file_name: &str, json: &str) -> PathBuf {
        let path = dir.join(file_name);
        std::fs::write(&path, json).unwrap();
        path
    }

    fn selected_items_for(input_dir: &str, items: &[&str]) -> Result<Vec<String>, String> {
        let mut args = vec!["--input-dir", input_dir];
        for item in items {
            args.extend(["--selected-item", *item]);
        }
        Ok(parse_args_from(args)?
            .request
            .selected_items
            .expect("selected_items should be set"))
    }

    /// Parses `flags` after an `--input-dir` that exists, then resolves the request.
    fn resolve_flags(flags: &[&str]) -> Result<PipelineConfig, String> {
        let dir = tempdir().unwrap();
        let mut args = vec!["--input-dir", path_arg(dir.path())];
        args.extend(flags);
        parse_args_from(args)?.request.resolve()
    }

    #[test]
    fn test_bitrate_is_checked_by_resolve() {
        for bad in ["0", "0k", "invalid", "1m"] {
            assert_eq!(
                resolve_flags(&["--bitrate", bad]).unwrap_err(),
                format!(
                    "Invalid bitrate format: '{bad}'. Must be a positive integer with an optional k/K/M suffix, like 128k or 192k."
                )
            );
        }
        for (ok, expected) in [("128k", "128k"), ("192K", "192K"), ("1M", "1M")] {
            let config = resolve_flags(&["--bitrate", ok]).unwrap();
            assert_eq!(config.bitrate, expected);
        }
    }

    #[test]
    fn test_cli_selected_item_duplicate_fails_validation() {
        let dir = tempdir().unwrap();
        write_files(dir.path(), &["ep01.mkv"]);
        let input_dir = path_arg(dir.path());
        let items = ["ep01.mkv", "ep01.mkv"];
        assert_eq!(selected_items_for(input_dir, &items).unwrap(), items);

        let mut args = vec!["--input-dir", input_dir];
        for item in items {
            args.extend(["--selected-item", item]);
        }
        let err = parse_args_from(args)
            .unwrap()
            .request
            .resolve()
            .unwrap_err();
        assert!(
            err.contains("Duplicate entry in selected_items: 'ep01.mkv'"),
            "{err}"
        );
    }

    #[test]
    fn test_value_flags_require_value() {
        for flag in [
            "--input-dir",
            "--selected-item",
            "--selected-items",
            "--out-dir",
            "--fonts-dir",
            "--assets-dir",
            "--preset",
            "--assignments",
            "--sub-offset",
            "--workers",
            "--io-jobs",
            "--ffmpeg-threads",
            "--bitrate",
            "--complexity",
        ] {
            assert_eq!(
                parse_args_from([flag]).unwrap_err(),
                format!("Missing value for {flag}")
            );
        }
    }

    #[test]
    fn test_numeric_flags_parse_without_range_checks() {
        for flag in [
            "--complexity",
            "--workers",
            "--io-jobs",
            "--ffmpeg-threads",
            "--sub-offset",
        ] {
            assert!(parse_args_from([flag, "invalid"]).is_err(), "{flag}");
        }

        // Out-of-range values pass through; BatchRequest::resolve owns the bounds.
        let parsed = parse_args_from([
            "--complexity",
            "11",
            "--workers",
            "0",
            "--io-jobs",
            "65",
            "--ffmpeg-threads",
            "0",
            "--sub-offset",
            "-1.5",
        ])
        .unwrap()
        .request;
        assert_eq!(parsed.complexity, Some(11));
        assert_eq!(parsed.workers, Some(0));
        assert_eq!(parsed.io_jobs, Some(65));
        assert_eq!(parsed.ffmpeg_threads, Some(0));
        assert_eq!(parsed.default_sub_offset_ms, Some(-1500));
    }

    #[test]
    fn test_sub_offset_seconds_round_to_whole_milliseconds() {
        let ms = |sec: &str| {
            parse_args_from(["--sub-offset", sec])
                .map(|parsed| parsed.request.default_sub_offset_ms)
        };
        for (sec, expected) in [
            ("0.5", 500),
            ("-1.5", -1500),
            ("0", 0),
            ("-0", 0),
            // Half a millisecond rounds away from zero, as the desktop controller does.
            ("0.0005", 1),
            ("-0.0005", -1),
            ("0.0004", 0),
            ("1.2344", 1234),
        ] {
            assert_eq!(ms(sec), Ok(Some(expected)), "{sec}");
        }

        // The last value parses to exactly 2^63 ms, one past `i64::MAX`; the error quotes it
        // as typed rather than as the rounded `f64`.
        for bad in [
            "inf",
            "-inf",
            "NaN",
            "1e300",
            "-1e300",
            "9223372036854775.807",
        ] {
            let err = ms(bad).unwrap_err();
            assert!(
                err.starts_with(&format!("Invalid --sub-offset: {bad} seconds "))
                    && err.contains("whole milliseconds"),
                "{bad}: {err}"
            );
        }
        assert_eq!(
            ms("abc"),
            Err("Invalid number for --sub-offset".to_string())
        );
    }

    #[test]
    fn test_ffmpeg_threads_auto_clears_override() {
        let parsed =
            parse_args_from(["--ffmpeg-threads", "4", "--ffmpeg-threads", "AUTO"]).unwrap();
        assert_eq!(parsed.request.ffmpeg_threads, None);
    }

    #[test]
    fn test_numeric_flag_parse_error_messages() {
        for (flag, expected) in [
            ("--workers", "Invalid integer for --workers"),
            ("--io-jobs", "Invalid integer for --io-jobs"),
            ("--complexity", "Invalid integer for --complexity"),
            ("--ffmpeg-threads", "Invalid integer for --ffmpeg-threads"),
            ("--sub-offset", "Invalid number for --sub-offset"),
        ] {
            assert_eq!(
                parse_args_from([flag, "x"]).err().as_deref(),
                Some(expected),
                "{flag}"
            );
        }
    }

    #[test]
    fn test_cli_flags_and_extract() {
        let parsed = parse_args_from([
            "--input-dir",
            "/tmp/anime",
            "--plan",
            "--events",
            "--clear-cache",
            "--extract",
            "--dry-run",
            "--no-cache",
        ])
        .unwrap();

        assert!(parsed.has_input_dir);
        assert_eq!(parsed.request.input_dir, PathBuf::from("/tmp/anime"));
        assert!(parsed.plan);
        assert!(parsed.events);
        assert!(parsed.clear_cache);
        assert_eq!(parsed.request.mode, PipelineMode::Extract);
        assert_eq!(parsed.request.dry_run, Some(true));
        assert_eq!(parsed.request.no_cache, Some(true));
    }

    #[test]
    fn test_help_flag() {
        assert!(parse_args_from(["-h"]).unwrap().help);
        assert!(parse_args_from(["--help"]).unwrap().help);
    }

    #[test]
    fn test_help_ranges_come_from_the_core_constants() {
        let help = help_text();
        let balanced = lookup_preset(DEFAULT_PRESET_ID).unwrap();
        for line in [
            format!(
                "--complexity <{0}>        Opus compression complexity level, {0} (default: preset complexity, else {1})",
                range_text(&COMPLEXITY_RANGE),
                balanced.complexity
            ),
            format!(
                "--bitrate <RATE>           Opus audio transcoding bitrate with optional k/K/M suffix (default: preset bitrate, else {})",
                balanced.bitrate
            ),
            format!(
                "Number of concurrent worker tasks, {} ",
                range_text(&WORKERS_RANGE)
            ),
            format!(
                "Disk I/O jobs concurrency limit, {} (0 uses the worker count) (default: {DEFAULT_IO_JOBS})",
                range_text(&IO_JOBS_RANGE)
            ),
            format!(
                "FLAC decoder threads per audio transcode job, {} ",
                range_text(&FFMPEG_THREADS_RANGE)
            ),
        ] {
            assert!(help.contains(&line), "{line}");
        }
    }

    #[test]
    fn test_unknown_flag() {
        assert!(parse_args_from(["--unknown-flag"]).is_err());
        assert_eq!(
            parse_args_from(["--sub-assignments", "subs.json"]).unwrap_err(),
            "Unknown argument: --sub-assignments"
        );
    }

    #[test]
    fn test_preset_parsing() {
        let parsed = parse_args_from(["--preset", "balanced"]).unwrap();
        assert_eq!(parsed.request.preset.as_deref(), Some("balanced"));

        let parsed_fast = parse_args_from(["--preset", "fast"]).unwrap();
        assert_eq!(parsed_fast.request.preset.as_deref(), Some("fast"));

        // The preset is checked by `resolve`, not while parsing.
        let parsed_unknown = parse_args_from(["--preset", "unknown_profile"]).unwrap();
        assert_eq!(
            parsed_unknown.request.preset.as_deref(),
            Some("unknown_profile")
        );
        assert_eq!(
            resolve_flags(&["--preset", "unknown_profile"]).unwrap_err(),
            "Unknown preset profile: 'unknown_profile'"
        );
    }

    #[test]
    fn test_assignments_parsing() {
        let dir = tempdir().unwrap();
        let assign_file = write_assignments(
            dir.path(),
            "assignments.json",
            r#"[{"target_video": "ep01.mkv", "asset_path": "ep01.ass"}]"#,
        );

        let parsed = parse_args_from([
            "--input-dir",
            path_arg(dir.path()),
            "--assignments",
            path_arg(&assign_file),
        ])
        .unwrap();

        let assignments = parsed.request.assignments.expect("should have assignments");
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].target_video, "ep01.mkv");
        assert_eq!(assignments[0].asset_path, PathBuf::from("ep01.ass"));
    }

    #[test]
    fn test_assignments_reject_non_subtitle_track_type() {
        let dir = tempdir().unwrap();
        let entry =
            r#"[{"target_video": "ep01.mkv", "asset_path": "ep01.ass", "track_type": "audio"}]"#;
        for json in [entry.to_string(), format!(r#"{{"assignments": {entry}}}"#)] {
            let assign_file = write_assignments(dir.path(), "assignments.json", &json);
            let err = parse_args_from([
                "--input-dir",
                path_arg(dir.path()),
                "--assignments",
                path_arg(&assign_file),
            ])
            .unwrap_err();
            assert!(
                err.starts_with("Invalid track assignment: unknown variant `audio`"),
                "{json}: {err}"
            );
        }
    }

    #[test]
    fn test_request_resolve_preflight() {
        let dir = tempdir().unwrap();
        write_files(dir.path(), &["ep01.mkv", "ep01.ass"]);
        let input_dir = path_arg(dir.path());

        let assign_file = write_assignments(
            dir.path(),
            "assignments.json",
            r#"[{"target_video": "ep01.mkv", "asset_path": "ep01.ass"}]"#,
        );
        let parsed = parse_args_from([
            "--input-dir",
            input_dir,
            "--assignments",
            path_arg(&assign_file),
            "--preset",
            "fast",
            "--workers",
            "4",
        ])
        .unwrap();
        assert!(parsed.request.resolve().is_ok());

        let assign_missing_file = write_assignments(
            dir.path(),
            "assignments_missing.json",
            r#"[{"target_video": "missing.mkv", "asset_path": "ep01.ass"}]"#,
        );
        let parsed_missing = parse_args_from([
            "--input-dir",
            input_dir,
            "--assignments",
            path_arg(&assign_missing_file),
        ])
        .unwrap();
        assert!(parsed_missing.request.resolve().is_err());
    }

    #[test]
    fn test_cli_selected_item_literal_comma_preserved() {
        let dir = tempdir().unwrap();
        let filename = "[Group] Show - 01 [1080p, AAC].mkv";
        write_files(dir.path(), &[filename]);

        let items = selected_items_for(path_arg(dir.path()), &[filename]).unwrap();
        assert_eq!(items, vec![filename]);
    }

    #[test]
    fn test_cli_selected_items_comma_separated() {
        let dir = tempdir().unwrap();
        write_files(dir.path(), &["ep01.mkv", "ep02.mkv", "ep03.mkv"]);

        let parsed = parse_args_from([
            "--input-dir",
            path_arg(dir.path()),
            "--selected-items",
            "ep01.mkv,  ep02.mkv ,ep03.mkv ",
        ])
        .unwrap();

        let items = parsed
            .request
            .selected_items
            .expect("should have selected_items");
        assert_eq!(items, vec!["ep01.mkv", "ep02.mkv", "ep03.mkv"]);
    }

    #[test]
    fn test_cli_selected_item_missing_input_dir_fails() {
        let expected = "Cannot specify --selected-item or --selected-items without --input-dir";
        assert_eq!(
            parse_args_from(["--selected-item", "ep01.mkv"]).unwrap_err(),
            expected
        );
        assert_eq!(
            parse_args_from(["--selected-items", "ep01.mkv,ep02.mkv"]).unwrap_err(),
            expected
        );
    }

    #[test]
    fn test_cli_selected_item_outside_input_dir_fails() {
        let dir = tempdir().unwrap();
        let input_dir = dir.path().join("input");
        std::fs::create_dir_all(&input_dir).unwrap();
        write_files(dir.path(), &["outside/secret.mkv"]);
        let outside_file = dir.path().join("outside/secret.mkv");

        for item in ["../outside/secret.mkv", path_arg(&outside_file)] {
            let err = selected_items_for(path_arg(&input_dir), &[item]).unwrap_err();
            assert!(err.contains("is outside input directory"), "{item}: {err}");
        }
    }

    #[test]
    fn test_cli_selected_item_cwd_relative_resolution() {
        let cwd = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&cwd).unwrap();
        write_files(dir.path(), &["ep01.mkv"]);
        let rel_from_cwd = dir.path().strip_prefix(&cwd).unwrap().join("ep01.mkv");

        let items = selected_items_for(path_arg(dir.path()), &[path_arg(&rel_from_cwd)]).unwrap();
        assert_eq!(items, vec!["ep01.mkv"]);
    }

    #[test]
    fn test_cli_selected_item_relative_input_dir_prefix_stripped() {
        let items =
            selected_items_for("media/anime", &["media/anime/Season 1/S01E01.mkv"]).unwrap();
        assert_eq!(items, vec!["Season 1/S01E01.mkv"]);
    }

    #[test]
    fn test_cli_selected_item_empty_flag_fails() {
        assert_eq!(
            parse_args_from(["--input-dir", ".", "--selected-item", ""]).unwrap_err(),
            "Cannot specify empty --selected-item"
        );
        assert_eq!(
            parse_args_from(["--input-dir", ".", "--selected-items", "  ,  "]).unwrap_err(),
            "Cannot specify empty --selected-items"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_cli_selected_item_symlink_escape_fails() {
        let outside_dir = tempdir().unwrap();
        write_files(outside_dir.path(), &["secret.mkv"]);
        let input_dir = tempdir().unwrap();
        std::os::unix::fs::symlink(
            outside_dir.path().join("secret.mkv"),
            input_dir.path().join("leak.mkv"),
        )
        .unwrap();

        let err = selected_items_for(path_arg(input_dir.path()), &["leak.mkv"]).unwrap_err();
        assert!(err.contains("is outside input directory"), "{err}");
    }

    #[test]
    fn test_cli_selected_item_absolute_nonexistent_inside_reports_not_found() {
        let dir = tempdir().unwrap();
        let ghost = dir.path().join("ghost.mkv");

        let parsed = parse_args_from([
            "--input-dir",
            path_arg(dir.path()),
            "--selected-item",
            path_arg(&ghost),
        ])
        .unwrap();

        assert_eq!(
            parsed.request.selected_items.as_deref(),
            Some(&["ghost.mkv".to_string()][..])
        );
        let err = parsed.request.resolve().unwrap_err();
        assert!(
            err.contains("Selected item does not exist:"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_is_mkv_extract_exe() {
        assert!(is_mkv_extract_exe("mkv-extract"));
        assert!(is_mkv_extract_exe("/usr/bin/mkv-extract"));
        assert!(is_mkv_extract_exe("./mkv-extract"));
        assert!(is_mkv_extract_exe("mkv-extract.exe"));
        assert!(is_mkv_extract_exe("MKV-EXTRACT.EXE"));
        assert!(is_mkv_extract_exe("mkv-extract.Exe"));
        assert!(is_mkv_extract_exe(r"C:\tools\mkv-extract.exe"));
        assert!(is_mkv_extract_exe(
            r"C:\Program Files\MKVAnimeBatch\MKV-EXTRACT.EXE"
        ));

        assert!(!is_mkv_extract_exe("mkv-cli"));
        assert!(!is_mkv_extract_exe("/usr/bin/mkv-cli"));
        assert!(!is_mkv_extract_exe("mkv-cli.exe"));
        assert!(!is_mkv_extract_exe("MKV-CLI.EXE"));
        assert!(!is_mkv_extract_exe(r"C:\bin\mkv-cli.exe"));
        assert!(!is_mkv_extract_exe("other-mkv-extract.exe"));
        assert!(!is_mkv_extract_exe(""));
    }

    #[test]
    fn test_exe_name_mode_default_and_overrides() {
        let mode = |exe: &str, args: &[&str]| {
            parse_args_with_exe(Some(exe), args.iter())
                .unwrap()
                .request
                .mode
        };
        assert_eq!(mode("mkv-cli", &["--input-dir", "."]), PipelineMode::Remux);
        assert_eq!(
            mode("mkv-cli", &["--input-dir", ".", "--extract"]),
            PipelineMode::Extract
        );
        assert_eq!(
            mode("mkv-extract", &["--input-dir", "."]),
            PipelineMode::Extract
        );
        assert_eq!(
            mode("mkv-extract", &["--input-dir", ".", "--remux"]),
            PipelineMode::Remux
        );
    }
}
