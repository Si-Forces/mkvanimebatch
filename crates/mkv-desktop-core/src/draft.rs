//! The Settings Draft of each mode (ADR-0016): the values of its settings form, held as typed, so a
//! view only mirrors them. Edits arrive as Intents; only the complexity is normalized (clamped) on
//! edit, everything else is parsed when a Run builds its `BatchRequest`, and nothing is persisted.

use crate::queue::{Direction, MetaInputs};
use crate::text::is_js_whitespace;
use mkv_core::pipeline::PipelineMode;
use mkv_core::preset::require_preset;
use mkv_core::request::{
    seconds_to_ms, BatchRequest, EffectiveSettings, COMPLEXITY_RANGE, DEFAULT_PRESET_ID,
    FFMPEG_THREADS_RANGE, IO_JOBS_RANGE, WORKERS_RANGE,
};
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::str::FromStr;

/// The Preset Profile a hand-edited encode is labelled with.
const CUSTOM_PRESET: &str = "custom";

/// One mode's settings form, every field a string as typed but the two checkboxes. The remux-only
/// fields are `None` in an extract draft and left out of its wire form.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingsDraft {
    pub input_dir: String,
    pub out_dir: String,
    pub fonts_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets_dir: Option<String>,
    /// The Preset Profile id the form is on: a label, since the bitrate and complexity are what a
    /// run sends.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complexity: Option<String>,
    /// Seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_offset: Option<String>,
    pub workers: String,
    pub io_jobs: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ffmpeg_threads: Option<String>,
    pub dry_run: bool,
    pub no_cache: bool,
    /// The field (its wire name) the last Run could not read, which a view flags: set by a Run
    /// that fails to build its request, cleared by the next edit of that field or the next Run.
    pub invalid: Option<&'static str>,
}

/// Why a draft builds no request: the field (its wire name) and the error text.
pub type DraftError = (&'static str, String);

/// The fields an `EditDraft` Intent sets; each one left out stays as it is. The preset is set by
/// `SetPreset` alone.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftChanges {
    pub input_dir: Option<String>,
    pub out_dir: Option<String>,
    pub fonts_dir: Option<String>,
    pub assets_dir: Option<String>,
    pub bitrate: Option<String>,
    pub complexity: Option<String>,
    pub sub_offset: Option<String>,
    pub workers: Option<String>,
    pub io_jobs: Option<String>,
    pub ffmpeg_threads: Option<String>,
    pub dry_run: Option<bool>,
    pub no_cache: Option<bool>,
}

/// A Settings Draft field a stepper steps, by one, or the subtitle offset by half a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepField {
    SubOffset,
    Workers,
    IoJobs,
    FfmpegThreads,
}

/// Sets `field` to `value` when there is one; true when that changed it.
fn replace<T: PartialEq>(field: &mut T, value: Option<T>) -> bool {
    match value {
        Some(value) if *field != value => {
            *field = value;
            true
        }
        _ => false,
    }
}

impl SettingsDraft {
    /// The form a mode starts with: empty, but for the remux encode, which starts on the default
    /// Preset Profile, and its zero subtitle offset.
    pub fn new(mode: PipelineMode) -> Self {
        let remux = mode == PipelineMode::Remux;
        let preset = remux
            .then(|| require_preset(DEFAULT_PRESET_ID).expect("the default preset is built in"));
        Self {
            input_dir: String::new(),
            out_dir: String::new(),
            fonts_dir: String::new(),
            assets_dir: remux.then(String::new),
            bitrate: preset.as_ref().map(|preset| preset.bitrate.clone()),
            complexity: preset.as_ref().map(|preset| preset.complexity.to_string()),
            preset: preset.map(|preset| preset.id),
            sub_offset: remux.then(|| "0".into()),
            workers: String::new(),
            io_jobs: String::new(),
            ffmpeg_threads: remux.then(String::new),
            dry_run: false,
            no_cache: false,
            invalid: None,
        }
    }

