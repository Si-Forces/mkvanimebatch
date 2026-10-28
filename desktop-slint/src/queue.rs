//! Each mode's queue list (P2): the published rows mapped into the window's `QueueRow`s, the list
//! header into a `QueueHead`, and the diff that keeps the mode's `VecModel` across publications.
//! Pure, and run off the UI thread (in the Publisher, before the hop to the event loop): only the
//! model writes an [`Update`] carries run on it. Every text and rule the list shows that the
//! published `ModeView` does not already carry is here; `queue-view.js` is the reference.

use crate::inspector::{InspectorList, InspectorUpdate};
use mkv_anime_batch_slint_ui::{
    LibraryItem, Mode, QueueFilter, QueueHead, QueueRow, RowMark, RowTone,
};
use mkv_desktop_core::actor::{ModeView, ScanView};
use mkv_desktop_core::presentation::{Bucket, LibraryEntry, Tone, NOT_IN_BATCH};
use mkv_desktop_core::queue::Row;
use mkv_desktop_core::snapshot::ItemStatus;

/// The group header of a release the Library does not name.
const DEFAULT_GROUP: &str = "Episodes";
/// The card while the folder is inspected.
const SCANNING_TITLE: &str = "Scanning folder";
const SCANNING_TEXT: &str = "Inspecting MKV files and reading track metadata…";
/// The card after a failed scan.
const FAILED_TITLE: &str = "Scan failed";
const FAILED_TEXT: &str =
    "That folder could not be inspected. Choose another folder and try again.";
const FAILED_ACTION: &str = "Choose another folder…";
/// The empty list's button: it opens the folder picker.
const OPEN_FOLDER: &str = "Open folder…";
/// Between the crumbs of the list header's path.
pub(crate) const CRUMB_SEPARATOR: &str = " › ";

/// The filter chip's bucket; `None` for All, which shows every row.
fn bucket_of(filter: QueueFilter) -> Option<Bucket> {
    match filter {
        QueueFilter::All => None,
        QueueFilter::Todo => Some(Bucket::Todo),
        QueueFilter::Done => Some(Bucket::Done),
        QueueFilter::Failed => Some(Bucket::Failed),
    }
}

/// Whether `filter` shows `row`: an unselected row has no bucket, so only All shows it.
fn shows(row: &Row, filter: QueueFilter) -> bool {
    bucket_of(filter).is_none_or(|bucket| row.bucket == Some(bucket))
}

/// A count as a Slint `int`, saturating.
fn to_slint_count(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// The published group header of `release`: the input folder's covers `.` and the empty release.
fn group_label<'a>(library: &'a [LibraryEntry], release: &str) -> &'a str {
    let key = if release.is_empty() { "." } else { release };
    library
        .iter()
        .find(|entry| entry.release.as_deref().unwrap_or(".") == key)
        .map_or(DEFAULT_GROUP, |entry| entry.group_label.as_str())
}

/// The status mark beside the label: a ring while processing (spinning until the first progress
/// value), a tick or a cross for an outcome, none for an unselected row.
fn mark(row: &Row) -> RowMark {
    if !row.selected {
        return RowMark::None;
    }
    match row.status {
        ItemStatus::Processing if row.progress.is_some() => RowMark::Ring,
        ItemStatus::Processing => RowMark::Spin,
        ItemStatus::Completed => RowMark::Ok,
        ItemStatus::Error => RowMark::Err,
        _ => RowMark::None,
    }
}

pub(crate) fn tone(tone: Tone) -> RowTone {
    match tone {
        Tone::Info => RowTone::Info,
        Tone::Ok => RowTone::Ok,
        Tone::Err => RowTone::Err,
        Tone::Mute => RowTone::Mute,
    }
}

