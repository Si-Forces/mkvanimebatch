//! The webview transport's diff of two View States (ADR-0016, S4 amendment). The core publishes
//! the whole View State; the webview adapter sends only what changed since its last emission.
//!
//! A patch is `{seq, base, ...the top-level keys whose value changed}`, `base` being the `seq`
//! of the emission it applies to. A changed key carries its whole new value, `null` included.
//! `remux` and `extract` go one level deeper: a changed mode carries only its changed ModeView
//! keys, and its rows go either as `rows` (the whole list, when the mode's `manifest_rev`, its
//! row count or the id at any index changed) or as `row_changes`, `[[index, Row], ...]` by
//! ascending index. An unchanged mode is absent. With nothing changed the patch is
//! `{seq, base}`, so a view sees every `seq`.

use mkv_desktop_core::actor::{ModeView, ViewState};
use serde::Serialize;
use serde_json::{Map, Result, Value};
use std::sync::Arc;

/// The patch that takes a view holding `prev` to `next`.
pub(crate) fn view_patch(prev: &ViewState, next: &ViewState) -> Result<Value> {
    // Destructured, so a new ViewState field fails to compile until it is diffed here.
    let ViewState {
        seq,
        job,
        run,
        busy,
        defaults,
        presets,
        remux,
        extract,
        telemetry,
    } = next;
    let mut patch = Map::new();
    patch.insert("seq".into(), Value::from(*seq));
    patch.insert("base".into(), Value::from(prev.seq));
    changed(&mut patch, "job", &prev.job, job)?;
    changed(&mut patch, "run", &prev.run, run)?;
    changed(&mut patch, "busy", &prev.busy, busy)?;
    changed(&mut patch, "defaults", &prev.defaults, defaults)?;
    changed(&mut patch, "presets", &prev.presets, presets)?;
    for (key, prev, next) in [
        ("remux", &prev.remux, remux),
        ("extract", &prev.extract, extract),
    ] {
        if let Some(mode) = mode_patch(prev, next)? {
            patch.insert(key.into(), mode);
        }
    }
    changed(&mut patch, "telemetry", &prev.telemetry, telemetry)?;
    Ok(Value::Object(patch))
}

/// The changed keys of one mode, or `None` when nothing in it changed.
fn mode_patch(prev: &ModeView, next: &ModeView) -> Result<Option<Value>> {
    let ModeView {
        directory,
        scan,
        manifest_rev,
        subtitle_candidates,
        rows,
        vanished_selected_ids,
        assignments,
        draft,
        outcome,
    } = next;
    let mut patch = Map::new();
    changed(&mut patch, "directory", &prev.directory, directory)?;
    changed(&mut patch, "scan", &prev.scan, scan)?;
    changed(&mut patch, "manifest_rev", &prev.manifest_rev, manifest_rev)?;
    changed(
        &mut patch,
        "subtitle_candidates",
        &prev.subtitle_candidates,
        subtitle_candidates,
    )?;
    // The actor reuses the rows of a mode it did not rebuild, so most publications stop here.
    if !Arc::ptr_eq(&prev.rows, rows) {
        let replaced = prev.manifest_rev != *manifest_rev
            || prev.rows.len() != rows.len()
            || prev.rows.iter().zip(rows.iter()).any(|(a, b)| a.id != b.id);
        if replaced {
            patch.insert("rows".into(), serde_json::to_value(rows)?);
        } else {
            let changes: Vec<_> = prev
                .rows
                .iter()
                .zip(rows.iter())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(index, (_, row))| (index, row))
                .collect();
            if !changes.is_empty() {
                patch.insert("row_changes".into(), serde_json::to_value(changes)?);
            }
        }
    }
    changed(
        &mut patch,
        "vanished_selected_ids",
        &prev.vanished_selected_ids,
        vanished_selected_ids,
    )?;
    changed(&mut patch, "assignments", &prev.assignments, assignments)?;
    changed(&mut patch, "draft", &prev.draft, draft)?;
    changed(&mut patch, "outcome", &prev.outcome, outcome)?;
    Ok((!patch.is_empty()).then_some(Value::Object(patch)))
}

