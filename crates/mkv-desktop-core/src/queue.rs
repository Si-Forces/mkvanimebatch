//! The Queue Tracker's model, one per mode: the accepted Inspection Manifest, the Queue Selection
//! and its order, and the Track Assignments. `rows` lays a Job Snapshot's participants over the
//! manifest order, which is what a view renders. Directory Identities are compared byte for byte:
//! the Engine Bridge produces every one in display-canonical form and nothing here normalizes a
//! path.

use crate::snapshot::{ItemStatus, JobSnapshot};
use mkv_core::manifest::{InspectionManifest, InspectionMetadata};
use mkv_core::request::TrackAssignment;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// The only Inspection Manifest version the queue accepts.
const MANIFEST_VERSION: u64 = 2;

/// True when both Directory Identities name the same directory: byte-for-byte equal and not
/// empty. A missing identity is never the same, even as another missing one.
pub fn is_same_directory(a: Option<&str>, b: Option<&str>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if !a.is_empty() && a == b)
}

/// Whether a Queue Selection change asked to move an item earlier or later in the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
}

/// One queue row as a view renders it: the manifest entry, its selection, and, for a participant
/// of the mode's Job Snapshot, the job's progress on it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub id: String,
    /// The manifest's name, or the job's file for a participant the manifest does not list.
    pub name: String,
    /// The item's Release Directory; empty for a participant the manifest does not list.
    pub release: String,
    pub metadata: Option<InspectionMetadata>,
    pub inspection_error: Option<String>,
    pub selected: bool,
    pub status: ItemStatus,
    pub progress: Option<f64>,
    pub start_time_ms: Option<u64>,
    pub duration_sec: Option<f64>,
    /// Only the reason the engine gave: a view supplies its own default label.
    pub reason: Option<String>,
    pub error: Option<String>,
    /// Whether the item is in the batch of the mode's Job Snapshot.
    pub participant: bool,
}

/// The queue of one mode.
#[derive(Debug, Default)]
pub struct Queue {
    /// The accepted Inspection Manifest, kept for Inspection Reuse.
    manifest: Option<InspectionManifest>,
    /// The Directory Identity the queue shows. It outlives a failed scan.
    directory: Option<String>,
    /// The Directory Identity `manifest` is an inspection of: `None` after a failed scan, so the
    /// next scan of the directory inspects it again.
    last_scanned: Option<String>,
    /// Item ID -> its index in `manifest.items`.
    index: HashMap<String, usize>,
    selected: HashSet<String>,
    /// The visible order: the manifest's items.
    order: Vec<String>,
    /// `order` plus the IDs that vanished from the directory, in their last relative order, so
    /// one that reappears takes its old place.
    known_order: Vec<String>,
    /// Selected IDs the last rescan no longer found, until the user changes the selection.
    vanished_selected_ids: Vec<String>,
    /// Track Assignments (remux only), by item ID.
    assignments: BTreeMap<String, TrackAssignment>,
    /// Bumped whenever the manifest is replaced, so a view knows when to rebuild its rows.
    manifest_rev: u64,
}

impl Queue {
    pub fn directory(&self) -> Option<&str> {
        self.directory.as_deref()
    }

    pub fn manifest_rev(&self) -> u64 {
        self.manifest_rev
    }

    pub fn vanished_selected_ids(&self) -> &[String] {
        &self.vanished_selected_ids
    }

    pub fn assignments(&self) -> &BTreeMap<String, TrackAssignment> {
        &self.assignments
    }

    pub fn subtitle_candidates(&self) -> BTreeMap<String, Vec<String>> {
        self.manifest
            .as_ref()
            .filter(|manifest| !manifest.items.is_empty())
            .map(|manifest| manifest.subtitle_candidates.clone())
            .unwrap_or_default()
    }

    /// The selected item IDs in queue order.
    #[cfg(test)]
    pub fn selected_ids(&self) -> Vec<&str> {
        self.order
            .iter()
            .filter(|id| self.selected.contains(*id))
            .map(String::as_str)
            .collect()
    }

