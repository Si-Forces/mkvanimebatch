//! The Queue Tracker's model, one per mode: the accepted Inspection Manifest, the Queue Selection
//! and its order, and the Track Assignments. `rows` lays a Job Snapshot's participants over the
//! manifest order and presents each row, which is what a view renders. Directory Identities are
//! compared byte for byte: the Engine Bridge produces every one in display-canonical form and
//! nothing here normalizes a path.

use crate::presentation::{
    bucket, failed_meta, inspected_meta, job_only_meta, status_label, status_title, status_tone,
    Bucket, Tone,
};
use crate::snapshot::{ItemSnapshot, ItemStatus, JobSnapshot};
use mkv_core::manifest::{InspectionManifest, ManifestItem};
use mkv_core::request::TrackAssignment;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

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
/// of the mode's Job Snapshot, the job's progress on it, with the texts a view shows for them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub id: String,
    /// The manifest's name, or the job's file for a participant the manifest does not list.
    pub name: String,
    /// The item's Release Directory; empty for a participant the manifest does not list.
    pub release: String,
    /// Whether the accepted manifest lists the item: only such a row is in the Queue Selection.
    pub in_manifest: bool,
    pub selected: bool,
    pub status: ItemStatus,
    /// The job's progress on a processing item, in percent: the status ring's arc.
    pub progress: Option<f64>,
    /// Whether the item is in the batch of the mode's Job Snapshot.
    pub participant: bool,
    /// The status column text; a processing row's elapsed time advances with the Presentation
    /// Heartbeat.
    pub label: String,
    pub tone: Tone,
    /// The status hover text: the error, else the reason (the engine's, else the default label).
    pub title: String,
    /// The line under the name: what a run writes for the item, why it failed, or its outcome.
    pub meta: String,
    /// Whether `meta` reports a failure (of the item or of its inspection).
    pub meta_error: bool,
    /// The filter chip the row counts under: `None` when unselected.
    pub bucket: Option<Bucket>,
}

#[cfg(test)]
impl Row {
    /// A queued, selected manifest row of `id`, named after it, in the root release.
    pub(crate) fn test(id: &str) -> Self {
        Row {
            id: id.into(),
            name: id.into(),
            release: ".".into(),
            in_manifest: true,
            selected: true,
            status: ItemStatus::Queued,
            progress: None,
            participant: false,
            label: "Queued".into(),
            tone: Tone::Mute,
            title: String::new(),
            meta: String::new(),
            meta_error: false,
            bucket: Some(Bucket::Todo),
        }
    }
}

/// What a row's meta line depends on besides its manifest entry and Track Assignment: the
/// mode, and for remux the Settings Draft's bitrate and assets dir, trimmed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetaInputs {
    pub remux: bool,
    pub bitrate: String,
    pub assets_dir: String,
}

/// The part of a manifest row that depends only on the manifest, the Track Assignments and the
/// `MetaInputs`, kept between publications so one only lays the status over it.
#[derive(Debug, Clone)]
struct RowBase {
    name: String,
    release: String,
    meta: String,
    meta_error: bool,
}

/// The `RowBase` of each manifest item, by its index in the manifest, and what it was built from.
#[derive(Debug, Default)]
struct BaseCache {
    /// The `manifest_rev`, `assignments_rev` and inputs `rows` was built from.
    key: Option<(u64, u64, MetaInputs)>,
    rows: Vec<RowBase>,
    /// How often `rows` was built.
    #[cfg(test)]
    builds: usize,
}

