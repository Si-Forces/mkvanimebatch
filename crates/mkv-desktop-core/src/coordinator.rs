//! The Job Coordinator (ADR-0016): the lifecycle of the one job the app runs at a time, across
//! both modes, and the preflight that turns the Queue Selection a Run captured, checked against
//! the forced scan that followed it, into the batch the job runs. The Desktop Core actor drives
//! both; neither does any I/O.

use crate::queue::{is_same_directory, Queue};
use mkv_core::pipeline::PipelineMode;
use mkv_core::request::TrackAssignment;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// Where the one job stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Nothing owns the batch.
    #[default]
    Idle,
    /// A mode's Run is resolving its request, scanning or launching.
    Starting,
    /// The job was launched.
    Running,
    /// A Stop was requested and teardown is pending.
    Stopping,
}

/// The phase and the mode that owns the batch while it is not idle. Only the named transitions
/// move it; each answers whether it applied, and a refused one changes nothing.
#[derive(Debug, Default)]
pub struct JobCoordinator {
    phase: Phase,
    owner: Option<PipelineMode>,
}

impl JobCoordinator {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The mode that owns the batch while starting, running or stopping.
    pub fn owner(&self) -> Option<PipelineMode> {
        self.owner
    }

    /// Moves from one of the `from` phases to `to`, keeping the owner.
    fn shift(&mut self, from: &[Phase], to: Phase) -> bool {
        if !from.contains(&self.phase) {
            return false;
        }
        self.phase = to;
        true
    }

    pub fn begin(&mut self, mode: PipelineMode) -> bool {
        let begun = self.shift(&[Phase::Idle], Phase::Starting);
        if begun {
            self.owner = Some(mode);
        }
        begun
    }

    /// A stopped start does not launch: it stays stopping.
    pub fn launched(&mut self) -> bool {
        self.shift(&[Phase::Starting], Phase::Running)
    }

    pub fn request_stop(&mut self) -> bool {
        self.shift(&[Phase::Starting, Phase::Running], Phase::Stopping)
    }

    pub fn stop_failed(&mut self) -> bool {
        self.shift(&[Phase::Stopping], Phase::Running)
    }

    /// Releases the batch from any phase, so a repeat is harmless.
    pub fn finish(&mut self) {
        self.phase = Phase::Idle;
        self.owner = None;
    }
}

/// The queue of a mode as a Run found it, before its forced scan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Captured {
    pub directory: Option<String>,
    /// The Queue Selection, in queue order.
    pub selected: Vec<String>,
    pub vanished: Vec<String>,
    pub assignments: BTreeMap<String, TrackAssignment>,
}

impl Captured {
    pub fn of(queue: &Queue) -> Self {
        Self {
            directory: queue.directory().map(str::to_string),
            selected: queue
                .selected_ids()
                .into_iter()
                .map(str::to_string)
                .collect(),
            vanished: queue.vanished_selected_ids().to_vec(),
            assignments: queue.assignments().clone(),
        }
    }
}

/// What a job runs, from preflight: its `selected_items` and `assignments`.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub selected_items: Vec<String>,
    /// `None` for extract, or when no selected item the scan found has one.
    pub assignments: Option<Vec<TrackAssignment>>,
}