    /// Applies `changes`, all or none: true when a field changed. The complexity is clamped to the
    /// engine's range (see `clamp_complexity`). A changed bitrate or complexity is a hand-edited
    /// encode, so the preset becomes custom. A field the mode does not have is an error.
    pub fn edit(&mut self, changes: DraftChanges) -> Result<bool, String> {
        let DraftChanges {
            input_dir,
            out_dir,
            fonts_dir,
            assets_dir,
            bitrate,
            complexity,
            sub_offset,
            workers,
            io_jobs,
            ffmpeg_threads,
            dry_run,
            no_cache,
        } = changes;
        // Editing the flagged field clears the flag, whatever the new value.
        let edited = |field: &str| match field {
            "input_dir" => input_dir.is_some(),
            "sub_offset" => sub_offset.is_some(),
            "complexity" => complexity.is_some(),
            "workers" => workers.is_some(),
            "io_jobs" => io_jobs.is_some(),
            "ffmpeg_threads" => ffmpeg_threads.is_some(),
            _ => false,
        };
        let unflag = self.invalid.is_some_and(edited);
        let complexity = complexity.as_deref().map(clamp_complexity);
        let encode = (self.bitrate.clone(), self.complexity.clone());
        // A field that is `None` is one the mode lacks: an extract draft has none of these.
        let remux_only = [
            ("assets_dir", &mut self.assets_dir, assets_dir),
            ("bitrate", &mut self.bitrate, bitrate),
            ("complexity", &mut self.complexity, complexity),
            ("sub_offset", &mut self.sub_offset, sub_offset),
            ("ffmpeg_threads", &mut self.ffmpeg_threads, ffmpeg_threads),
        ];
        if let Some((name, ..)) = remux_only
            .iter()
            .find(|(_, field, value)| field.is_none() && value.is_some())
        {
            return Err(format!("Extract has no {name} setting."));
        }

        let mut changed = unflag;
        if unflag {
            self.invalid = None;
        }
        for (_, field, value) in remux_only {
            if let Some(field) = field {
                changed |= replace(field, value);
            }
        }
        for (field, value) in [
            (&mut self.input_dir, input_dir),
            (&mut self.out_dir, out_dir),
            (&mut self.fonts_dir, fonts_dir),
            (&mut self.workers, workers),
            (&mut self.io_jobs, io_jobs),
        ] {
            changed |= replace(field, value);
        }
        changed |= replace(&mut self.dry_run, dry_run);
        changed |= replace(&mut self.no_cache, no_cache);
        if (&self.bitrate, &self.complexity) != (&encode.0, &encode.1) {
            self.preset = Some(CUSTOM_PRESET.into());
        }
        Ok(changed)
    }

    /// Puts the form on Preset Profile `id`, under its canonical id, as a request resolves it:
    /// its bitrate and complexity, or for custom only the label, which keeps the values the form
    /// has. True when that changed the form.
    pub fn set_preset(&mut self, id: &str) -> Result<bool, String> {
        let Some(preset) = self.preset.as_mut() else {
            return Err("Preset Profiles are for remux only.".into());
        };
        let profile = require_preset(id)?;
        let mut changed = replace(preset, Some(profile.id.clone()));
        if profile.id != CUSTOM_PRESET {
            let bitrate = Some(profile.bitrate);
            let complexity = Some(profile.complexity.to_string());
            changed |= self.bitrate != bitrate || self.complexity != complexity;
            self.bitrate = bitrate;
            self.complexity = complexity;
            // The profile replaced a flagged complexity.
            if self.invalid == Some("complexity") {
                self.invalid = None;
                changed = true;
            }
        }
        Ok(changed)
    }

    /// Steps `field` one step `up` or down, as an edit of it (see `edit`): from its value, else
    /// from the engine's default behind its `auto` hint (the workers and Disk I/O, from
    /// `defaults`), else from 0, clamped to the range the engine accepts.
    pub fn step(
        &mut self,
        field: StepField,
        direction: Direction,
        defaults: Option<&EffectiveSettings>,
    ) -> Result<bool, String> {
        let bounds =
            |range: RangeInclusive<usize>| Some((*range.start() as f64, *range.end() as f64));
        let (current, step, bounds, auto) = match field {
            StepField::SubOffset => (self.sub_offset.as_deref(), 0.5, None, None),
            StepField::Workers => (
                Some(self.workers.as_str()),
                1.0,
                bounds(WORKERS_RANGE),
                defaults.map(|defaults| defaults.workers),
            ),
            StepField::IoJobs => (
                Some(self.io_jobs.as_str()),
                1.0,
                bounds(IO_JOBS_RANGE),
                defaults.map(|defaults| defaults.io_jobs),
            ),
            StepField::FfmpegThreads => (
                self.ffmpeg_threads.as_deref(),
                1.0,
                bounds(FFMPEG_THREADS_RANGE),
                None,
            ),
        };
        let sign = match direction {
            Direction::Up => 1.0,
            Direction::Down => -1.0,
        };
        // A field the mode lacks steps nothing: `edit` refuses it.
        let value = next_step_value(
            current.unwrap_or_default(),
            auto.map(|auto| auto as f64),
            sign * step,
            bounds,
        )
        .to_string();
        let mut changes = DraftChanges::default();
        *match field {
            StepField::SubOffset => &mut changes.sub_offset,
            StepField::Workers => &mut changes.workers,
            StepField::IoJobs => &mut changes.io_jobs,
            StepField::FfmpegThreads => &mut changes.ffmpeg_threads,
        } = Some(value);
        self.edit(changes)
    }