/// The rows `filter` shows, each run of one release under a header naming it in upper case
/// (style.css) and counting the rows of the run the filter shows, when the queue spans more than
/// one release. Every row of a run carries its header, which the list pins at its top while the
/// run scrolls under it; the run's first row `opens` it. A row the filter hides stays in the
/// list, not shown, so each index is the core's and a filter change rewrites rows in place.
pub(crate) fn map_rows(
    rows: &[Row],
    library: &[LibraryEntry],
    filter: QueueFilter,
) -> Vec<QueueRow> {
    let grouped = rows
        .first()
        .is_some_and(|first| rows.iter().any(|row| row.release != first.release));
    let shown: Vec<usize> = (0..rows.len())
        .filter(|&i| shows(&rows[i], filter))
        .collect();
    // Each shown row of a run of one release among the shown rows: its header and count, and
    // whether it starts the run.
    let mut heads = vec![None; rows.len()];
    if grouped {
        for run in shown.chunk_by(|&a, &b| rows[a].release == rows[b].release) {
            let release = rows[run[0]].release.as_str();
            let label: slint::SharedString = group_label(library, release).to_uppercase().into();
            for (n, &i) in run.iter().enumerate() {
                heads[i] = Some((label.clone(), run.len(), n == 0));
            }
        }
    }
    rows.iter()
        .zip(heads)
        .enumerate()
        .map(|(index, (row, head))| QueueRow {
            id: row.id.as_str().into(),
            num: format!("{:02}", index + 1).into(),
            name: row.name.as_str().into(),
            meta: row.meta.as_str().into(),
            meta_error: row.meta_error,
            label: if row.selected {
                row.label.as_str()
            } else {
                NOT_IN_BATCH
            }
            .into(),
            tone: if row.selected {
                tone(row.tone)
            } else {
                RowTone::Mute
            },
            mark: mark(row),
            // Percent; a display value, so f32 is plenty.
            progress: row.progress.unwrap_or_default() as f32,
            title: row.title.as_str().into(),
            selected: row.selected,
            check_label: format!("Include {} in the batch", row.name).into(),
            shown: shows(row, filter),
            head: head
                .as_ref()
                .map(|(label, ..)| label.clone())
                .unwrap_or_default(),
            head_count: head.as_ref().map_or(0, |&(_, n, _)| to_slint_count(n)),
            opens: head.is_some_and(|(.., opens)| opens),
        })
        .collect()
}

/// The list header's path: every crumb but the last, each followed by the separator, then the
/// last, which the header draws bold.
fn crumbs(crumbs: &[String]) -> (String, String) {
    match crumbs.split_last() {
        Some((end, lead)) => (
            lead.iter()
                .map(|crumb| format!("{crumb}{CRUMB_SEPARATOR}"))
                .collect(),
            end.clone(),
        ),
        None => (String::new(), String::new()),
    }
}

/// The card shown in place of the rows; its action is empty for none.
#[derive(Default)]
struct Card<'a> {
    title: &'a str,
    text: &'a str,
    action: &'a str,
    spinner: bool,
}

/// The card shown in place of the rows, or `None` for the rows. A rescan of the folder whose rows
/// are on screen (every Run forces one) keeps them there; after a card, it shows its own
/// (queue-view.js `showScanning`).
fn card(view: &ModeView, rows_shown: bool) -> Option<Card<'_>> {
    let card = |title, text, action, spinner| {
        Some(Card {
            title,
            text,
            action,
            spinner,
        })
    };
    match &view.scan {
        ScanView::Scanning { directory }
            if rows_shown
                && !view.rows.is_empty()
                && view.directory.as_ref() == Some(directory) =>
        {
            None
        }
        ScanView::Scanning { .. } => card(SCANNING_TITLE, SCANNING_TEXT, "", true),
        ScanView::Failed => card(FAILED_TITLE, FAILED_TEXT, FAILED_ACTION, false),
        ScanView::Idle => view
            .empty
            .as_ref()
            .and_then(|empty| card(empty.title, empty.description, OPEN_FOLDER, false)),
    }
}

/// The list header, the filter chips, the card and the row the Episode Inspector shows.
/// `rows_shown`: whether the last head showed the rows rather than a card.
pub(crate) fn map_head(view: &ModeView, filter: QueueFilter, rows_shown: bool) -> QueueHead {
    let (crumbs_lead, crumbs_end) = crumbs(&view.crumbs);
    let card = card(view, rows_shown);
    let card_shown = card.is_some();
    let card = card.unwrap_or_default();
    // From the chips' counts, so the message agrees with the chip.
    let none_match = match filter {
        QueueFilter::All => false,
        QueueFilter::Todo => view.filters.todo == 0,
        QueueFilter::Done => view.filters.done == 0,
        QueueFilter::Failed => view.filters.failed == 0,
    };
    QueueHead {
        title: view.title.as_str().into(),
        crumbs_lead: crumbs_lead.into(),
        crumbs_end: crumbs_end.into(),
        all: to_slint_count(view.filters.all),
        todo: to_slint_count(view.filters.todo),
        done: to_slint_count(view.filters.done),
        failed: to_slint_count(view.filters.failed),
        filter,
        // Only beside rows: a card replaces them (queue-view.js hides it with no row mounted).
        filter_empty: !card_shown && none_match && !view.rows.is_empty(),
        card_shown,
        card_title: card.title.into(),
        card_text: card.text.into(),
        card_action: card.action.into(),
        card_spinner: card.spinner,
        selection_locked: view.controls.selection_locked,
        inspected: view
            .inspector
            .as_ref()
            .map_or("", |inspector| inspector.id.as_str())
            .into(),
    }
}