    /// Accepts `manifest` as the inspection of `directory`. A rescan of the directory the queue
    /// shows keeps the surviving selection, order and Track Assignments; another directory starts
    /// over. Returns false, changing nothing, for a manifest of another version.
    pub fn accept(&mut self, manifest: InspectionManifest, directory: String) -> bool {
        if manifest.version != MANIFEST_VERSION {
            return false;
        }
        let preserve = is_same_directory(self.directory.as_deref(), Some(&directory));
        self.populate(manifest, preserve);
        self.directory = Some(directory.clone());
        self.last_scanned = Some(directory);
        true
    }

    /// Inspection Reuse: re-accepts the manifest already held when it is an inspection of
    /// `directory`, keeping the selection. Returns false when it is not.
    pub fn reuse(&mut self, directory: &str) -> bool {
        if !is_same_directory(self.last_scanned.as_deref(), Some(directory)) {
            return false;
        }
        let Some(manifest) = self.manifest.take() else {
            return false;
        };
        self.populate(manifest, true);
        true
    }

    /// A failed scan: the rows stay, but the next scan of the directory inspects it again.
    pub fn fail(&mut self) {
        self.last_scanned = None;
    }

    /// Empties the queue: no directory, no manifest, no selection, no Track Assignments.
    pub fn reset(&mut self) {
        let manifest_rev = self.manifest_rev + 1;
        *self = Self {
            manifest_rev,
            ..Self::default()
        };
    }

    fn populate(&mut self, manifest: InspectionManifest, preserve: bool) {
        let mut index = HashMap::with_capacity(manifest.items.len());
        let mut order = Vec::with_capacity(manifest.items.len());
        for (position, item) in manifest.items.iter().enumerate() {
            let id = item.id.to_string();
            if index.insert(id.clone(), position).is_none() {
                order.push(id);
            }
        }
        if preserve {
            self.reconcile(index, order);
            // Only a surviving item keeps its Track Assignment.
            let index = &self.index;
            self.assignments.retain(|id, _| index.contains_key(id));
        } else {
            self.selected = order.iter().cloned().collect();
            self.known_order = order.clone();
            self.order = order;
            self.index = index;
            self.vanished_selected_ids.clear();
            self.assignments.clear();
        }
        self.manifest = Some(manifest);
        self.manifest_rev += 1;
    }

    /// A same-directory rescan: surviving items keep their selection and order, a vanished
    /// selected ID is remembered (and restored selected, in its old place, should it come back),
    /// and a newly found ID is appended unselected.
    fn reconcile(&mut self, index: HashMap<String, usize>, found: Vec<String>) {
        let prior: Vec<String> = std::mem::take(&mut self.vanished_selected_ids);
        let prior_vanished: HashSet<&String> = prior.iter().collect();
        // Still vanished first, in the order remembered, then the ones this rescan lost.
        let mut vanished: Vec<String> = prior
            .iter()
            .filter(|id| !index.contains_key(*id))
            .cloned()
            .collect();
        let mut is_vanished: HashSet<String> = vanished.iter().cloned().collect();
        let known: HashSet<&String> = self.known_order.iter().collect();
        let mut order = Vec::with_capacity(found.len());
        let mut known_order = Vec::with_capacity(self.known_order.len());
        let mut selected = HashSet::new();

        for id in &self.known_order {
            known_order.push(id.clone());
            let was_selected = if self.index.contains_key(id) {
                self.selected.contains(id)
            } else {
                prior_vanished.contains(id)
            };
            if !index.contains_key(id) {
                if was_selected && is_vanished.insert(id.clone()) {
                    vanished.push(id.clone());
                }
                continue;
            }
            if was_selected {
                selected.insert(id.clone());
            }
            order.push(id.clone());
        }
        for id in found {
            if known.contains(&id) {
                continue;
            }
            // A reappearing vanished ID is in `known_order` already, so only a never-seen one
            // gets here; it is selected only if it was remembered as selected.
            if prior_vanished.contains(&id) {
                selected.insert(id.clone());
            }
            order.push(id.clone());
            known_order.push(id);
        }

        self.index = index;
        self.order = order;
        self.known_order = known_order;
        self.selected = selected;
        self.vanished_selected_ids = vanished;
    }