    /// What the rows of `mode` read from the draft for their meta lines: the remux bitrate and
    /// assets dir, trimmed, as a run would send them; nothing for extract.
    pub fn meta_inputs(&self, mode: PipelineMode) -> MetaInputs {
        let trimmed =
            |value: &Option<String>| value.as_deref().unwrap_or_default().trim().to_string();
        match mode {
            PipelineMode::Remux => MetaInputs {
                remux: true,
                bitrate: trimmed(&self.bitrate),
                assets_dir: trimmed(&self.assets_dir),
            },
            PipelineMode::Extract => MetaInputs::default(),
        }
    }

    /// The `BatchRequest` a Run of `mode` sends, before preflight sets its Queue Selection and
    /// Track Assignments. Every value is trimmed and an empty one is unset; the sub-offset is read
    /// as seconds, and the counts as whole numbers. Extract sends no preset.
    pub fn request(&self, mode: PipelineMode) -> Result<BatchRequest, DraftError> {
        let input_dir = self.input_dir.trim();
        if input_dir.is_empty() {
            return Err(("input_dir", "Input directory is required.".into()));
        }
        let default_sub_offset_ms = match text(self.sub_offset.as_deref()) {
            None => None,
            Some(raw) => match raw.parse().ok().and_then(seconds_to_ms) {
                Some(ms) => Some(ms),
                None => {
                    let message = format!(
                        "Invalid sub-offset value '{raw}': must be a number of seconds within range."
                    );
                    return Err(("sub_offset", message));
                }
            },
        };
        let path = |value: Option<&str>| text(value).map(PathBuf::from);
        Ok(BatchRequest {
            mode,
            input_dir: input_dir.into(),
            out_dir: path(Some(&self.out_dir)),
            fonts_dir: path(Some(&self.fonts_dir)),
            assets_dir: path(self.assets_dir.as_deref()),
            preset: self.preset.clone(),
            bitrate: text(self.bitrate.as_deref()).map(str::to_string),
            // `new`, `set_preset` and `edit` (which clamps, #126) store a whole number in range;
            // only a direct write to the field can reach this error.
            complexity: whole(
                self.complexity.as_deref(),
                "complexity",
                "encoder complexity",
            )?,
            workers: whole(Some(&self.workers), "workers", "workers")?,
            io_jobs: whole(Some(&self.io_jobs), "io_jobs", "Disk I/O")?,
            ffmpeg_threads: whole(
                self.ffmpeg_threads.as_deref(),
                "ffmpeg_threads",
                "FFmpeg threads",
            )?,
            default_sub_offset_ms,
            dry_run: Some(self.dry_run),
            no_cache: Some(self.no_cache),
            assignments: None,
            selected_items: None,
        })
    }
}

/// The value a stepper click puts in a field holding `current`: `current` plus `delta`, where a
/// field that holds no finite number starts from `auto`, else 0, clamped to `bounds` and
/// rounded to 10 significant digits, which drops noise such as 0.30000000000000004. A typed 0 is a value. A
/// number field holds a number or nothing, so a strict parse stands in for JavaScript's lenient
/// `parseFloat`; Rust's exact decimal rounding and `toPrecision` differ only on an exact binary
/// tie at the tenth digit.
fn next_step_value(
    current: &str,
    auto: Option<f64>,
    delta: f64,
    bounds: Option<(f64, f64)>,
) -> f64 {
    let start = current
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .or(auto)
        .unwrap_or(0.0);
    let mut value = start + delta;
    if let Some((min, max)) = bounds {
        value = value.clamp(min, max);
    }
    // Plus zero, so the field never reads `-0`.
    format!("{value:.9e}").parse::<f64>().unwrap_or(value) + 0.0
}