/// Checks the Queue Selection `captured` before the forced scan against `queue` after it. The
/// same directory (byte for byte) runs the captured selection and Track Assignments, but refuses
/// when a selected item vanished, now or in an earlier rescan, or reappeared during the scan;
/// another directory runs what the scan selected, which is all of it.
pub fn preflight(mode: PipelineMode, captured: &Captured, queue: &Queue) -> Result<Batch, String> {
    let same = is_same_directory(captured.directory.as_deref(), queue.directory());
    let selected: Vec<String> = if same {
        captured.selected.clone()
    } else {
        queue
            .selected_ids()
            .into_iter()
            .map(str::to_string)
            .collect()
    };
    if selected.is_empty() {
        return Err("No episodes selected in queue.".into());
    }
    if same {
        let reappeared: Vec<&str> = captured
            .vanished
            .iter()
            .map(String::as_str)
            .filter(|id| queue.contains(id))
            .collect();
        let mut seen = HashSet::new();
        let missing: Vec<&str> = selected
            .iter()
            .map(String::as_str)
            .filter(|id| !queue.contains(id))
            .chain(queue.vanished_selected_ids().iter().map(String::as_str))
            .chain(captured.vanished.iter().map(String::as_str))
            .filter(|id| seen.insert(*id))
            .collect();
        if !reappeared.is_empty() {
            return Err(format!(
                "Queue selection changed during inspection; previously missing selected episodes reappeared: {}. Review the queue and press Run again; job was not started.",
                reappeared.join(", ")
            ));
        }
        if !missing.is_empty() {
            return Err(format!(
                "Selected episodes disappeared from the input directory: {}. Change the queue selection to continue; job was not started.",
                missing.join(", ")
            ));
        }
    }
    let assignments = (mode == PipelineMode::Remux)
        .then(|| {
            let source = if same {
                &captured.assignments
            } else {
                queue.assignments()
            };
            let selected: HashSet<&str> = selected.iter().map(String::as_str).collect();
            // Only those of the items the scan still found, and of those the selected ones.
            source
                .values()
                .filter(|assignment| {
                    queue.contains(&assignment.target_video)
                        && selected.contains(assignment.target_video.as_str())
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|assignments| !assignments.is_empty());
    Ok(Batch {
        selected_items: selected,
        assignments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::test_support::manifest_of;

    const PHASES: [Phase; 4] = [
        Phase::Idle,
        Phase::Starting,
        Phase::Running,
        Phase::Stopping,
    ];

    /// A coordinator moved into `phase`, owned by remux where a mode owns the batch.
    fn in_phase(phase: Phase) -> JobCoordinator {
        let mut coordinator = JobCoordinator::default();
        if phase != Phase::Idle {
            coordinator.begin(PipelineMode::Remux);
        }
        if matches!(phase, Phase::Running | Phase::Stopping) {
            coordinator.launched();
        }
        if phase == Phase::Stopping {
            coordinator.request_stop();
        }
        assert_eq!(coordinator.phase(), phase);
        coordinator
    }

    #[test]
    fn test_only_one_mode_owns_the_batch() {
        let mut coordinator = JobCoordinator::default();
        assert!(coordinator.begin(PipelineMode::Remux));
        assert!(!coordinator.begin(PipelineMode::Extract));
        assert_eq!(coordinator.owner(), Some(PipelineMode::Remux));
        coordinator.finish();
        assert!(coordinator.begin(PipelineMode::Extract));
    }

    #[test]
    fn test_a_job_walks_idle_starting_running_stopping_and_back_to_idle() {
        let mut coordinator = JobCoordinator::default();
        assert_eq!(
            (coordinator.phase(), coordinator.owner()),
            (Phase::Idle, None)
        );
        assert!(coordinator.begin(PipelineMode::Remux));
        assert_eq!(coordinator.phase(), Phase::Starting);
        assert!(coordinator.launched());
        assert_eq!(coordinator.phase(), Phase::Running);
        assert!(coordinator.request_stop());
        assert_eq!(coordinator.phase(), Phase::Stopping);
        // The mode keeps the batch until the job ends.
        assert_eq!(coordinator.owner(), Some(PipelineMode::Remux));
        coordinator.finish();
        assert_eq!(
            (coordinator.phase(), coordinator.owner()),
            (Phase::Idle, None)
        );
    }

    #[test]
    fn test_a_stop_during_starting_moves_straight_to_stopping_and_cannot_launch() {
        let mut coordinator = in_phase(Phase::Starting);
        assert!(coordinator.request_stop());
        assert!(!coordinator.launched());
        assert_eq!(coordinator.phase(), Phase::Stopping);
    }

    #[test]
    fn test_a_failed_stop_returns_to_running_and_keeps_the_mode() {
        let mut coordinator = in_phase(Phase::Stopping);
        assert!(coordinator.stop_failed());
        assert_eq!(coordinator.phase(), Phase::Running);
        assert_eq!(coordinator.owner(), Some(PipelineMode::Remux));
        assert!(
            coordinator.request_stop(),
            "the stop can be asked for again"
        );
    }

    #[test]
    fn test_each_transition_is_refused_from_every_other_phase_and_changes_nothing() {
        type Transition = fn(&mut JobCoordinator) -> bool;
        let transitions: [(&str, Transition, &[Phase]); 4] = [
            ("begin", |c| c.begin(PipelineMode::Extract), &[Phase::Idle]),
            ("launched", |c| c.launched(), &[Phase::Starting]),
            (
                "request_stop",
                |c| c.request_stop(),
                &[Phase::Starting, Phase::Running],
            ),
            ("stop_failed", |c| c.stop_failed(), &[Phase::Stopping]),
        ];
        for (name, transition, legal) in transitions {
            for phase in PHASES.into_iter().filter(|phase| !legal.contains(phase)) {
                let mut coordinator = in_phase(phase);
                let owner = coordinator.owner();
                assert!(!transition(&mut coordinator), "{name} from {phase:?}");
                assert_eq!(coordinator.phase(), phase, "{name} from {phase:?}");
                assert_eq!(coordinator.owner(), owner, "{name} from {phase:?}");
            }
        }
    }

    #[test]
    fn test_finish_ends_in_idle_from_every_phase_and_is_idempotent() {
        for phase in PHASES {
            let mut coordinator = in_phase(phase);
            coordinator.finish();
            coordinator.finish();
            assert_eq!(
                (coordinator.phase(), coordinator.owner()),
                (Phase::Idle, None)
            );
            assert!(coordinator.begin(PipelineMode::Remux), "{phase:?}");
        }
    }

    fn subtitle(id: &str) -> TrackAssignment {
        TrackAssignment {
            target_video: id.into(),
            asset_path: format!("/subs/{id}.ass").into(),
            track_type: Default::default(),
            language: None,
            title: None,
            offset_ms: None,
        }
    }

    /// A queue that accepted `ids` as the inspection of `directory`.
    fn scanned(directory: &str, ids: &[&str]) -> Queue {
        let mut queue = Queue::default();
        assert!(queue.accept(manifest_of(ids), directory.into()));
        queue
    }

    #[test]
    fn test_the_same_directory_runs_the_captured_selection_and_its_assignments() {
        let mut queue = scanned("/anime", &["ep1", "ep2", "ep3"]);
        queue.set_selected("ep3", false);
        for id in ["ep1", "ep2", "ep3"] {
            queue.assign(subtitle(id));
        }
        let captured = Captured::of(&queue);
        // What the scan changed meanwhile does not reach the run.
        queue.clear_assignment("ep1");
        queue.set_selected("ep2", false);

        let batch = preflight(PipelineMode::Remux, &captured, &queue).unwrap();
        assert_eq!(batch.selected_items, ["ep1", "ep2"]);
        let targets: Vec<_> = batch.assignments.unwrap();
        assert_eq!(targets, [subtitle("ep1"), subtitle("ep2")]);
        // Extract sends none.
        let batch = preflight(PipelineMode::Extract, &captured, &queue).unwrap();
        assert_eq!(batch.assignments, None);
    }

    #[test]
    fn test_another_directory_runs_all_it_found_and_drops_the_old_assignments() {
        let mut old = scanned("/anime/a", &["ep1", "ep2"]);
        old.set_selected("ep2", false);
        old.assign(subtitle("ep1"));
        let captured = Captured::of(&old);
        let batch = preflight(
            PipelineMode::Remux,
            &captured,
            &scanned("/anime/b", &["ep1", "ep2"]),
        )
        .unwrap();
        assert_eq!(
            batch,
            Batch {
                selected_items: vec!["ep1".into(), "ep2".into()],
                assignments: None,
            }
        );
        // An empty queue before the scan is another directory too.
        let batch = preflight(
            PipelineMode::Remux,
            &Captured::default(),
            &scanned("/anime/b", &["ep1"]),
        )
        .unwrap();
        assert_eq!(batch.selected_items, ["ep1"]);
    }

    #[test]
    fn test_an_assignment_of_no_selected_item_is_none() {
        let mut queue = scanned("/anime", &["ep1", "ep2"]);
        queue.assign(subtitle("ep2"));
        queue.set_selected("ep2", false);
        let batch = preflight(PipelineMode::Remux, &Captured::of(&queue), &queue).unwrap();
        assert_eq!(batch.assignments, None);
    }

    #[test]
    fn test_nothing_selected_is_refused() {
        let mut queue = scanned("/anime", &["ep1"]);
        queue.set_all_selected(false);
        let error = preflight(PipelineMode::Remux, &Captured::of(&queue), &queue).unwrap_err();
        assert_eq!(error, "No episodes selected in queue.");
        // A scan of another directory that found nothing selects nothing.
        let error = preflight(
            PipelineMode::Remux,
            &Captured::default(),
            &scanned("/b", &[]),
        );
        assert_eq!(error.unwrap_err(), "No episodes selected in queue.");
    }

    #[test]
    fn test_a_selected_item_the_scan_lost_is_refused_by_name() {
        let before = scanned("/anime", &["ep1", "ep2"]);
        let captured = Captured::of(&before);
        let mut after = before;
        assert!(after.accept(manifest_of(&["ep1"]), "/anime".into()));
        let error = preflight(PipelineMode::Remux, &captured, &after).unwrap_err();
        assert_eq!(
            error,
            "Selected episodes disappeared from the input directory: ep2. Change the queue selection to continue; job was not started."
        );
    }

    #[test]
    fn test_a_selection_that_vanished_in_an_earlier_rescan_is_refused() {
        let mut queue = scanned("/anime", &["ep1", "ep2"]);
        assert!(queue.accept(manifest_of(&["ep1"]), "/anime".into()));
        assert_eq!(queue.vanished_selected_ids(), ["ep2"]);
        let error = preflight(PipelineMode::Remux, &Captured::of(&queue), &queue).unwrap_err();
        assert!(
            error.contains("disappeared from the input directory: ep2."),
            "{error}"
        );
    }

    #[test]
    fn test_a_vanished_selection_that_reappears_during_the_scan_is_refused() {
        let mut queue = scanned("/anime", &["ep1", "ep2"]);
        assert!(queue.accept(manifest_of(&["ep1"]), "/anime".into()));
        let captured = Captured::of(&queue);
        assert!(queue.accept(manifest_of(&["ep1", "ep2"]), "/anime".into()));
        let error = preflight(PipelineMode::Remux, &captured, &queue).unwrap_err();
        assert_eq!(
            error,
            "Queue selection changed during inspection; previously missing selected episodes reappeared: ep2. Review the queue and press Run again; job was not started."
        );
    }
}