    /// Selects or deselects one item. An explicit choice forgets the vanished selected IDs.
    /// Returns false for an ID the queue does not hold.
    pub fn set_selected(&mut self, id: &str, selected: bool) -> bool {
        if !self.index.contains_key(id) {
            return false;
        }
        if selected {
            self.selected.insert(id.to_string());
        } else {
            self.selected.remove(id);
        }
        self.vanished_selected_ids.clear();
        true
    }

    /// Selects or deselects every item, and forgets the vanished selected IDs. Returns false
    /// when nothing changed.
    pub fn set_all_selected(&mut self, selected: bool) -> bool {
        let next: HashSet<String> = if selected {
            self.order.iter().cloned().collect()
        } else {
            HashSet::new()
        };
        let changed = next != self.selected || !self.vanished_selected_ids.is_empty();
        self.selected = next;
        self.vanished_selected_ids.clear();
        changed
    }

    /// Swaps an item with its neighbour in the order ("up" is earlier), mirrored in the known
    /// order. Returns false for an unknown ID or an item already at that end.
    pub fn reorder(&mut self, id: &str, direction: Direction) -> bool {
        let Some(at) = self.order.iter().position(|candidate| candidate == id) else {
            return false;
        };
        let target = match direction {
            Direction::Up => at.checked_sub(1),
            Direction::Down => Some(at + 1).filter(|&target| target < self.order.len()),
        };
        let Some(target) = target else {
            return false;
        };
        let other = self.order[target].clone();
        self.order.swap(at, target);
        let known = |wanted: &str| self.known_order.iter().position(|id| id == wanted);
        if let (Some(a), Some(b)) = (known(id), known(&other)) {
            self.known_order.swap(a, b);
        }
        true
    }

    /// Sets the Track Assignment of its target item. Returns false for an item the queue does not
    /// hold.
    pub fn assign(&mut self, assignment: TrackAssignment) -> bool {
        if !self.index.contains_key(&assignment.target_video) {
            return false;
        }
        self.assignments
            .insert(assignment.target_video.clone(), assignment);
        true
    }

    /// Clears the Track Assignment of `id`. Returns false when it had none.
    pub fn clear_assignment(&mut self, id: &str) -> bool {
        self.assignments.remove(id).is_some()
    }

    /// The rows in queue order, with `job`'s progress laid over its participants. `job` is the
    /// Job Snapshot of this queue's mode, if any. A participant the manifest does not list follows
    /// the manifest's items, in batch order.
    pub fn rows(&self, job: Option<&JobSnapshot>) -> Vec<Row> {
        let manifest = self.manifest.as_ref();
        let participant = |id: &str| job.and_then(|job| job.items.get(id));
        let mut rows: Vec<Row> = self
            .order
            .iter()
            .map(|id| {
                let entry = manifest.map(|manifest| &manifest.items[self.index[id]]);
                let mut row = Row {
                    id: id.clone(),
                    name: entry.map_or_else(|| id.clone(), |entry| entry.name.clone()),
                    release: entry.map_or_else(String::new, |entry| entry.release.clone()),
                    metadata: entry.map(|entry| entry.metadata.clone()),
                    inspection_error: entry.and_then(|entry| entry.inspection_error.clone()),
                    selected: self.selected.contains(id),
                    status: ItemStatus::Queued,
                    progress: None,
                    start_time_ms: None,
                    duration_sec: None,
                    reason: None,
                    error: None,
                    participant: false,
                };
                if let Some(item) = participant(id) {
                    overlay(&mut row, item);
                }
                row
            })
            .collect();
        if let Some(job) = job {
            for id in &job.ordered_item_ids {
                if self.index.contains_key(id) {
                    continue;
                }
                let item = &job.items[id];
                let mut row = Row {
                    id: id.clone(),
                    name: item.file.clone(),
                    release: String::new(),
                    metadata: None,
                    inspection_error: None,
                    selected: true,
                    status: ItemStatus::Queued,
                    progress: None,
                    start_time_ms: None,
                    duration_sec: None,
                    reason: None,
                    error: None,
                    participant: false,
                };
                overlay(&mut row, item);
                rows.push(row);
            }
        }
        rows
    }
}