/// A typed complexity as the form held it before (ADR-0016, #126): its leading integer, as
/// JavaScript's `parseInt(raw, 10)` reads it, clamped to `COMPLEXITY_RANGE`; the range's minimum
/// when it has none.
fn clamp_complexity(raw: &str) -> String {
    let (min, max) = (*COMPLEXITY_RANGE.start(), *COMPLEXITY_RANGE.end());
    let raw = raw.trim_start_matches(is_js_whitespace);
    let (negative, raw) = match raw.as_bytes().first() {
        Some(b'-') => (true, &raw[1..]),
        Some(b'+') => (false, &raw[1..]),
        _ => (false, raw),
    };
    let digits = raw.len() - raw.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return min.to_string();
    }
    // Only an overflow fails to parse a run of digits: past the maximum either way.
    let value = raw[..digits].parse::<u32>().unwrap_or(u32::MAX);
    if negative { min } else { value.clamp(min, max) }.to_string()
}

/// `value` trimmed, or `None` when the mode lacks the field or it is empty.
fn text(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// `value` as a whole number, or `None` when it is empty; `label` is the field as the form
/// names it.
fn whole<T: FromStr>(
    value: Option<&str>,
    field: &'static str,
    label: &str,
) -> Result<Option<T>, DraftError> {
    text(value)
        .map(|raw| {
            raw.parse().map_err(|_| {
                let message = format!("Invalid {label} value '{raw}': must be a whole number.");
                (field, message)
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remux() -> SettingsDraft {
        SettingsDraft::new(PipelineMode::Remux)
    }

    #[test]
    fn test_a_draft_starts_on_the_forms_defaults() {
        let draft = remux();
        assert_eq!(draft.preset.as_deref(), Some("balanced"));
        assert_eq!(draft.bitrate.as_deref(), Some("192k"));
        assert_eq!(draft.complexity.as_deref(), Some("5"));
        assert_eq!(draft.sub_offset.as_deref(), Some("0"));
        assert_eq!(draft.assets_dir.as_deref(), Some(""));
        assert_eq!(draft.ffmpeg_threads.as_deref(), Some(""));
        assert_eq!((draft.workers.as_str(), draft.io_jobs.as_str()), ("", ""));
        assert!(!draft.dry_run && !draft.no_cache);
    }

    #[test]
    fn test_an_edit_answers_whether_it_changed_anything() {
        let mut draft = remux();
        let edit = DraftChanges {
            input_dir: Some(" /anime ".into()),
            workers: Some("4x".into()),
            dry_run: Some(true),
            ..DraftChanges::default()
        };
        assert_eq!(draft.edit(edit.clone()), Ok(true));
        // Held as typed: nothing is trimmed or parsed.
        assert_eq!(draft.input_dir, " /anime ");
        assert_eq!(draft.workers, "4x");
        assert!(draft.dry_run);
        assert_eq!(draft.edit(edit), Ok(false));
        assert_eq!(draft.edit(DraftChanges::default()), Ok(false));
        assert_eq!(draft.preset.as_deref(), Some("balanced"));
    }

    #[test]
    fn test_a_hand_edited_bitrate_or_complexity_makes_the_preset_custom() {
        for edit in [
            DraftChanges {
                bitrate: Some("256k".into()),
                ..DraftChanges::default()
            },
            DraftChanges {
                complexity: Some("9".into()),
                ..DraftChanges::default()
            },
        ] {
            let mut draft = remux();
            assert_eq!(draft.edit(edit), Ok(true));
            assert_eq!(draft.preset.as_deref(), Some("custom"));
        }
        // An edit that leaves the encode as it was leaves the preset too.
        let mut draft = remux();
        let same = DraftChanges {
            bitrate: Some("192k".into()),
            sub_offset: Some("1.5".into()),
            ..DraftChanges::default()
        };
        assert_eq!(draft.edit(same), Ok(true));
        assert_eq!(draft.preset.as_deref(), Some("balanced"));
    }

    #[test]
    fn test_set_preset_applies_only_the_catalogs_profiles() {
        let mut draft = remux();
        // Not in the catalog: nothing changes, and the error is the one a request answers.
        assert_eq!(
            draft.set_preset("studio"),
            Err("Unknown preset profile: 'studio'".into())
        );
        assert_eq!(draft, remux());

        assert_eq!(draft.set_preset("fast"), Ok(true));
        assert_eq!(draft.preset.as_deref(), Some("fast"));
        assert_eq!(draft.bitrate.as_deref(), Some("128k"));
        assert_eq!(draft.complexity.as_deref(), Some("5"));
        assert_eq!(draft.set_preset("hifi"), Ok(true));
        assert_eq!(draft.bitrate.as_deref(), Some("320k"));
        assert_eq!(draft.complexity.as_deref(), Some("10"));
        assert_eq!(draft.set_preset("hifi"), Ok(false));
        // An alias or another case is the profile a request resolves, under its canonical id.
        assert_eq!(draft.set_preset("High-Quality"), Ok(false));
        assert_eq!(draft.set_preset(" DEFAULT "), Ok(true));
        assert_eq!(draft.preset.as_deref(), Some("balanced"));
        assert_eq!(draft.bitrate.as_deref(), Some("192k"));
    }

    #[test]
    fn test_custom_only_relabels_the_form() {
        let mut draft = remux();
        draft.set_preset("hifi").unwrap();
        assert_eq!(draft.set_preset("custom"), Ok(true));
        assert_eq!(draft.preset.as_deref(), Some("custom"));
        // The catalog's custom row says 192k / 5, but the form keeps its own values.
        assert_eq!(draft.bitrate.as_deref(), Some("320k"));
        assert_eq!(draft.complexity.as_deref(), Some("10"));
        assert_eq!(draft.set_preset("custom"), Ok(false));
    }

    #[test]
    fn test_an_extract_draft_has_no_remux_settings() {
        let mut draft = SettingsDraft::new(PipelineMode::Extract);
        assert!(draft.set_preset("fast").is_err());
        let error = draft
            .edit(DraftChanges {
                out_dir: Some("/out".into()),
                ffmpeg_threads: Some("2".into()),
                ..DraftChanges::default()
            })
            .unwrap_err();
        assert_eq!(error, "Extract has no ffmpeg_threads setting.");
        // All or none: the out_dir of the refused edit is not applied either.
        assert_eq!(draft, SettingsDraft::new(PipelineMode::Extract));
    }

    #[test]
    fn test_draft_wire_shape() {
        assert_eq!(
            serde_json::to_value(remux()).unwrap(),
            serde_json::json!({
                "input_dir": "", "out_dir": "", "fonts_dir": "", "assets_dir": "",
                "preset": "balanced", "bitrate": "192k", "complexity": "5", "sub_offset": "0",
                "workers": "", "io_jobs": "", "ffmpeg_threads": "",
                "dry_run": false, "no_cache": false, "invalid": null
            })
        );
        assert_eq!(
            serde_json::to_value(SettingsDraft::new(PipelineMode::Extract)).unwrap(),
            serde_json::json!({
                "input_dir": "", "out_dir": "", "fonts_dir": "", "workers": "", "io_jobs": "",
                "dry_run": false, "no_cache": false, "invalid": null
            })
        );
        let changes: DraftChanges =
            serde_json::from_value(serde_json::json!({ "bitrate": "256k", "no_cache": true }))
                .unwrap();
        assert_eq!(
            changes,
            DraftChanges {
                bitrate: Some("256k".into()),
                no_cache: Some(true),
                ..DraftChanges::default()
            }
        );
        // The preset is `SetPreset`'s, and a misspelled field is no silent no-op.
        for json in [
            serde_json::json!({ "preset": "fast" }),
            serde_json::json!({ "bitrates": "256k" }),
        ] {
            assert!(serde_json::from_value::<DraftChanges>(json).is_err());
        }
    }

    /// A remux draft of `/anime` with `changes` applied.
    fn typed(changes: DraftChanges) -> SettingsDraft {
        let mut draft = remux();
        draft
            .edit(DraftChanges {
                input_dir: Some(" /anime ".into()),
                ..changes
            })
            .unwrap();
        draft
    }

    #[test]
    fn test_a_request_trims_every_value_and_unsets_the_empty_ones() {
        let draft = typed(DraftChanges {
            out_dir: Some(" /out ".into()),
            fonts_dir: Some("  ".into()),
            assets_dir: Some("/subs".into()),
            bitrate: Some(" 256k ".into()),
            complexity: Some(" 8 ".into()),
            sub_offset: Some(" 1.5 ".into()),
            workers: Some("6".into()),
            io_jobs: Some("0".into()),
            ffmpeg_threads: Some("4".into()),
            no_cache: Some(true),
            ..DraftChanges::default()
        });
        assert_eq!(
            draft.request(PipelineMode::Remux),
            Ok(BatchRequest {
                mode: PipelineMode::Remux,
                input_dir: "/anime".into(),
                out_dir: Some("/out".into()),
                fonts_dir: None,
                assets_dir: Some("/subs".into()),
                preset: Some("custom".into()),
                bitrate: Some("256k".into()),
                complexity: Some(8),
                // An explicit 0 is kept: the engine expands it to the worker count.
                io_jobs: Some(0),
                workers: Some(6),
                ffmpeg_threads: Some(4),
                default_sub_offset_ms: Some(1500),
                dry_run: Some(false),
                no_cache: Some(true),
                assignments: None,
                selected_items: None,
            })
        );
        // Empty counts are unset, so the engine's defaults apply.
        let request = typed(DraftChanges::default())
            .request(PipelineMode::Remux)
            .unwrap();
        assert_eq!(request.preset.as_deref(), Some("balanced"));
        assert_eq!((request.workers, request.io_jobs), (None, None));
        assert_eq!(request.ffmpeg_threads, None);
        // Extract has no remux settings, and sends no preset.
        let mut extract = SettingsDraft::new(PipelineMode::Extract);
        extract.input_dir = "/anime".into();
        let request = extract.request(PipelineMode::Extract).unwrap();
        assert_eq!(
            request,
            BatchRequest {
                mode: PipelineMode::Extract,
                input_dir: "/anime".into(),
                dry_run: Some(false),
                no_cache: Some(false),
                ..BatchRequest::default()
            }
        );
    }

    #[test]
    fn test_an_empty_input_directory_builds_no_request() {
        for input_dir in ["", "   "] {
            let mut draft = remux();
            draft.input_dir = input_dir.into();
            assert_eq!(
                draft.request(PipelineMode::Remux),
                Err(("input_dir", "Input directory is required.".into()))
            );
        }
    }

    #[test]
    fn test_the_sub_offset_is_whole_milliseconds_rounded_half_away_from_zero() {
        for (seconds, expected) in [
            ("1.5", Some(1500)),
            ("-0.5", Some(-500)),
            ("0", Some(0)),
            ("-0", Some(0)),
            ("", None),
            ("0.0005", Some(1)),
            ("-0.0005", Some(-1)),
            // 1e18 ms fits i64, so the CLI and now the desktop accept it.
            ("1e15", Some(1_000_000_000_000_000_000)),
            ("-1e15", Some(-1_000_000_000_000_000_000)),
        ] {
            let draft = typed(DraftChanges {
                sub_offset: Some(seconds.into()),
                ..DraftChanges::default()
            });
            let request = draft.request(PipelineMode::Remux).unwrap();
            assert_eq!(request.default_sub_offset_ms, expected, "{seconds:?}");
        }
    }

    #[test]
    fn test_a_sub_offset_that_is_no_number_of_seconds_in_range_builds_no_request() {
        for seconds in ["abc", "1e300", "1e400", "inf", "NaN"] {
            let draft = typed(DraftChanges {
                sub_offset: Some(format!(" {seconds} ")),
                ..DraftChanges::default()
            });
            let message = format!(
                "Invalid sub-offset value '{seconds}': must be a number of seconds within range."
            );
            assert_eq!(
                draft.request(PipelineMode::Remux),
                Err(("sub_offset", message))
            );
        }
    }

    #[test]
    fn test_a_count_that_is_no_whole_number_builds_no_request() {
        let cases = [
            ("complexity", "encoder complexity"),
            ("workers", "workers"),
            ("io_jobs", "Disk I/O"),
            ("ffmpeg_threads", "FFmpeg threads"),
        ];
        for (field, label) in cases {
            for raw in ["2.5", "-1", "abc", "1e3"] {
                let mut draft = typed(DraftChanges::default());
                let value = Some(raw.to_string());
                match field {
                    "complexity" => draft.complexity = value,
                    "workers" => draft.workers = raw.into(),
                    "io_jobs" => draft.io_jobs = raw.into(),
                    _ => draft.ffmpeg_threads = value,
                }
                let message = format!("Invalid {label} value '{raw}': must be a whole number.");
                assert_eq!(draft.request(PipelineMode::Remux), Err((field, message)));
            }
        }
    }

    #[test]
    fn test_the_flag_clears_on_the_next_edit_of_its_field_only() {
        let mut draft = typed(DraftChanges::default());
        draft.invalid = Some("workers");
        let other = DraftChanges {
            io_jobs: Some("2".into()),
            ..DraftChanges::default()
        };
        assert_eq!(draft.edit(other), Ok(true));
        assert_eq!(draft.invalid, Some("workers"));
        // Even an edit that leaves the value as it was clears it, and answers that it changed.
        let same = DraftChanges {
            workers: Some(String::new()),
            ..DraftChanges::default()
        };
        assert_eq!(draft.edit(same.clone()), Ok(true));
        assert_eq!(draft.invalid, None);
        assert_eq!(draft.edit(same), Ok(false));

        // An edit of a flagged complexity clears it too.
        draft.invalid = Some("complexity");
        let complexity = DraftChanges {
            complexity: Some("5".into()),
            ..DraftChanges::default()
        };
        assert_eq!(draft.edit(complexity), Ok(true));
        assert_eq!(draft.invalid, None);

        // A profile replaces a flagged complexity; custom keeps it.
        draft.invalid = Some("complexity");
        assert_eq!(draft.set_preset("custom"), Ok(true));
        assert_eq!(draft.invalid, Some("complexity"));
        assert_eq!(draft.set_preset("custom"), Ok(false));
        assert_eq!(draft.set_preset("balanced"), Ok(true));
        assert_eq!(draft.invalid, None);
    }

    // The webview's former complexity normalization (app.js `normalizeNumber`).
    #[test]
    fn test_an_edit_clamps_the_complexity_to_the_engines_range() {
        for (raw, expected) in [
            ("7", "7"),
            ("15", "10"),
            ("-3", "0"),
            ("-0", "0"),
            ("", "0"),
            ("  ", "0"),
            ("abc", "0"),
            ("-", "0"),
            // parseInt reads the leading integer and ignores the rest.
            (" 8 ", "8"),
            ("\u{feff}\u{a0}9", "9"),
            ("+4", "4"),
            ("2.9", "2"),
            ("1e3", "1"),
            ("06", "6"),
            ("0x10", "0"),
            ("99999999999999999999", "10"),
            ("-99999999999999999999", "0"),
        ] {
            let mut draft = remux();
            draft
                .edit(DraftChanges {
                    complexity: Some(raw.into()),
                    ..DraftChanges::default()
                })
                .unwrap();
            assert_eq!(draft.complexity.as_deref(), Some(expected), "{raw:?}");
        }
        // A value the clamp keeps as it was changes nothing, so the preset stays.
        let mut draft = remux();
        let same = DraftChanges {
            complexity: Some("05".into()),
            ..DraftChanges::default()
        };
        assert_eq!(draft.edit(same), Ok(false));
        assert_eq!(draft.preset.as_deref(), Some("balanced"));
    }

    /// `next_step_value` with `step` in `dir` and the bounds `min..=max`.
    fn stepped(
        current: &str,
        auto: Option<f64>,
        dir: f64,
        step: f64,
        range: Option<(f64, f64)>,
    ) -> f64 {
        next_step_value(current, auto, dir * step, range)
    }

    const WORKERS: Option<(f64, f64)> = Some((1.0, 64.0));

    // The webview's former stepper cases.
    #[test]
    fn test_an_empty_field_with_an_auto_value_starts_from_it() {
        assert_eq!(stepped("", Some(6.0), 1.0, 1.0, WORKERS), 7.0);
        assert_eq!(stepped("", Some(6.0), -1.0, 1.0, WORKERS), 5.0);
        // A number input reports an unparsable entry as "", and a lone sign parses as nothing too.
        assert_eq!(stepped("-", Some(6.0), 1.0, 1.0, WORKERS), 7.0);
        // Rust parses "inf"; a value that is not finite is no number either.
        assert_eq!(stepped("inf", Some(6.0), 1.0, 1.0, WORKERS), 7.0);
        assert_eq!(stepped("-infinity", None, 1.0, 0.5, None), 0.5);
    }

    #[test]
    fn test_an_empty_field_without_an_auto_value_starts_from_0() {
        let io_jobs = Some((0.0, 64.0));
        let threads = Some((1.0, 32.0));
        assert_eq!(stepped("", None, 1.0, 1.0, io_jobs), 1.0);
        assert_eq!(stepped("", None, -1.0, 1.0, io_jobs), 0.0);
        // FFmpeg threads (min 1): + lands on the minimum, - is clamped to it.
        assert_eq!(stepped("", None, 1.0, 1.0, threads), 1.0);
        assert_eq!(stepped("", None, -1.0, 1.0, threads), 1.0);
        assert_eq!(stepped("", None, 1.0, 0.5, None), 0.5);
    }

    #[test]
    fn test_a_value_steps_from_itself_and_ignores_the_auto_value() {
        assert_eq!(stepped("4", Some(6.0), 1.0, 1.0, WORKERS), 5.0);
        assert_eq!(stepped("4", Some(6.0), -1.0, 1.0, WORKERS), 3.0);
        // A typed 0 is a value: the auto value does not replace it.
        assert_eq!(stepped("0", Some(6.0), 1.0, 1.0, Some((0.0, 64.0))), 1.0);
        assert_eq!(stepped("0", Some(6.0), -1.0, 1.0, Some((0.0, 64.0))), 0.0);
    }

    #[test]
    fn test_a_step_is_clamped_to_the_bounds() {
        assert_eq!(stepped("64", Some(6.0), 1.0, 1.0, WORKERS), 64.0);
        assert_eq!(stepped("1", Some(6.0), -1.0, 1.0, WORKERS), 1.0);
        assert_eq!(stepped("", Some(1.0), -1.0, 1.0, WORKERS), 1.0);
        assert_eq!(stepped("", Some(64.0), 1.0, 1.0, WORKERS), 64.0);
        // A typed value outside the bounds is pulled in, not just stepped.
        assert_eq!(stepped("100", Some(6.0), 1.0, 1.0, WORKERS), 64.0);
        assert_eq!(stepped("-5", Some(6.0), -1.0, 1.0, WORKERS), 1.0);
    }

    #[test]
    fn test_no_bounds_leave_a_step_unbounded_and_fractions_drop_their_noise() {
        assert_eq!(stepped("-3", None, -1.0, 1.0, None), -4.0);
        assert_eq!(stepped("1000", None, 1.0, 1.0, None), 1001.0);
        // Rounded to ten significant digits like toPrecision(10); the two could differ only on an
        // exact binary tie at the tenth digit, which steps of 0.5 or 1 from typed values never meet.
        assert_eq!(stepped("0.2", None, 1.0, 0.1, None), 0.3);
        assert_eq!(stepped("0.3", None, -1.0, 0.1, None), 0.2);
        assert_eq!(stepped("-0.5", None, 1.0, 0.5, None).to_string(), "0");
    }

    #[test]
    fn test_a_step_edits_the_field_within_the_engines_range_from_the_published_defaults() {
        let defaults = EffectiveSettings {
            workers: 6,
            io_jobs: 4,
            ffmpeg_threads: 2,
            bitrate: "192k".into(),
            complexity: 5,
        };
        let mut draft = remux();
        assert_eq!(
            draft.step(StepField::Workers, Direction::Up, Some(&defaults)),
            Ok(true)
        );
        assert_eq!(draft.workers, "7");
        assert_eq!(
            draft.step(StepField::IoJobs, Direction::Down, Some(&defaults)),
            Ok(true)
        );
        assert_eq!(draft.io_jobs, "3");
        // No auto value for FFmpeg threads: from 0, up to the minimum of 1.
        draft
            .step(StepField::FfmpegThreads, Direction::Up, Some(&defaults))
            .unwrap();
        assert_eq!(draft.ffmpeg_threads.as_deref(), Some("1"));
        assert_eq!(
            draft.step(StepField::FfmpegThreads, Direction::Down, None),
            Ok(false)
        );
        // The subtitle offset steps by half a second, unbounded.
        for _ in 0..3 {
            draft
                .step(StepField::SubOffset, Direction::Down, None)
                .unwrap();
        }
        assert_eq!(draft.sub_offset.as_deref(), Some("-1.5"));
        draft.workers = "64".into();
        assert_eq!(
            draft.step(StepField::Workers, Direction::Up, None),
            Ok(false)
        );
        // A step is an edit: it clears the flag of the field it steps.
        draft.invalid = Some("workers");
        draft.workers = "x".into();
        assert_eq!(
            draft.step(StepField::Workers, Direction::Up, None),
            Ok(true)
        );
        assert_eq!((draft.workers.as_str(), draft.invalid), ("1", None));

        let mut extract = SettingsDraft::new(PipelineMode::Extract);
        assert_eq!(
            extract.step(StepField::SubOffset, Direction::Up, None),
            Err("Extract has no sub_offset setting.".into())
        );
        assert_eq!(
            extract.step(StepField::Workers, Direction::Up, Some(&defaults)),
            Ok(true)
        );
        assert_eq!(extract.workers, "7");
    }
}