/// Puts `next` under `key` when it differs from `prev`.
fn changed<T: Serialize + PartialEq>(
    patch: &mut Map<String, Value>,
    key: &str,
    prev: &T,
    next: &T,
) -> Result<()> {
    if prev != next {
        patch.insert(key.into(), serde_json::to_value(next)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mkv_core::request::EffectiveSettings;
    use mkv_desktop_core::actor::{JobView, Outcome, ScanView};
    use mkv_desktop_core::coordinator::Phase;
    use mkv_desktop_core::queue::Row;
    use mkv_desktop_core::snapshot::{BatchStatus, ItemStatus};
    use serde_json::json;

    fn row(id: &str) -> Row {
        Row {
            id: id.into(),
            name: format!("{id}.mkv"),
            release: ".".into(),
            metadata: None,
            inspection_error: None,
            in_manifest: true,
            selected: true,
            status: ItemStatus::Queued,
            progress: None,
            start_time_ms: None,
            duration_sec: None,
            reason: None,
            error: None,
            participant: false,
        }
    }

    /// A state at `seq` with the remux queue holding `ids` under manifest 1.
    fn state(seq: u64, ids: &[&str]) -> ViewState {
        let mut state = ViewState::new(None);
        state.seq = seq;
        state.remux.manifest_rev = 1;
        state.remux.rows = Arc::new(ids.iter().map(|id| row(id)).collect());
        state
    }

    /// `prev` as the actor publishes it next: the same rows, `seq` one higher.
    fn next(prev: &ViewState) -> ViewState {
        ViewState {
            seq: prev.seq + 1,
            ..prev.clone()
        }
    }

    fn patch(prev: &ViewState, next: &ViewState) -> Value {
        view_patch(prev, next).unwrap()
    }

    fn job() -> JobView {
        JobView {
            job_id: 1,
            mode: "remux".into(),
            status: BatchStatus::Running,
            active: true,
            start_time_ms: 1_700_000_000_000,
            input_dir: "/anime".into(),
            effective_settings: EffectiveSettings::defaults(),
            participants: vec!["ep1".into(), "ep2".into()],
        }
    }

    /// Applies `payload` to `applied` by the rules the webview applies it by: a payload without
    /// `base` replaces the state; a patch replaces each key it carries, and inside a mode each
    /// key, `row_changes` writing each row at its index.
    fn merge(applied: &Value, payload: &Value) -> Value {
        let Some(base) = payload.get("base") else {
            return payload.clone();
        };
        assert_eq!(
            *base, applied["seq"],
            "a patch applies to the state of its base"
        );
        let mut state = applied.as_object().unwrap().clone();
        for (key, value) in payload.as_object().unwrap() {
            match key.as_str() {
                "base" => {}
                "remux" | "extract" => {
                    let mode = state.get_mut(key).unwrap().as_object_mut().unwrap();
                    for (field, value) in value.as_object().unwrap() {
                        if field == "row_changes" {
                            let rows = mode["rows"].as_array_mut().unwrap();
                            for change in value.as_array().unwrap() {
                                let index = change[0].as_u64().unwrap() as usize;
                                rows[index] = change[1].clone();
                            }
                        } else {
                            mode.insert(field.clone(), value.clone());
                        }
                    }
                }
                _ => {
                    state.insert(key.clone(), value.clone());
                }
            }
        }
        Value::Object(state)
    }

    #[test]
    fn test_nothing_changed_is_only_seq_and_base_even_with_rebuilt_rows() {
        let prev = state(4, &["ep1", "ep2"]);
        let mut same = next(&prev);
        assert_eq!(patch(&prev, &same), json!({ "seq": 5, "base": 4 }));
        // A mode rebuilt into equal rows is still unchanged.
        same.remux.rows = Arc::new((*prev.remux.rows).clone());
        assert_eq!(patch(&prev, &same), json!({ "seq": 5, "base": 4 }));
    }

    #[test]
    fn test_a_changed_top_level_key_carries_its_whole_value_and_null_is_sent() {
        let prev = state(1, &["ep1"]);
        let mut running = next(&prev);
        running.run.phase = Phase::Running;
        running.busy = true;
        running.job = Some(job());
        assert_eq!(
            patch(&prev, &running),
            json!({
                "seq": 2,
                "base": 1,
                "run": serde_json::to_value(&running.run).unwrap(),
                "busy": true,
                "job": serde_json::to_value(&running.job).unwrap(),
            })
        );
        let mut ended = next(&running);
        ended.job = None;
        assert_eq!(
            patch(&running, &ended),
            json!({ "seq": 3, "base": 2, "job": null })
        );
    }

    #[test]
    fn test_one_rows_change_goes_by_its_index_and_the_untouched_mode_is_absent() {
        let prev = state(1, &["ep1", "ep2", "ep3"]);
        let mut progressed = next(&prev);
        let mut rows = (*prev.remux.rows).clone();
        rows[1].status = ItemStatus::Processing;
        rows[1].progress = Some(40.0);
        progressed.remux.rows = Arc::new(rows);
        assert_eq!(
            patch(&prev, &progressed),
            json!({
                "seq": 2,
                "base": 1,
                "remux": { "row_changes": [[1, serde_json::to_value(&progressed.remux.rows[1]).unwrap()]] },
            })
        );
    }

    #[test]
    fn test_rows_go_whole_on_a_new_manifest_a_new_length_or_a_new_id_order() {
        let prev = state(1, &["ep1", "ep2"]);
        let rows =
            |state: &ViewState| json!({ "rows": serde_json::to_value(&state.remux.rows).unwrap() });

        let mut manifest = next(&prev);
        manifest.remux.manifest_rev = 2;
        manifest.remux.rows = Arc::new(vec![row("ep1"), row("ep2")]);
        let mut expected = rows(&manifest);
        expected["manifest_rev"] = json!(2);
        assert_eq!(patch(&prev, &manifest)["remux"], expected);

        let mut longer = next(&prev);
        longer.remux.rows = Arc::new(vec![row("ep1"), row("ep2"), row("ep3")]);
        assert_eq!(patch(&prev, &longer)["remux"], rows(&longer));

        let mut reordered = next(&prev);
        reordered.remux.rows = Arc::new(vec![row("ep2"), row("ep1")]);
        assert_eq!(patch(&prev, &reordered)["remux"], rows(&reordered));
    }

    #[test]
    fn test_a_changed_mode_key_other_than_rows_goes_alone() {
        let prev = state(1, &["ep1"]);
        let mut failed = next(&prev);
        failed.extract.scan = ScanView::Failed;
        failed.extract.outcome = Some(Outcome::Failed);
        assert_eq!(
            patch(&prev, &failed),
            json!({
                "seq": 2,
                "base": 1,
                "extract": { "scan": { "state": "failed" }, "outcome": "failed" },
            })
        );
    }

    #[test]
    fn test_each_patch_merged_into_the_previous_state_is_the_next_state() {
        // A scan, a start, a job's progress and its end, as the actor publishes them.
        let empty = ViewState::new(Some(EffectiveSettings::defaults()));
        let mut scanning = next(&empty);
        scanning.remux.directory = Some("/anime".into());
        scanning.remux.scan = ScanView::Scanning {
            directory: "/anime".into(),
        };
        let mut scanned = next(&scanning);
        scanned.remux.scan = ScanView::Idle;
        scanned.remux.manifest_rev = 1;
        scanned.remux.rows = Arc::new(vec![row("ep1"), row("ep2"), row("ep3")]);
        let mut starting = next(&scanned);
        starting.run.phase = Phase::Starting;
        starting.busy = true;
        let mut running = next(&starting);
        running.run.phase = Phase::Running;
        running.job = Some(job());
        let mut rows = (*scanned.remux.rows).clone();
        for row in &mut rows[..2] {
            row.participant = true;
        }
        rows[0].status = ItemStatus::Processing;
        running.remux.rows = Arc::new(rows.clone());
        let mut progressed = next(&running);
        rows[0].status = ItemStatus::Completed;
        rows[1].status = ItemStatus::Processing;
        rows[1].progress = Some(12.5);
        progressed.remux.rows = Arc::new(rows);
        let mut ended = next(&progressed);
        ended.run.phase = Phase::Idle;
        ended.busy = false;
        ended.job.as_mut().unwrap().active = false;
        ended.remux.outcome = Some(Outcome::Done);
        let mut reset = next(&ended);
        reset.job = None;
        reset.remux.rows = Arc::new(Vec::new());
        reset.remux.manifest_rev = 2;

        let states = [
            empty, scanning, scanned, starting, running, progressed, ended, reset,
        ];
        let mut applied = serde_json::to_value(&states[0]).unwrap();
        for pair in states.windows(2) {
            applied = merge(&applied, &patch(&pair[0], &pair[1]));
            assert_eq!(applied, serde_json::to_value(&pair[1]).unwrap());
        }
    }
}