/// Lays a participant's progress over its row.
fn overlay(row: &mut Row, item: &crate::snapshot::ItemSnapshot) {
    row.status = item.status;
    row.progress = item.progress;
    row.start_time_ms = item.start_time_ms;
    row.duration_sec = item.duration_sec;
    row.reason = item.reason.clone();
    row.error = item.error.clone();
    row.participant = true;
}

/// Test helpers, also used by the actor's tests.
#[cfg(test)]
pub(crate) mod test_support {
    use mkv_core::item_id::ItemId;
    use mkv_core::manifest::{InspectionManifest, ManifestItem};

    /// A version 2 manifest of `ids`, each named after its ID, in the root release.
    pub fn manifest_of(ids: &[&str]) -> InspectionManifest {
        InspectionManifest {
            version: 2,
            subtitle_candidates: Default::default(),
            items: ids
                .iter()
                .map(|id| ManifestItem {
                    id: ItemId::parse(id),
                    name: (*id).to_string(),
                    release: ".".into(),
                    metadata: Default::default(),
                    inspection_error: None,
                    extra: Default::default(),
                })
                .collect(),
            extra: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::manifest_of;
    use super::*;
    use crate::snapshot::test_support::{batch_complete, batch_start, begun, item_complete};
    use mkv_core::request::AssignmentKind;

    /// A queue that accepted `ids` as the inspection of `/anime`.
    fn queue_of(ids: &[&str]) -> Queue {
        let mut queue = Queue::default();
        assert!(queue.accept(manifest_of(ids), "/anime".into()));
        queue
    }

    /// The queue's order with each item's selection, as `id` or `id-` for a deselected one.
    fn shown(queue: &Queue) -> Vec<String> {
        queue
            .rows(None)
            .iter()
            .map(|row| {
                if row.selected {
                    row.id.clone()
                } else {
                    format!("{}-", row.id)
                }
            })
            .collect()
    }

    fn subtitle(id: &str, path: &str) -> TrackAssignment {
        TrackAssignment {
            target_video: id.into(),
            asset_path: path.into(),
            track_type: AssignmentKind::Subtitle,
            language: Some("ind".into()),
            title: None,
            offset_ms: None,
        }
    }

    #[test]
    fn test_accept_builds_rows_from_the_manifest_with_every_item_selected() {
        let mut manifest = manifest_of(&["Season 01/Show - 01.mkv", "Season 02/Show - 01.mkv"]);
        for (item, release) in manifest.items.iter_mut().zip(["Season 01", "Season 02"]) {
            item.name = "Show - 01.mkv".into();
            item.release = release.into();
            item.metadata.resolution = Some("1080p".into());
        }
        manifest.items[1].inspection_error = Some("mkvmerge inspection timed out".into());
        let mut queue = Queue::default();
        assert!(queue.accept(manifest, "/anime".into()));

        let rows = queue.rows(None);
        // Duplicate basenames in different directories stay two items.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "Season 01/Show - 01.mkv");
        assert_eq!(rows[1].id, "Season 02/Show - 01.mkv");
        assert_eq!(rows[0].name, rows[1].name);
        assert_eq!(rows[1].release, "Season 02");
        assert_eq!(
            rows[0].metadata.as_ref().unwrap().resolution.as_deref(),
            Some("1080p")
        );
        assert_eq!(rows[0].inspection_error, None);
        assert_eq!(
            rows[1].inspection_error.as_deref(),
            Some("mkvmerge inspection timed out")
        );
        assert!(rows
            .iter()
            .all(|row| row.selected && row.status == ItemStatus::Queued && !row.participant));
        assert_eq!(queue.directory(), Some("/anime"));
        assert_eq!(queue.manifest_rev(), 1);
    }

    #[test]
    fn test_accept_refuses_a_manifest_of_another_version() {
        let mut queue = queue_of(&["ep1"]);
        let mut manifest = manifest_of(&["ep2"]);
        manifest.version = 1;
        assert!(!queue.accept(manifest, "/other".into()));
        assert_eq!(shown(&queue), ["ep1"]);
        assert_eq!(queue.directory(), Some("/anime"));
    }

    #[test]
    fn test_selection_and_reorder() {
        let mut queue = queue_of(&["e1", "e2", "e3"]);
        assert!(queue.set_selected("e2", false));
        assert_eq!(shown(&queue), ["e1", "e2-", "e3"]);
        assert!(queue.set_selected("e2", true));
        assert!(!queue.set_selected("missing", true));

        assert!(queue.set_all_selected(false));
        assert_eq!(shown(&queue), ["e1-", "e2-", "e3-"]);
        assert!(!queue.set_all_selected(false));
        assert!(queue.set_all_selected(true));
        assert_eq!(queue.selected_ids(), ["e1", "e2", "e3"]);

        assert!(queue.reorder("e2", Direction::Up));
        assert_eq!(shown(&queue), ["e2", "e1", "e3"]);
        // Already at the top or the bottom: nothing moves.
        assert!(!queue.reorder("e2", Direction::Up));
        assert!(!queue.reorder("e3", Direction::Down));
        assert!(!queue.reorder("missing", Direction::Down));
        assert!(queue.reorder("e2", Direction::Down));
        assert_eq!(shown(&queue), ["e1", "e2", "e3"]);
    }

    #[test]
    fn test_a_same_directory_rescan_keeps_choices_and_order_and_appends_new_ids_unselected() {
        let mut queue = queue_of(&["ep1", "ep2", "gone"]);
        queue.set_selected("ep1", false);
        queue.reorder("ep2", Direction::Up);

        assert!(queue.accept(manifest_of(&["ep1", "new", "ep2"]), "/anime".into()));
        assert_eq!(shown(&queue), ["ep2", "ep1-", "new-"]);
        assert_eq!(queue.manifest_rev(), 2);
    }

    #[test]
    fn test_another_directory_starts_a_new_selection_and_order() {
        let mut queue = queue_of(&["ep1", "ep2"]);
        queue.set_selected("ep1", false);
        queue.reorder("ep2", Direction::Up);
        queue.assign(subtitle("ep2", "/subs/ep2.ass"));

        assert!(queue.accept(manifest_of(&["ep1", "ep2"]), "/other".into()));
        assert_eq!(shown(&queue), ["ep1", "ep2"]);
        assert!(queue.assignments().is_empty());
        assert_eq!(queue.directory(), Some("/other"));
    }

    #[test]
    fn test_directory_identities_compare_byte_for_byte() {
        assert!(is_same_directory(Some("/anime/show"), Some("/anime/show")));
        assert!(is_same_directory(
            Some(r"C:\Anime\Show"),
            Some(r"C:\Anime\Show")
        ));
        // Nothing normalizes: the bridge resolves every spelling to one identity first.
        for (a, b) in [
            ("/anime/show", "/anime/show/"),
            ("/anime/show", "/anime/x/../show"),
            ("/anime/Show", "/anime/show"),
            (r"C:\Anime\Show", "c:/anime/show"),
            ("/anime/showA", "/anime/showB"),
            ("", "/anime/show"),
            ("", ""),
        ] {
            assert!(!is_same_directory(Some(a), Some(b)), "{a} vs {b}");
        }
        assert!(!is_same_directory(None, Some("/anime/show")));
        assert!(!is_same_directory(None, None));

        // A trailing slash is another directory to the queue, so the selection starts over.
        let mut queue = queue_of(&["ep1", "ep2"]);
        queue.set_selected("ep1", false);
        queue.accept(manifest_of(&["ep1", "ep2"]), "/anime/".into());
        assert_eq!(shown(&queue), ["ep1", "ep2"]);
    }

    #[test]
    fn test_a_rescan_remembers_vanished_selected_ids_until_the_selection_changes() {
        let mut queue = queue_of(&["ep1", "ep2", "ep3"]);
        queue.set_selected("ep3", false);
        queue.accept(manifest_of(&["ep1"]), "/anime".into());
        assert_eq!(queue.vanished_selected_ids(), ["ep2"]);

        queue.accept(manifest_of(&["ep1"]), "/anime".into());
        assert_eq!(queue.vanished_selected_ids(), ["ep2"]);

        queue.set_selected("ep1", true);
        assert!(queue.vanished_selected_ids().is_empty());

        for selected in [true, false] {
            let mut queue = queue_of(&["ep1", "ep2"]);
            queue.accept(manifest_of(&["ep1"]), "/anime".into());
            queue.set_all_selected(selected);
            assert!(queue.vanished_selected_ids().is_empty());
        }
    }

    #[test]
    fn test_a_vanished_selected_id_that_reappears_is_selected_in_its_old_place() {
        let mut queue = queue_of(&["ep1", "ep2", "ep3"]);
        queue.reorder("ep2", Direction::Up);
        queue.accept(manifest_of(&["ep1", "ep3"]), "/anime".into());
        assert_eq!(shown(&queue), ["ep1", "ep3"]);
        assert_eq!(queue.vanished_selected_ids(), ["ep2"]);

        queue.accept(manifest_of(&["ep1", "ep2", "ep3"]), "/anime".into());
        assert_eq!(shown(&queue), ["ep2", "ep1", "ep3"]);
        assert!(queue.vanished_selected_ids().is_empty());
    }

    #[test]
    fn test_an_empty_rescan_keeps_the_vanished_selection_for_the_next_one() {
        let mut queue = queue_of(&["ep1", "ep2"]);
        queue.set_selected("ep2", false);
        queue.accept(manifest_of(&[]), "/anime".into());
        assert!(queue.rows(None).is_empty());
        assert_eq!(queue.vanished_selected_ids(), ["ep1"]);
        assert_eq!(queue.directory(), Some("/anime"));

        queue.accept(manifest_of(&["ep2", "ep1"]), "/anime".into());
        assert_eq!(shown(&queue), ["ep1", "ep2-"]);
    }

    #[test]
    fn test_track_assignments_carry_over_only_for_surviving_items() {
        let mut queue = queue_of(&["Season 01/ep1.mkv", "Season 01/gone.mkv"]);
        assert!(queue.assign(subtitle("Season 01/ep1.mkv", "Season 01/better.ass")));
        assert!(queue.assign(subtitle("Season 01/gone.mkv", "Season 01/deleted.ass")));
        // An item the queue does not hold takes no assignment.
        assert!(!queue.assign(subtitle("orphaned.mkv", "/subs/orphaned.ass")));

        queue.accept(
            manifest_of(&["Season 01/ep1.mkv", "Season 01/ep2.mkv"]),
            "/anime".into(),
        );
        assert_eq!(
            queue.assignments().values().collect::<Vec<_>>(),
            [&subtitle("Season 01/ep1.mkv", "Season 01/better.ass")]
        );
        assert!(queue.clear_assignment("Season 01/ep1.mkv"));
        assert!(!queue.clear_assignment("Season 01/ep1.mkv"));

        queue.assign(subtitle("Season 01/ep2.mkv", "/subs/ep2.ass"));
        queue.accept(manifest_of(&[]), "/anime".into());
        assert!(queue.assignments().is_empty());
    }

    #[test]
    fn test_reuse_reaccepts_the_held_manifest_only_for_its_directory() {
        let mut queue = queue_of(&["ep1", "ep2"]);
        queue.set_selected("ep2", false);
        assert!(!queue.reuse("/other"));
        assert!(queue.reuse("/anime"));
        assert_eq!(shown(&queue), ["ep1", "ep2-"]);
        assert_eq!(queue.manifest_rev(), 2);

        // A failed scan keeps the rows but not the reuse.
        queue.fail();
        assert!(!queue.reuse("/anime"));
        assert_eq!(shown(&queue), ["ep1", "ep2-"]);

        queue.reset();
        assert!(queue.rows(None).is_empty());
        assert_eq!(queue.directory(), None);
        assert!(!queue.reuse("/anime"));
        assert_eq!(queue.manifest_rev(), 3);
    }

    #[test]
    fn test_rows_lay_the_job_over_its_participants_and_leave_the_rest_queued() {
        let mut queue = queue_of(&["ep1", "ep2", "ep3"]);
        queue.set_selected("ep3", false);
        // A subset batch, in its own order, and a participant the manifest no longer lists.
        let mut job = begun(1);
        job.apply(&batch_start(&["ep2", "ep1", "extra"]));
        job.apply(&item_complete("ep1", 4.0));
        job.apply(&batch_complete(false, true));

        let rows = queue.rows(Some(&job));
        let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["ep1", "ep2", "ep3", "extra"]);
        assert_eq!(rows[0].status, ItemStatus::Completed);
        assert_eq!(rows[0].duration_sec, Some(4.0));
        assert!(rows[0].participant && rows[1].participant && rows[3].participant);
        assert_eq!(rows[1].status, ItemStatus::Queued);
        // A non-participant keeps its selection and stays queued.
        assert!(!rows[2].participant);
        assert!(!rows[2].selected);
        assert_eq!(rows[2].status, ItemStatus::Queued);
        assert_eq!(rows[3].name, "extra.mkv");
        assert_eq!(rows[3].metadata, None);
    }

    #[test]
    fn test_a_job_that_ended_before_batch_start_leaves_every_row_and_selection_alone() {
        let mut queue = queue_of(&["ep1", "ep2"]);
        queue.set_selected("ep2", false);
        let mut job = begun(1);
        job.apply(&batch_complete(false, false));

        assert_eq!(queue.rows(Some(&job)), queue.rows(None));
        assert_eq!(shown(&queue), ["ep1", "ep2-"]);
    }

    #[test]
    fn test_ten_thousand_items() {
        let ids: Vec<String> = (0..10_000).map(|i| format!("ep{i}.mkv")).collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut queue = queue_of(&ids);
        let mut job = begun(1);
        job.apply(&batch_start(&ids));
        for id in &ids {
            job.apply(&item_complete(id, 1.0));
        }
        queue.accept(manifest_of(&ids), "/anime".into());

        let rows = queue.rows(Some(&job));
        assert_eq!(rows.len(), 10_000);
        assert!(rows.iter().all(|row| row.status == ItemStatus::Completed));
        assert_eq!(queue.selected_ids().len(), 10_000);
    }

    #[test]
    fn test_row_wire_shape() {
        let queue = queue_of(&["ep1"]);
        assert_eq!(
            serde_json::to_value(&queue.rows(None)[0]).unwrap(),
            serde_json::json!({
                "id": "ep1",
                "name": "ep1",
                "release": ".",
                "metadata": {
                    "resolution": null,
                    "video_codec": null,
                    "video_tag": null,
                    "audio_summary": null,
                    "has_flac": null,
                    "sub_summary": null,
                    "fonts_count": null
                },
                "inspection_error": null,
                "selected": true,
                "status": "queued",
                "progress": null,
                "start_time_ms": null,
                "duration_sec": null,
                "reason": null,
                "error": null,
                "participant": false
            })
        );
    }
}