/// A mode's mapped rows, and the manifest they came from.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Rows {
    pub(crate) manifest_rev: u64,
    pub(crate) rows: Vec<QueueRow>,
}

/// What a mode's `VecModel` must do to go from one [`Rows`] to the next.
#[derive(Debug, PartialEq)]
pub(crate) enum RowPatch {
    /// Another manifest, row count or row at an index: replace every row.
    Reset(Vec<QueueRow>),
    /// The rows that changed, by index; none for a publication that changed no row.
    Rows(Vec<(usize, QueueRow)>),
}

/// The patch from `old` to `new` (`view_patch.rs`'s row diff): a reset when the manifest, the row
/// count or any row's id changed, else only the rows that differ.
pub(crate) fn diff_rows(old: &Rows, new: &Rows) -> RowPatch {
    let replaced = old.manifest_rev != new.manifest_rev
        || old.rows.len() != new.rows.len()
        || old.rows.iter().zip(&new.rows).any(|(a, b)| a.id != b.id);
    if replaced {
        return RowPatch::Reset(new.rows.clone());
    }
    RowPatch::Rows(
        old.rows
            .iter()
            .zip(&new.rows)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(index, (_, row))| (index, row.clone()))
            .collect(),
    )
}

/// What the event loop writes into one mode's window state.
#[derive(Debug, Default)]
pub(crate) struct Update {
    pub(crate) head: Option<QueueHead>,
    pub(crate) rows: Option<RowPatch>,
    /// Whether a card replaced the rows, or another folder's view came, since the last take, so
    /// the list starts at the top again, even when a later head replaced that one before the take.
    pub(crate) to_top: bool,
    pub(crate) library: Option<Vec<LibraryItem>>,
    pub(crate) inspector: Option<InspectorUpdate>,
    /// The row to scroll the list to, once `rows` are applied: a revealed release's first.
    pub(crate) reveal: Option<usize>,
}

/// One mode's list between the Publisher and the window: its filter, the newest view, what the
/// model holds once every taken update is applied, and the update not yet taken. An update is
/// always against the applied rows, so a publication that replaces one not yet taken still
/// patches the model correctly.
pub(crate) struct ModeList {
    mode: Mode,
    filter: QueueFilter,
    pub(crate) view: Option<ModeView>,
    applied: Rows,
    pending: Option<Rows>,
    patch: Option<RowPatch>,
    head: Option<QueueHead>,
    /// Whether the newest head shows the rows rather than a card.
    rows_shown: bool,
    /// Whether a card replaced the rows, or another folder's view came, since the last take.
    to_top: bool,
    /// The Library and the Episode Inspector.
    inspector: InspectorList,
    /// A release the Library revealed, until rows mapped with every row shown are staged.
    reveal: Option<String>,
    /// Its first row's index, with those rows.
    reveal_row: Option<usize>,
}

impl ModeList {
    /// `mode`'s list, showing every row, before its first view.
    pub(crate) fn new(mode: Mode) -> Self {
        Self {
            mode,
            filter: QueueFilter::All,
            view: None,
            applied: Rows::default(),
            pending: None,
            patch: None,
            head: None,
            rows_shown: false,
            to_top: false,
            inspector: InspectorList::default(),
            reveal: None,
            reveal_row: None,
        }
    }

    pub(crate) fn filter(&self) -> QueueFilter {
        self.filter
    }

    /// Shows the rows `filter` matches once they are mapped again; a later click than a reveal
    /// not shown yet, so it cancels it.
    pub(crate) fn set_filter(&mut self, filter: QueueFilter) {
        self.filter = filter;
        self.cancel_reveal();
    }

    /// Drops a reveal not shown yet.
    pub(crate) fn cancel_reveal(&mut self) {
        self.reveal = None;
        self.reveal_row = None;
    }