/// The queue of one mode.
#[derive(Debug, Default)]
pub struct Queue {
    /// The accepted Inspection Manifest, kept for Inspection Reuse; shared with every queue that
    /// accepted the same inspection.
    manifest: Option<Arc<InspectionManifest>>,
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
    /// Bumped whenever a Track Assignment is set or cleared, which changes a row's meta line.
    assignments_rev: u64,
    base: BaseCache,
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
        // A folder with no episodes shows none: a candidate is offered per item's release, and the
        // webview cleared them for an empty manifest before S2 too.
        self.manifest
            .as_ref()
            .filter(|manifest| !manifest.items.is_empty())
            .map(|manifest| manifest.subtitle_candidates.clone())
            .unwrap_or_default()
    }

    /// The accepted manifest's entry of `id`.
    pub fn entry(&self, id: &str) -> Option<&ManifestItem> {
        let at = *self.index.get(id)?;
        self.manifest.as_ref()?.items.get(at)
    }

    /// Whether the accepted manifest lists `id`.
    pub fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }

    /// The selected item IDs in queue order.
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
    pub fn accept(
        &mut self,
        manifest: impl Into<Arc<InspectionManifest>>,
        directory: String,
    ) -> bool {
        let manifest = manifest.into();
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

    fn populate(&mut self, manifest: Arc<InspectionManifest>, preserve: bool) {
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
            // A vanished ID is always in `known_order`, so only a never-seen one gets here, and
            // it is appended unselected.
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
    /// Returns false for an ID the queue does not hold, or when nothing changed.
    pub fn set_selected(&mut self, id: &str, selected: bool) -> bool {
        if !self.index.contains_key(id) {
            return false;
        }
        let toggled = if selected {
            self.selected.insert(id.to_string())
        } else {
            self.selected.remove(id)
        };
        let changed = toggled || !self.vanished_selected_ids.is_empty();
        self.vanished_selected_ids.clear();
        changed
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
        self.assignments_rev += 1;
        true
    }

    /// Clears the Track Assignment of `id`. Returns false when it had none.
    pub fn clear_assignment(&mut self, id: &str) -> bool {
        let cleared = self.assignments.remove(id).is_some();
        self.assignments_rev += u64::from(cleared);
        cleared
    }

    /// The rows in queue order, with `job`'s progress laid over its participants, presented at
    /// `now` with `inputs`. `job` is the Job Snapshot of this queue's mode, if any. A participant
    /// the manifest does not list follows the manifest's items, in batch order.
    pub fn rows(
        &mut self,
        job: Option<&JobSnapshot>,
        inputs: &MetaInputs,
        now: Instant,
    ) -> Vec<Row> {
        self.refresh_base(inputs);
        let item = |id: &str| job.and_then(|job| job.items.get(id));
        let mut rows: Vec<Row> = self
            .order
            .iter()
            .map(|id| {
                let base = self.base.rows.get(self.index[id]);
                let selected = self.selected.contains(id);
                present(id, true, selected, base, item(id), now)
            })
            .collect();
        if let Some(job) = job {
            for id in &job.ordered_item_ids {
                if !self.index.contains_key(id) {
                    rows.push(present(id, false, true, None, item(id), now));
                }
            }
        }
        rows
    }

    /// Builds the `RowBase` of every manifest item again, unless the manifest, the Track
    /// Assignments and `inputs` are those it was built from.
    fn refresh_base(&mut self, inputs: &MetaInputs) {
        let key = (self.manifest_rev, self.assignments_rev, inputs.clone());
        if self.base.key.as_ref() == Some(&key) {
            return;
        }
        let Some(manifest) = self.manifest.as_deref() else {
            self.base.rows.clear();
            self.base.key = Some(key);
            return;
        };
        self.base.rows = manifest
            .items
            .iter()
            .map(|entry| {
                let (meta, meta_error) = match entry.inspection_error.as_deref() {
                    Some(error) if !error.is_empty() => {
                        (format!("Inspection failed: {error}"), true)
                    }
                    _ => {
                        let candidates = Some(entry.release.as_str())
                            .filter(|release| !release.is_empty())
                            .and_then(|release| manifest.subtitle_candidates.get(release));
                        let assignment = self
                            .assignments
                            .get(&entry.id.to_string())
                            .map(|assignment| assignment.asset_path.to_string_lossy());
                        let meta = inspected_meta(
                            inputs.remux,
                            &entry.metadata,
                            &entry.release,
                            candidates.map_or(&[][..], Vec::as_slice),
                            assignment.as_deref(),
                            &inputs.bitrate,
                            &inputs.assets_dir,
                        );
                        (meta, false)
                    }
                };
                RowBase {
                    name: entry.name.clone(),
                    release: entry.release.clone(),
                    meta,
                    meta_error,
                }
            })
            .collect();
        self.base.key = Some(key);
        #[cfg(test)]
        {
            self.base.builds += 1;
        }
    }
}

/// A row of `id` as a view renders it: from its manifest `base` (none for a participant the
/// manifest does not list) and, for a participant, the job's `item`, at `now`.
fn present(
    id: &str,
    in_manifest: bool,
    selected: bool,
    base: Option<&RowBase>,
    item: Option<&ItemSnapshot>,
    now: Instant,
) -> Row {
    let status = item.map_or(ItemStatus::Queued, |item| item.status);
    let progress = item.and_then(|item| item.progress);
    let duration_sec = item.and_then(|item| item.duration_sec);
    let reason = item.and_then(|item| item.reason.as_deref());
    let error = item.and_then(|item| item.error.as_deref());
    let elapsed = item
        .and_then(ItemSnapshot::started_at)
        .map(|started_at| now.saturating_duration_since(started_at));
    // A failed row says why, in place of a manifest row's inspected meta, so a batch with several
    // failures reads without opening each row.
    let (meta, meta_error) = if status == ItemStatus::Error {
        (failed_meta(error), true)
    } else {
        match base {
            Some(base) => (base.meta.clone(), base.meta_error),
            None => (job_only_meta(status, duration_sec, reason, error), false),
        }
    };
    let name = match (base, item) {
        (Some(base), _) => base.name.clone(),
        (None, Some(item)) => item.file.clone(),
        (None, None) => id.to_string(),
    };
    Row {
        id: id.to_string(),
        name,
        release: base.map(|base| base.release.clone()).unwrap_or_default(),
        in_manifest,
        selected,
        status,
        progress,
        participant: item.is_some(),
        label: status_label(status, progress, elapsed, duration_sec),
        tone: status_tone(status),
        title: status_title(status, error, reason),
        meta,
        meta_error,
        bucket: bucket(status, selected),
    }
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

    /// The rows of `queue` as an extract queue presents them.
    fn rows(queue: &mut Queue, job: Option<&JobSnapshot>) -> Vec<Row> {
        queue.rows(job, &MetaInputs::default(), Instant::now())
    }

    /// The queue's order with each item's selection, as `id` or `id-` for a deselected one.
    fn shown(queue: &mut Queue) -> Vec<String> {
        rows(queue, None)
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

        let rows = rows(&mut queue, None);
        // Duplicate basenames in different directories stay two items.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "Season 01/Show - 01.mkv");
        assert_eq!(rows[1].id, "Season 02/Show - 01.mkv");
        assert_eq!(rows[0].name, rows[1].name);
        assert_eq!(rows[1].release, "Season 02");
        assert_eq!(
            (rows[0].meta.as_str(), rows[0].meta_error),
            ("No subs · 0 fonts", false)
        );
        assert_eq!(
            (rows[1].meta.as_str(), rows[1].meta_error),
            ("Inspection failed: mkvmerge inspection timed out", true)
        );
        assert!(rows.iter().all(|row| row.selected
            && row.in_manifest
            && row.status == ItemStatus::Queued
            && !row.participant));
        assert_eq!(queue.directory(), Some("/anime"));
        assert_eq!(queue.manifest_rev(), 1);
    }

    #[test]
    fn test_accept_refuses_a_manifest_of_another_version() {
        let mut queue = queue_of(&["ep1"]);
        let mut manifest = manifest_of(&["ep2"]);
        manifest.version = 1;
        assert!(!queue.accept(manifest, "/other".into()));
        assert_eq!(shown(&mut queue), ["ep1"]);
        assert_eq!(queue.directory(), Some("/anime"));
    }

    #[test]
    fn test_selection_and_reorder() {
        let mut queue = queue_of(&["e1", "e2", "e3"]);
        assert!(queue.set_selected("e2", false));
        assert_eq!(shown(&mut queue), ["e1", "e2-", "e3"]);
        assert!(queue.set_selected("e2", true));
        // An unchanged value is a no-op.
        assert!(!queue.set_selected("e2", true));
        assert!(!queue.set_selected("missing", true));

        assert!(queue.set_all_selected(false));
        assert_eq!(shown(&mut queue), ["e1-", "e2-", "e3-"]);
        assert!(!queue.set_all_selected(false));
        assert!(queue.set_all_selected(true));
        assert_eq!(queue.selected_ids(), ["e1", "e2", "e3"]);

        assert!(queue.reorder("e2", Direction::Up));
        assert_eq!(shown(&mut queue), ["e2", "e1", "e3"]);
        // Already at the top or the bottom: nothing moves.
        assert!(!queue.reorder("e2", Direction::Up));
        assert!(!queue.reorder("e3", Direction::Down));
        assert!(!queue.reorder("missing", Direction::Down));
        assert!(queue.reorder("e2", Direction::Down));
        assert_eq!(shown(&mut queue), ["e1", "e2", "e3"]);
    }

    #[test]
    fn test_a_same_directory_rescan_keeps_choices_and_order_and_appends_new_ids_unselected() {
        let mut queue = queue_of(&["ep1", "ep2", "gone"]);
        queue.set_selected("ep1", false);
        queue.reorder("ep2", Direction::Up);

        assert!(queue.accept(manifest_of(&["ep1", "new", "ep2"]), "/anime".into()));
        assert_eq!(shown(&mut queue), ["ep2", "ep1-", "new-"]);
        assert_eq!(queue.manifest_rev(), 2);
    }

    #[test]
    fn test_another_directory_starts_a_new_selection_and_order() {
        let mut queue = queue_of(&["ep1", "ep2"]);
        queue.set_selected("ep1", false);
        queue.reorder("ep2", Direction::Up);
        queue.assign(subtitle("ep2", "/subs/ep2.ass"));

        assert!(queue.accept(manifest_of(&["ep1", "ep2"]), "/other".into()));
        assert_eq!(shown(&mut queue), ["ep1", "ep2"]);
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
        assert_eq!(shown(&mut queue), ["ep1", "ep2"]);
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
        assert_eq!(shown(&mut queue), ["ep1", "ep3"]);
        assert_eq!(queue.vanished_selected_ids(), ["ep2"]);

        queue.accept(manifest_of(&["ep1", "ep2", "ep3"]), "/anime".into());
        assert_eq!(shown(&mut queue), ["ep2", "ep1", "ep3"]);
        assert!(queue.vanished_selected_ids().is_empty());
    }

    #[test]
    fn test_an_empty_rescan_keeps_the_vanished_selection_for_the_next_one() {
        let mut queue = queue_of(&["ep1", "ep2"]);
        queue.set_selected("ep2", false);
        queue.accept(manifest_of(&[]), "/anime".into());
        assert!(rows(&mut queue, None).is_empty());
        assert_eq!(queue.vanished_selected_ids(), ["ep1"]);
        assert_eq!(queue.directory(), Some("/anime"));

        queue.accept(manifest_of(&["ep2", "ep1"]), "/anime".into());
        assert_eq!(shown(&mut queue), ["ep1", "ep2-"]);
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
        assert_eq!(shown(&mut queue), ["ep1", "ep2-"]);
        assert_eq!(queue.manifest_rev(), 2);

        // A failed scan keeps the rows but not the reuse.
        queue.fail();
        assert!(!queue.reuse("/anime"));
        assert_eq!(shown(&mut queue), ["ep1", "ep2-"]);

        queue.reset();
        assert!(rows(&mut queue, None).is_empty());
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

        let rows = rows(&mut queue, Some(&job));
        let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["ep1", "ep2", "ep3", "extra"]);
        assert_eq!(rows[0].status, ItemStatus::Completed);
        assert_eq!(rows[0].label, "Done in 4.0s");
        assert!(rows[0].participant && rows[1].participant && rows[3].participant);
        assert_eq!(rows[1].status, ItemStatus::Queued);
        // A non-participant keeps its selection and stays queued.
        assert!(!rows[2].participant);
        assert!(!rows[2].selected);
        assert_eq!(rows[2].status, ItemStatus::Queued);
        assert_eq!(rows[3].name, "extra.mkv");
        assert_eq!(rows[3].meta, "Queued");
        assert!(rows[0].in_manifest && !rows[3].in_manifest);
    }

    #[test]
    fn test_a_job_that_ended_before_batch_start_leaves_every_row_and_selection_alone() {
        let mut queue = queue_of(&["ep1", "ep2"]);
        queue.set_selected("ep2", false);
        let mut job = begun(1);
        job.apply(&batch_complete(false, false));

        assert_eq!(rows(&mut queue, Some(&job)), rows(&mut queue, None));
        assert_eq!(shown(&mut queue), ["ep1", "ep2-"]);
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

        let rows = rows(&mut queue, Some(&job));
        assert_eq!(rows.len(), 10_000);
        assert!(rows.iter().all(|row| row.status == ItemStatus::Completed));
        assert_eq!(queue.selected_ids().len(), 10_000);
    }

    #[test]
    fn test_row_wire_shape() {
        let mut queue = queue_of(&["ep1"]);
        assert_eq!(
            serde_json::to_value(&rows(&mut queue, None)[0]).unwrap(),
            serde_json::json!({
                "id": "ep1",
                "name": "ep1",
                "release": ".",
                "in_manifest": true,
                "selected": true,
                "status": "queued",
                "progress": null,
                "participant": false,
                "label": "Queued",
                "tone": "mute",
                "title": "",
                "meta": "No subs · 0 fonts",
                "meta_error": false,
                "bucket": "todo"
            })
        );
    }

    /// A remux queue's inputs at `bitrate` and `assets_dir`.
    fn remux(bitrate: &str, assets_dir: &str) -> MetaInputs {
        MetaInputs {
            remux: true,
            bitrate: bitrate.into(),
            assets_dir: assets_dir.into(),
        }
    }

    #[test]
    fn test_the_row_base_is_reused_until_its_manifest_assignments_or_inputs_change() {
        let mut manifest = manifest_of(&["ep1", "ep2"]);
        for item in &mut manifest.items {
            item.metadata.audio_summary = Some("FLAC 2.0".into());
            item.metadata.has_flac = Some(true);
        }
        let mut queue = Queue::default();
        queue.accept(manifest.clone(), "/anime".into());
        let now = Instant::now();
        let meta =
            |queue: &mut Queue, inputs: &MetaInputs| queue.rows(None, inputs, now)[0].meta.clone();
        assert_eq!(
            meta(&mut queue, &remux("192k", "")),
            "FLAC 2.0 → Opus 192k · No subs · 0 fonts"
        );
        // A selection, an order or a job changes no base.
        queue.set_selected("ep1", false);
        queue.reorder("ep2", Direction::Up);
        let mut job = begun(1);
        job.apply(&batch_start(&["ep1"]));
        queue.rows(Some(&job), &remux("192k", ""), now);
        assert_eq!(queue.base.builds, 1);

        assert_eq!(
            meta(&mut queue, &remux("256k", "")),
            "FLAC 2.0 → Opus 256k · No subs · 0 fonts"
        );
        assert_eq!(
            meta(&mut queue, &remux("256k", "/assets")),
            "FLAC 2.0 → Opus 256k · Assets dir fallback · 0 fonts"
        );
        assert_eq!(queue.base.builds, 3);
        queue.assign(subtitle("ep2", "/subs/ep2.ass"));
        assert_eq!(
            meta(&mut queue, &remux("256k", "/assets")),
            "FLAC 2.0 → Opus 256k · Manual subs · 0 fonts"
        );
        assert!(queue.clear_assignment("ep2"));
        assert!(!queue.clear_assignment("ep2"));
        meta(&mut queue, &remux("256k", "/assets"));
        assert_eq!(queue.base.builds, 5);
        queue.accept(manifest, "/anime".into());
        meta(&mut queue, &remux("256k", "/assets"));
        assert_eq!(queue.base.builds, 6);
    }

    #[test]
    fn test_a_row_presents_its_status_over_its_base() {
        let mut queue = queue_of(&["ep1", "ep2", "ep3"]);
        queue.set_selected("ep3", false);
        let mut job = begun(1);
        let start = Instant::now();
        job.apply_at(&batch_start(&["ep1", "ep2", "gone"]), start);
        job.apply_at(&crate::snapshot::test_support::item_start("ep1"), start);
        job.apply_at(
            &crate::snapshot::test_support::item_progress("ep1", 64.4),
            start,
        );
        job.apply_at(&crate::snapshot::test_support::item_start("ep2"), start);
        let later = start + std::time::Duration::from_secs(72);
        let rows = queue.rows(Some(&job), &MetaInputs::default(), later);
        let shown: Vec<_> = rows
            .iter()
            .map(|row| (row.label.as_str(), row.tone, row.bucket, row.title.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("64% · 1m 12s", Tone::Info, Some(Bucket::Todo), ""),
                ("Processing · 1m 12s", Tone::Info, Some(Bucket::Todo), ""),
                ("Queued", Tone::Mute, None, ""),
                ("Queued", Tone::Mute, Some(Bucket::Todo), ""),
            ]
        );
        // A participant the manifest does not list is named after the job's file.
        assert_eq!(
            (rows[3].name.as_str(), rows[3].meta.as_str()),
            ("gone.mkv", "Queued")
        );

        // A failure replaces the meta line; the next manifest of the folder brings it back.
        job.apply_at(&failed("ep1", Some("mkvmerge exited with 2")), later);
        job.apply_at(&failed("gone", None), later);
        let rows = queue.rows(Some(&job), &MetaInputs::default(), later);
        assert_eq!(
            (
                rows[0].meta.as_str(),
                rows[0].meta_error,
                rows[0].title.as_str()
            ),
            (
                "Failed · mkvmerge exited with 2",
                true,
                "mkvmerge exited with 2"
            )
        );
        assert_eq!(
            (rows[0].label.as_str(), rows[0].tone),
            ("Failed", Tone::Err)
        );
        assert_eq!(
            (rows[3].meta.as_str(), rows[3].title.as_str()),
            ("Failed · Failed", "Failed")
        );
        assert_eq!(rows[0].bucket, Some(Bucket::Failed));
        let rows = queue.rows(None, &MetaInputs::default(), later);
        assert_eq!(
            (rows[0].meta.as_str(), rows[0].meta_error),
            ("No subs · 0 fonts", false)
        );
    }

    /// An `ItemComplete` that fails `id` with `error`.
    fn failed(id: &str, error: Option<&str>) -> mkv_core::pipeline::JobEvent {
        let mut event = item_complete(id, 1.0);
        if let mkv_core::pipeline::JobEvent::ItemComplete {
            status, error: e, ..
        } = &mut event
        {
            *status = mkv_core::pipeline::ItemOutcome::Error;
            *e = error.map(str::to_string);
        }
        event
    }
}
