//! The Settings Draft of each mode (ADR-0016): the values of its settings form, held as typed, so a
//! view only mirrors them. Edits arrive as Intents; nothing is parsed until a run reads them, and
//! nothing is persisted.

use mkv_core::pipeline::PipelineMode;
use mkv_core::preset::require_preset;
use mkv_core::request::DEFAULT_PRESET_ID;
use serde::{Deserialize, Serialize};

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
}

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
        }
    }

    /// Applies `changes`, all or none: true when a field changed. A changed bitrate or complexity
    /// is a hand-edited encode, so the preset becomes custom. A field the mode does not have is an
    /// error.
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

        let mut changed = false;
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
        }
        Ok(changed)
    }
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
                "dry_run": false, "no_cache": false
            })
        );
        assert_eq!(
            serde_json::to_value(SettingsDraft::new(PipelineMode::Extract)).unwrap(),
            serde_json::json!({
                "input_dir": "", "out_dir": "", "fonts_dir": "", "workers": "", "io_jobs": "",
                "dry_run": false, "no_cache": false
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
}