    /// The filter to map `view`'s rows with, or `None` when they are the rows last mapped. The
    /// actor shares the rows of a mode it did not rebuild, so that is most publications.
    pub(crate) fn wants_rows(&self, view: &ModeView) -> Option<QueueFilter> {
        let same = self.view.as_ref().is_some_and(|last| {
            std::sync::Arc::ptr_eq(&last.rows, &view.rows)
                && last.manifest_rev == view.manifest_rev
                && last.library == view.library
        });
        (!same).then_some(self.filter)
    }

    /// Takes `view`, with its rows mapped by [`wants_rows`](Self::wants_rows)'s filter. Rows
    /// mapped with a filter that changed meanwhile are mapped again.
    pub(crate) fn put(&mut self, view: ModeView, mapped: Option<(QueueFilter, Vec<QueueRow>)>) {
        // Another folder's rows: a release revealed in the last one is not this one's, and they
        // start at the top, even with no scanning card published between (a cached folder).
        if self
            .view
            .as_ref()
            .is_some_and(|last| last.directory != view.directory)
        {
            self.cancel_reveal();
            self.to_top = true;
        }
        let rows = match mapped {
            Some((filter, rows)) if filter == self.filter => Some(rows),
            Some(_) => Some(map_rows(&view.rows, &view.library, self.filter)),
            None => None,
        };
        if let Some(rows) = rows {
            self.stage(Rows {
                manifest_rev: view.manifest_rev,
                rows,
            });
            self.resolve_reveal(&view);
        }
        self.set_head(map_head(&view, self.filter, self.rows_shown));
        self.inspector.put(&view, self.mode);
        self.view = Some(view);
    }

    /// Shows every row again and, once they are staged, scrolls the list to `release`'s first
    /// row (queue-view.js `revealRelease`). The caller maps the rows again (`Coalescer::remap`).
    pub(crate) fn reveal(&mut self, release: String) {
        self.filter = QueueFilter::All;
        self.reveal = Some(release);
    }

    /// The pending reveal's row in `view`, whose rows were just staged with the newest filter.
    fn resolve_reveal(&mut self, view: &ModeView) {
        if let Some(release) = self.reveal.take() {
            self.reveal_row = view.rows.iter().position(|row| row.release == release);
        }
    }

    /// Takes `rows`, `view`'s rows mapped with `filter`, unless the filter or the rows changed
    /// since. Returns whether it took them.
    pub(crate) fn put_filtered(
        &mut self,
        view: &ModeView,
        filter: QueueFilter,
        rows: Vec<QueueRow>,
    ) -> bool {
        if filter != self.filter || self.wants_rows(view).is_some() {
            return false;
        }
        let Some(newest) = &self.view else {
            return false;
        };
        let (head, manifest_rev) = (
            map_head(newest, filter, self.rows_shown),
            newest.manifest_rev,
        );
        self.set_head(head);
        self.stage(Rows { manifest_rev, rows });
        self.resolve_reveal(view);
        true
    }

    /// Stages `head`; a card that replaces the rows puts the list back at the top, once.
    fn set_head(&mut self, head: QueueHead) {
        self.to_top |= head.card_shown && self.rows_shown;
        self.rows_shown = !head.card_shown;
        self.head = Some(head);
    }

    fn stage(&mut self, rows: Rows) {
        self.patch = Some(diff_rows(&self.applied, &rows));
        self.pending = Some(rows);
    }

    /// The update to apply now; from here on the model holds its rows.
    pub(crate) fn take(&mut self) -> Update {
        if let Some(rows) = self.pending.take() {
            self.applied = rows;
        }
        let (library, inspector) = self.inspector.take();
        Update {
            head: self.head.take(),
            rows: self.patch.take(),
            to_top: std::mem::take(&mut self.to_top),
            library,
            inspector,
            reveal: self.reveal_row.take(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mkv_desktop_core::actor::ViewState;
    use std::sync::Arc;

    fn row(id: &str, release: &str, bucket: Option<Bucket>) -> Row {
        Row {
            id: id.into(),
            name: id.into(),
            release: release.into(),
            in_manifest: true,
            selected: bucket.is_some(),
            status: ItemStatus::Queued,
            progress: None,
            participant: false,
            label: "Queued".into(),
            tone: Tone::Mute,
            title: String::new(),
            meta: String::new(),
            meta_error: false,
            bucket,
        }
    }

    fn rows(manifest_rev: u64, ids: &[&str]) -> Rows {
        Rows {
            manifest_rev,
            rows: map_rows(
                &ids.iter()
                    .map(|id| row(id, ".", Some(Bucket::Todo)))
                    .collect::<Vec<_>>(),
                &[],
                QueueFilter::All,
            ),
        }
    }

    #[test]
    fn a_new_manifest_count_or_id_resets_the_rows() {
        let old = rows(1, &["a", "b"]);
        for new in [
            rows(2, &["a", "b"]),
            rows(1, &["a"]),
            rows(1, &["a", "c"]),
            rows(1, &["b", "a"]),
        ] {
            assert_eq!(diff_rows(&old, &new), RowPatch::Reset(new.rows.clone()));
        }
    }

    #[test]
    fn a_changed_row_alone_is_patched_and_an_unchanged_publication_writes_nothing() {
        let old = rows(1, &["a", "b", "c"]);
        assert_eq!(diff_rows(&old, &old.clone()), RowPatch::Rows(vec![]));
        let mut new = old.clone();
        new.rows[1].label = "40% · 3s".into();
        assert_eq!(
            diff_rows(&old, &new),
            RowPatch::Rows(vec![(1, new.rows[1].clone())])
        );
    }

    #[test]
    fn rows_map_their_status_selection_number_and_label() {
        let mut processing = row("Show - 01.mkv", ".", Some(Bucket::Todo));
        processing.status = ItemStatus::Processing;
        processing.progress = Some(40.0);
        processing.label = "40% · 3s".into();
        processing.tone = Tone::Info;
        let mut spinning = processing.clone();
        spinning.progress = None;
        let mut failed = row("b", ".", Some(Bucket::Failed));
        failed.status = ItemStatus::Error;
        failed.tone = Tone::Err;
        let mut dropped = failed.clone();
        dropped.selected = false;
        dropped.bucket = None;
        let mapped = map_rows(
            &[processing, spinning, failed, dropped],
            &[],
            QueueFilter::All,
        );
        let marks: Vec<_> = mapped
            .iter()
            .map(|row| (row.num.as_str(), row.mark, row.tone))
            .collect();
        assert_eq!(
            marks,
            [
                ("01", RowMark::Ring, RowTone::Info),
                ("02", RowMark::Spin, RowTone::Info),
                ("03", RowMark::Err, RowTone::Err),
                ("04", RowMark::None, RowTone::Mute),
            ]
        );
        assert_eq!(mapped[0].progress, 40.0);
        assert_eq!(mapped[0].check_label, "Include Show - 01.mkv in the batch");
        assert_eq!(mapped[3].label, NOT_IN_BATCH);
    }

    #[test]
    fn release_headers_count_the_rows_the_filter_shows() {
        let entry = |release: Option<&str>, group_label: &str| LibraryEntry {
            release: release.map(Into::into),
            label: group_label.into(),
            tooltip: String::new(),
            count: 0,
            group_label: group_label.into(),
        };
        let library = [entry(None, "Show"), entry(Some("S2"), "Season 2")];
        let queue = [
            row("a", ".", Some(Bucket::Todo)),
            row("b", "", Some(Bucket::Done)),
            row("c", "S2", Some(Bucket::Todo)),
            row("d", "S2", None),
            row("e", "S3", Some(Bucket::Todo)),
        ];
        let heads = |filter| {
            map_rows(&queue, &library, filter)
                .iter()
                .map(|row| (row.shown, row.head.to_string(), row.head_count, row.opens))
                .collect::<Vec<_>>()
        };
        let head = |label: &str, n| (true, label.to_string(), n, true);
        let under = |label: &str, n| (true, label.to_string(), n, false);
        let hidden = (false, String::new(), 0, false);
        // "." and "" are one group to the Library but two runs here, as in queue-view.js.
        assert_eq!(
            heads(QueueFilter::All),
            [
                head("SHOW", 1),
                head("SHOW", 1),
                head("SEASON 2", 2),
                under("SEASON 2", 2),
                head("EPISODES", 1)
            ]
        );
        assert_eq!(
            heads(QueueFilter::Todo),
            [
                head("SHOW", 1),
                hidden.clone(),
                head("SEASON 2", 1),
                hidden,
                head("EPISODES", 1)
            ]
        );
        // One release: no headers.
        assert!(map_rows(&queue[2..4], &library, QueueFilter::All)
            .iter()
            .all(|row| row.head.is_empty()));
    }

    #[test]
    fn the_head_says_what_the_list_shows_instead_of_rows() {
        let mut view = ViewState::new(None).remux;
        view.crumbs = vec!["media".into(), "anime".into(), "Show".into()];
        let head = map_head(&view, QueueFilter::All, false);
        assert_eq!(
            (head.crumbs_lead.as_str(), head.crumbs_end.as_str()),
            ("media › anime › ", "Show")
        );
        // An empty queue with no scan: the published empty card and the folder picker.
        assert!(head.card_shown && !head.card_spinner);
        assert_eq!(
            (head.card_title.as_str(), head.card_action.as_str()),
            ("No episodes yet", OPEN_FOLDER)
        );

        view.empty = None;
        view.scan = ScanView::Scanning {
            directory: "/media/Show".into(),
        };
        let head = map_head(&view, QueueFilter::All, false);
        assert!(head.card_shown && head.card_spinner);
        assert_eq!(head.card_title, SCANNING_TITLE);
        // A rescan of the folder whose rows are shown keeps them; after a card, it shows its own.
        view.rows = Arc::new(vec![row("a", ".", Some(Bucket::Todo))]);
        view.filters.all = 1;
        view.filters.todo = 1;
        view.directory = Some("/media/Show".into());
        assert!(!map_head(&view, QueueFilter::All, true).card_shown);
        assert!(map_head(&view, QueueFilter::All, false).card_shown);

        view.scan = ScanView::Failed;
        let head = map_head(&view, QueueFilter::Failed, true);
        assert_eq!(
            (head.card_title.as_str(), head.card_action.as_str()),
            (FAILED_TITLE, FAILED_ACTION)
        );
        // The filter shows none of the kept rows, but the card replaces them, so it says nothing.
        assert!(!head.filter_empty);
        view.scan = ScanView::Scanning {
            directory: "/media/Other".into(),
        };
        assert!(!map_head(&view, QueueFilter::Failed, true).filter_empty);
        // With the rows shown, it says so.
        view.scan = ScanView::Idle;
        assert!(map_head(&view, QueueFilter::Failed, true).filter_empty);
        assert!(!map_head(&view, QueueFilter::Todo, true).filter_empty);
    }

    /// queue-view.js keeps the rows through a rescan only when they were on screen: after a failed
    /// scan, a retry of the same folder shows the scanning card.
    #[test]
    fn a_retry_after_a_failed_scan_shows_the_scanning_card() {
        let mut view = ViewState::new(None).remux;
        view.rows = Arc::new(vec![row("a", ".", Some(Bucket::Todo))]);
        view.directory = Some("/media/Show".into());
        view.empty = None;
        let mut list = ModeList::new(Mode::Remux);
        let mut shown = |scan| {
            view.scan = scan;
            list.put(view.clone(), None);
            list.take().head.map(|head| head.card_shown)
        };
        let rescan = || ScanView::Scanning {
            directory: "/media/Show".into(),
        };
        assert_eq!(shown(ScanView::Idle), Some(false));
        assert_eq!(shown(rescan()), Some(false));
        assert_eq!(shown(ScanView::Failed), Some(true));
        assert_eq!(shown(rescan()), Some(true));
    }

    #[test]
    fn a_mode_list_patches_against_what_the_model_holds() {
        let mut view = ViewState::new(None).remux;
        view.rows = Arc::new(vec![
            row("a", ".", Some(Bucket::Todo)),
            row("b", ".", Some(Bucket::Done)),
        ]);
        view.manifest_rev = 1;
        let mut list = ModeList::new(Mode::Remux);
        let map = |list: &ModeList, view: &ModeView| {
            list.wants_rows(view)
                .map(|filter| (filter, map_rows(&view.rows, &view.library, filter)))
        };
        let mapped = map(&list, &view);
        list.put(view.clone(), mapped);
        assert!(matches!(list.take().rows, Some(RowPatch::Reset(rows)) if rows.len() == 2));

        // The same rows again map nothing, and update only the head.
        assert_eq!(list.wants_rows(&view), None);
        list.put(view.clone(), None);
        let update = list.take();
        assert!(update.head.is_some() && update.rows.is_none());

        // Two publications before a take: the second patches against the model, not the first.
        let mut first = (*view.rows).clone();
        first[0].label = "Done".into();
        view.rows = Arc::new(first);
        let mapped = map(&list, &view);
        list.put(view.clone(), mapped);
        let mut second = (*view.rows).clone();
        second[0].label = "Queued".into();
        second[1].label = "Failed".into();
        view.rows = Arc::new(second);
        let mapped = map(&list, &view);
        list.put(view.clone(), mapped);
        assert!(
            matches!(list.take().rows, Some(RowPatch::Rows(rows)) if rows.iter().map(|(i, _)| *i).collect::<Vec<_>>() == [1])
        );

        // A filter change patches the rows it hides and the filter a mapping raced with is redone.
        list.filter = QueueFilter::Done;
        let remapped = map_rows(&view.rows, &view.library, QueueFilter::Done);
        // Mapped with a filter that changed since: dropped, the newer remap stages its own.
        assert!(!list.put_filtered(&view, QueueFilter::All, remapped.clone()));
        assert!(list.put_filtered(&view, QueueFilter::Done, remapped));
        let update = list.take();
        assert_eq!(update.head.map(|head| head.filter), Some(QueueFilter::Done));
        assert!(
            matches!(update.rows, Some(RowPatch::Rows(rows)) if rows.iter().map(|(i, r)| (*i, r.shown)).collect::<Vec<_>>() == [(0, false)])
        );
        view.rows = Arc::new((*view.rows).clone());
        list.put(
            view.clone(),
            Some((
                QueueFilter::All,
                map_rows(&view.rows, &view.library, QueueFilter::All),
            )),
        );
        assert!(matches!(list.take().rows, Some(RowPatch::Rows(rows)) if rows.is_empty()));

        // A remap of a view whose rows a newer publication replaced is stale: it stages nothing,
        // or the old rows would stay under the new head.
        let old = view.clone();
        view.rows = Arc::new(vec![row("c", ".", Some(Bucket::Todo))]);
        let mapped = map(&list, &view);
        list.put(view.clone(), mapped);
        list.take();
        let filter = list.filter();
        let stale = map_rows(&old.rows, &old.library, filter);
        assert!(!list.put_filtered(&old, filter, stale));
        assert!(list.take().rows.is_none());
    }

    /// A card that replaces the rows puts the list at the top once, not again on every
    /// publication while it shows.
    #[test]
    fn a_card_scrolls_to_the_top_once() {
        let mut view = ViewState::new(None).remux;
        view.rows = Arc::new(vec![row("a", ".", Some(Bucket::Todo))]);
        view.directory = Some("/a".into());
        view.empty = None;
        let mut list = ModeList::new(Mode::Remux);
        list.put(view.clone(), None);
        list.take();
        let mut to_top = |scan| {
            view.scan = scan;
            list.put(view.clone(), None);
            list.take().to_top
        };
        assert!(!to_top(ScanView::Idle));
        assert!(to_top(ScanView::Failed));
        assert!(!to_top(ScanView::Failed));
    }

    /// A Library reveal scrolls once its rows are staged, unless a filter click, the folder's own
    /// entry or another folder came after it.
    #[test]
    fn a_later_filter_root_click_or_folder_cancels_a_reveal() {
        let mut view = ViewState::new(None).remux;
        view.rows = Arc::new(vec![
            row("a", ".", Some(Bucket::Todo)),
            row("b", "S2", Some(Bucket::Todo)),
        ]);
        view.directory = Some("/a".into());
        let mut list = ModeList::new(Mode::Remux);
        list.put(view.clone(), None);
        list.take();
        let mut revealed = |cancel: &dyn Fn(&mut ModeList, &ModeView)| {
            list.reveal("S2".into());
            cancel(&mut list, &view);
            let filter = list.filter();
            list.put_filtered(&view, filter, map_rows(&view.rows, &[], filter));
            list.take().reveal
        };
        assert_eq!(revealed(&|_, _| {}), Some(1));
        assert_eq!(
            revealed(&|list, _| list.set_filter(QueueFilter::Done)),
            None
        );
        assert_eq!(revealed(&|list, _| list.cancel_reveal()), None);
        let other_folder = |list: &mut ModeList, view: &ModeView| {
            let mut other = view.clone();
            other.directory = Some("/b".into());
            let rows = map_rows(&other.rows, &[], QueueFilter::All);
            list.put(other, Some((QueueFilter::All, rows)));
        };
        assert_eq!(revealed(&other_folder), None);
    }
}
