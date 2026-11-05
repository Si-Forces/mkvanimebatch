//! Asset discovery, indexing, and resolution for Release Directories: subtitles, chapters, and
//! covers through the Asset Resolver, fonts directories through the Release Preparer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::domain::{
    ancestor_dirs, detect_sub_lang, is_dialogue_track, lang_label, lang_mkv_tag, natural_sort_key,
    parse_episode, score_subtitle_category, season_is_exact, seasons_match,
    select_best_subtitle_track, EpisodeInfo, EpisodeType, NaturalSortToken, COVER_EXTENSIONS,
};
use crate::fonts::{find_fonts_dir, scan_fonts, FontInfo};
use crate::scan::{
    canonical_or_self, path_ext_lower, scan_assets, InspectedVideo, InspectionResult, ParsedFile,
    ScanCache,
};

fn ep_key(info: &EpisodeInfo) -> String {
    format!("{}|{}", info.ep_type, info.ep)
}

// ─── RELEASE PREPARATION ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PreparedRelease {
    pub fonts_dir: PathBuf,
    pub resolver: AssetResolver,
}

/// Prepares the Release Directories of one batch: resolves each one's fonts directory and
/// indexes its assets. Use one preparer per batch. Release Directories that share ancestors
/// would re-scan and re-index the same files, so the preparer caches those results: each
/// directory is indexed once, and every resolver that searches it shares that index. A
/// result equals the one a fresh preparer would return, except that a directory reached through
/// a second spelling keeps the path spelling of its first scan (see [`ScanCache`]).
#[derive(Debug, Default)]
pub struct ReleasePreparer {
    scan_cache: ScanCache,
    fonts_dir_cache: HashMap<PathBuf, PathBuf>,
    font_cache: HashMap<PathBuf, FontInfo>,
    /// Each indexed directory's segments, keyed by the spelling the directory was reached by.
    /// The contents come from the [`ScanCache`], so they follow its spelling rule.
    dir_indices: HashMap<PathBuf, DirIndex>,
}

impl ReleasePreparer {
    /// Resolves the fonts directory for `mkv_dir` and indexes the assets of every directory
    /// from `root_input_dir` down to it.
    ///
    /// Fonts come from `fonts_override`, else the fonts directory of the nearest directory (from
    /// `mkv_dir` upwards) whose fonts directory holds fonts, else the assets directory's fonts
    /// directory if it holds fonts, else `mkv_dir/fonts`.
    pub fn prepare(
        &mut self,
        mkv_dir: &Path,
        root_input_dir: &Path,
        fonts_override: Option<&Path>,
        assets_dir: Option<&Path>,
    ) -> PreparedRelease {
        let search_dirs = ancestor_dirs(mkv_dir, root_input_dir);
        let fonts_dir = match fonts_override {
            Some(over) => over.to_path_buf(),
            None => {
                let mut fonts_dir_with_fonts = |dir: &Path| -> Option<PathBuf> {
                    let candidate = find_fonts_dir(dir, &mut self.fonts_dir_cache);
                    let scanned = scan_fonts(&candidate, &mut self.font_cache);
                    (scanned.count > 0).then_some(candidate)
                };
                search_dirs
                    .iter()
                    .rev()
                    .find_map(|d| fonts_dir_with_fonts(d))
                    .or_else(|| assets_dir.and_then(&mut fonts_dir_with_fonts))
                    .unwrap_or_else(|| mkv_dir.join("fonts"))
            }
        };
        let resolver = AssetResolver::from_dir_indices(&search_dirs, assets_dir, |dir| {
            if let Some(hit) = self.dir_indices.get(dir) {
                return hit.clone();
            }
            let built = DirIndex::build(dir, &mut self.scan_cache, None);
            self.dir_indices.insert(dir.to_path_buf(), built.clone());
            built
        });
        PreparedRelease {
            fonts_dir,
            resolver,
        }
    }

    /// The fonts found in `fonts_dir`, as the attachment arguments `mkvmerge` takes.
    pub fn fonts(&mut self, fonts_dir: &Path) -> FontInfo {
        scan_fonts(fonts_dir, &mut self.font_cache)
    }
}

// ─── ASSET INDICES (Subtitles, Chapters, Covers) ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubEntry {
    file: ParsedFile,
    classification: SubClassification,
}

/// Subtitle scoring fields derived purely from one file's stem and extension.
///
/// `AssetResolver::discover` callers that share one cache across calls derive them once per
/// file. A `ReleasePreparer` keeps no such cache: it indexes each directory spelling once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubClassification {
    pub lang: String,
    pub category_score: i32,
    pub is_dialogue: bool,
}

fn classify_sub(sub_file: &ParsedFile) -> SubClassification {
    let lang = detect_sub_lang(&sub_file.stem);
    let is_ass = sub_file
        .path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("ass"));
    let category_score = score_subtitle_category(&lang, is_ass);
    let is_dialogue = is_dialogue_track(&sub_file.stem);
    SubClassification {
        lang,
        category_score,
        is_dialogue,
    }
}

/// `path`'s file name, lossily, or empty when it has none.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The segment `add` appends to: the last one, copied first when another index shares it, so
/// a shared segment (a Release Preparer's indexed directory, or a clone's) is never written.
fn owned_tail<T: Clone + Default>(segments: &mut Vec<Arc<T>>) -> &mut T {
    if segments.is_empty() {
        segments.push(Arc::default());
    }
    Arc::make_mut(
        segments
            .last_mut()
            .expect("a tail segment was just ensured"),
    )
}

/// One directory's subtitles (or, for an index built with `SubIndex::add`, every added one).
#[derive(Debug, Clone, Default)]
struct SubSegment {
    index: HashMap<String, Vec<SubEntry>>,
    index_paths: HashMap<String, HashSet<PathBuf>>,
    directories: Vec<PathBuf>,
    has_unparsed: bool,
}

/// Subtitles per episode key, as a list of segments in index order. Lookups read the segments
/// as one sequence, so splitting the same files into segments never changes a result.
#[derive(Debug, Clone, Default)]
pub struct SubIndex {
    segments: Vec<Arc<SubSegment>>,
}

impl SubIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, sub_file: ParsedFile) {
        owned_tail(&mut self.segments).add_classified(sub_file, None);
    }

    /// The entries for `key` across every segment, in index order. A path indexed in two
    /// segments (one directory indexed twice) appears twice; the later copy has a larger index
    /// and an otherwise equal priority, so `lookup_ranked` never picks it.
    fn entries<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a SubEntry> {
        self.segments
            .iter()
            .filter_map(move |segment| segment.index.get(key))
            .flatten()
    }

    fn lookup_ranked(
        &self,
        parsed: &EpisodeInfo,
        rank: impl Fn(&Path) -> Option<usize>,
    ) -> Option<(PathBuf, String)> {
        self.entries(&ep_key(parsed))
            .enumerate()
            .filter_map(|(idx, entry)| {
                let file_parsed = entry.file.parsed.as_ref()?;
                if !seasons_match(&parsed.season, &file_parsed.season) {
                    return None;
                }
                let distance = rank(entry.file.path.parent()?)?;
                Some((
                    (
                        std::cmp::Reverse(distance),
                        // Within a directory an exact season beats an unknown one.
                        season_is_exact(&parsed.season, &file_parsed.season),
                        entry.classification.category_score,
                        entry.classification.is_dialogue,
                        std::cmp::Reverse(entry.file.name.len()),
                        std::cmp::Reverse(idx),
                    ),
                    entry,
                ))
            })
            .max_by_key(|(priority, _)| *priority)
            .map(|(_, best)| (best.file.path.clone(), best.classification.lang.clone()))
    }

    fn lookup_same_dir(&self, parsed: &EpisodeInfo, dir: &Path) -> Option<(PathBuf, String)> {
        self.lookup_ranked(parsed, |candidate| (candidate == dir).then_some(0))
    }

    fn lookup_ancestors(&self, parsed: &EpisodeInfo, dir: &Path) -> Option<(PathBuf, String)> {
        self.lookup_ranked(parsed, |candidate| {
            dir.ancestors()
                .skip(1)
                .position(|ancestor| ancestor == candidate)
        })
    }

    /// Ranks directories by first appearance in index order: the assets directory before its
    /// parent.
    fn lookup_fallback(&self, parsed: &EpisodeInfo) -> Option<(PathBuf, String)> {
        self.lookup_ranked(parsed, |candidate| {
            self.segments
                .iter()
                .flat_map(|segment| &segment.directories)
                .position(|dir| dir == candidate)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.segments
            .iter()
            .all(|segment| segment.index.is_empty() && !segment.has_unparsed)
    }
}

impl SubSegment {
    /// Adds a subtitle, reusing `classification` from the caller's cache when it has one.
    /// Pass `None` to derive it on the spot.
    fn add_classified(&mut self, sub_file: ParsedFile, classification: Option<SubClassification>) {
        let Some(parsed) = &sub_file.parsed else {
            self.has_unparsed = true;
            return;
        };

        let key = ep_key(parsed);
        let paths = self.index_paths.entry(key.clone()).or_default();
        if paths.contains(&sub_file.path) {
            return;
        }
        paths.insert(sub_file.path.clone());
        if let Some(dir) = sub_file.path.parent() {
            if !self.directories.iter().any(|existing| existing == dir) {
                self.directories.push(dir.to_path_buf());
            }
        }
        let classification = classification.unwrap_or_else(|| classify_sub(&sub_file));
        self.index.entry(key).or_default().push(SubEntry {
            file: sub_file,
            classification,
        });
    }
}

// ─── CHAPTER INDEX ───────────────────────────────────────────────────────────

/// One directory's chapter files (or, for an index built with `ChapterIndex::add`, every added
/// one).
#[derive(Debug, Clone, Default)]
struct ChapterSegment {
    /// Every chapter file per episode key with its season, in add order.
    index: HashMap<String, Vec<(PathBuf, String)>>,
    unmatched: Vec<PathBuf>,
}

impl ChapterSegment {
    fn add(&mut self, xml_file: &ParsedFile) {
        if let Some(parsed) = &xml_file.parsed {
            self.index
                .entry(ep_key(parsed))
                .or_default()
                .push((xml_file.path.clone(), parsed.season.clone()));
        } else {
            self.unmatched.push(xml_file.path.clone());
        }
    }
}

/// Chapter files as a list of segments in index order. The release-tree index is filled
/// nearest directory first; the `--assets-dir` index holds the assets directory before its
/// parent (scan order, not ancestor distance). Lookups read the segments as one sequence.
#[derive(Debug, Clone, Default)]
pub struct ChapterIndex {
    segments: Vec<Arc<ChapterSegment>>,
}

impl ChapterIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Indexes `xml_file` as chapter XML without re-reading it: `scan_assets` is the only
    /// place a file is sniffed for chapter content. Only the path and season are kept.
    ///
    /// Contract: each file is added once, the first-indexed directory first (release tree:
    /// deepest first; `--assets-dir`: the assets directory before its parent).
    /// `lookup_with_warning` takes the first compatible file's directory as the nearest one
    /// and does not dedupe.
    pub fn add(&mut self, xml_file: &ParsedFile) {
        owned_tail(&mut self.segments).add(xml_file);
    }

    pub fn lookup(&self, parsed: &EpisodeInfo) -> Option<PathBuf> {
        self.lookup_with_warning(parsed).map(|(path, _)| path)
    }

    /// The chapter file for `parsed` and the warning text for the compatible files beside it
    /// (same episode, season equal or unknown) that were passed over. The nearest directory
    /// with a compatible file wins, then an exact season over an unknown one, then index order.
    /// A file of another known season is no candidate, so it is never named.
    fn lookup_with_warning(&self, parsed: &EpisodeInfo) -> Option<(PathBuf, Option<String>)> {
        let key = ep_key(parsed);
        let compatible = self
            .segments
            .iter()
            .filter_map(|segment| segment.index.get(&key))
            .flatten()
            .filter(|(_, season)| seasons_match(&parsed.season, season));
        // The first compatible file is in the nearest directory, so `beside` is never empty.
        let dir = compatible.clone().next()?.0.parent();
        let beside: Vec<&(PathBuf, String)> = compatible
            .filter(|(path, _)| path.parent() == dir)
            .collect();
        let chosen = beside
            .iter()
            .find(|(_, season)| season_is_exact(&parsed.season, season))
            .unwrap_or(&beside[0]);
        let ignored: Vec<String> = beside
            .iter()
            .filter(|candidate| candidate.0 != chosen.0)
            .map(|(path, _)| format!("'{}'", file_name_of(path)))
            .collect();
        let warning = (!ignored.is_empty()).then(|| {
            format!(
                "More than one chapter file for this episode in '{}': using '{}', ignoring {}",
                dir.unwrap_or(Path::new("")).display(),
                file_name_of(&chosen.0),
                ignored.join(", ")
            )
        });
        Some((chosen.0.clone(), warning))
    }

    /// Chapter files whose names carry no episode number, in index order.
    pub fn unmatched(&self) -> impl Iterator<Item = &Path> {
        self.segments
            .iter()
            .flat_map(|segment| segment.unmatched.iter().map(PathBuf::as_path))
    }

    /// The chapter file with no episode number directly in `dir`, when `dir` holds exactly one:
    /// the only file a leftover chapter claim can hand out there (ADR-0004). A file's directory
    /// that is spelled differently from `dir` is compared canonically, as the scan cache shares
    /// one directory's index under the first spelling that reached it.
    pub fn sole_unmatched_in(&self, dir: &Path) -> Option<&Path> {
        let canonical_dir = canonical_or_self(dir);
        let mut own = self.unmatched().filter(|path| {
            path.parent()
                .is_some_and(|parent| parent == dir || canonical_or_self(parent) == canonical_dir)
        });
        match (own.next(), own.next()) {
            (Some(sole), None) => Some(sole),
            _ => None,
        }
    }
}

// ─── COVER INDEX ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CoverKind {
    Underscore,
    Dot,
    Adjacent,
    Cover,
    Folder,
}

#[derive(Debug, Clone)]
struct CoverEntry {
    file: ParsedFile,
    episode: Option<EpisodeInfo>,
    kind: CoverKind,
    natural_name: Vec<NaturalSortToken>,
}

/// Orders covers by `(directory rank, open)`, then by kind, name and path. An open cover (an
/// episode cover with a non-exact season, or any other kind) ranks after an exact one in the
/// same directory. Two covers compare equal only when they share a path.
fn compare_ranked_covers(
    a_rank: (usize, bool),
    a: &CoverEntry,
    b_rank: (usize, bool),
    b: &CoverEntry,
) -> std::cmp::Ordering {
    a_rank
        .cmp(&b_rank)
        .then_with(|| a.kind.cmp(&b.kind))
        .then_with(|| a.natural_name.cmp(&b.natural_name))
        .then_with(|| a.file.path.cmp(&b.file.path))
}

/// One directory's covers (or, for an index built with `CoverIndex::add`, every added one).
/// The `*_entries` maps hold indices into `entries`.
#[derive(Debug, Clone, Default)]
struct CoverSegment {
    entries: Vec<CoverEntry>,
    paths: HashSet<PathBuf>,
    /// Each directory's rank within this segment, by first appearance.
    directory_ranks: HashMap<PathBuf, usize>,
    episode_entries: HashMap<EpisodeType, HashMap<String, HashMap<PathBuf, Vec<usize>>>>,
    adjacent_entries: HashMap<PathBuf, HashMap<String, Vec<usize>>>,
    directory_entries: HashMap<PathBuf, Vec<usize>>,
}

impl CoverSegment {
    fn add(&mut self, image_file: ParsedFile) {
        let ext = path_ext_lower(&image_file.path);
        if !COVER_EXTENSIONS.contains(&ext.as_str()) || !self.paths.insert(image_file.path.clone())
        {
            return;
        }

        let stem = image_file.stem.to_lowercase();
        let (kind, episode) = if stem == "cover" {
            (CoverKind::Cover, None)
        } else if stem == "folder" {
            (CoverKind::Folder, None)
        } else if stem.ends_with("_cover") || stem.ends_with(".cover") {
            let episode_stem = &image_file.stem[..image_file.stem.len() - "_cover".len()];
            let episode = parse_episode(episode_stem);
            let kind = if episode.is_some() {
                if stem.ends_with("_cover") {
                    CoverKind::Underscore
                } else {
                    CoverKind::Dot
                }
            } else {
                CoverKind::Adjacent
            };
            (kind, episode)
        } else {
            (CoverKind::Adjacent, None)
        };

        let entry_index = self.entries.len();
        if let Some(dir) = image_file.path.parent() {
            let dir = dir.to_path_buf();
            let next_rank = self.directory_ranks.len();
            self.directory_ranks.entry(dir.clone()).or_insert(next_rank);
            match kind {
                CoverKind::Underscore | CoverKind::Dot => {
                    if let Some(episode) = &episode {
                        self.episode_entries
                            .entry(episode.ep_type)
                            .or_default()
                            .entry(episode.ep.clone())
                            .or_default()
                            .entry(dir)
                            .or_default()
                            .push(entry_index);
                    }
                }
                CoverKind::Adjacent => {
                    self.adjacent_entries
                        .entry(dir)
                        .or_default()
                        .entry(image_file.stem.clone())
                        .or_default()
                        .push(entry_index);
                }
                CoverKind::Cover | CoverKind::Folder => {
                    self.directory_entries
                        .entry(dir)
                        .or_default()
                        .push(entry_index);
                }
            }
        }
        self.entries.push(CoverEntry {
            natural_name: natural_sort_key(&image_file.name),
            file: image_file,
            episode,
            kind,
        });
    }
}

/// Covers as a list of segments in index order. The ranking is a total order up to a shared
/// path, so the order candidates are gathered in never changes a lookup.
#[derive(Debug, Clone, Default)]
pub struct CoverIndex {
    segments: Vec<Arc<CoverSegment>>,
}

impl CoverIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, image_file: ParsedFile) {
        owned_tail(&mut self.segments).add(image_file);
    }

    /// `dir`'s rank by first appearance in index order: its rank in the first segment that
    /// holds it, after every directory of the segments before.
    fn directory_rank(&self, dir: &Path) -> Option<usize> {
        let mut offset = 0;
        for segment in &self.segments {
            if let Some(rank) = segment.directory_ranks.get(dir) {
                return Some(offset + rank);
            }
            offset += segment.directory_ranks.len();
        }
        None
    }

    fn lookup_ranked<'a>(
        video: &ParsedFile,
        candidates: impl Iterator<Item = (&'a CoverEntry, usize)>,
    ) -> Option<PathBuf> {
        candidates
            .filter_map(|(entry, distance)| {
                // Only episode covers carry a season; every other kind counts as open.
                let mut open = true;
                if matches!(entry.kind, CoverKind::Underscore | CoverKind::Dot) {
                    let (target, candidate) = video.parsed.as_ref().zip(entry.episode.as_ref())?;
                    if !seasons_match(&target.season, &candidate.season) {
                        return None;
                    }
                    open = !season_is_exact(&target.season, &candidate.season);
                }
                Some(((distance, open), entry))
            })
            .min_by(|(a_rank, a), (b_rank, b)| compare_ranked_covers(*a_rank, a, *b_rank, b))
            .map(|(_, entry)| entry.file.path.clone())
    }

    pub fn lookup(&self, video: &ParsedFile) -> Option<PathBuf> {
        let parent = video.path.parent()?;
        let parsed = video.parsed.as_ref();
        let directory_candidates =
            parent
                .ancestors()
                .enumerate()
                .flat_map(move |(distance, dir)| {
                    self.segments.iter().flat_map(move |segment| {
                        let episodes = parsed.and_then(|parsed| {
                            segment
                                .episode_entries
                                .get(&parsed.ep_type)?
                                .get(&parsed.ep)?
                                .get(dir)
                        });
                        let covers = segment.directory_entries.get(dir);
                        episodes
                            .into_iter()
                            .chain(covers)
                            .flatten()
                            .map(move |&index| (&segment.entries[index], distance))
                    })
                });
        let adjacent_candidates = self.segments.iter().flat_map(|segment| {
            segment
                .adjacent_entries
                .get(parent)
                .and_then(|entries| entries.get(video.stem.as_str()))
                .into_iter()
                .flatten()
                .map(move |&index| (&segment.entries[index], 0))
        });
        Self::lookup_ranked(video, directory_candidates.chain(adjacent_candidates))
    }

    /// Ranks covers by directory rank. Every directory cover is a candidate: the best of them
    /// is the directory cover any video may take, and the others never beat it.
    fn lookup_fallback(&self, video: &ParsedFile) -> Option<PathBuf> {
        let parsed = video.parsed.as_ref();
        let parent = video.path.parent();
        let candidates = self
            .segments
            .iter()
            .flat_map(move |segment| {
                let episodes = parsed
                    .and_then(|parsed| {
                        segment
                            .episode_entries
                            .get(&parsed.ep_type)?
                            .get(&parsed.ep)
                    })
                    .into_iter()
                    .flat_map(|by_dir| by_dir.values());
                let adjacent = parent
                    .and_then(|dir| segment.adjacent_entries.get(dir))
                    .and_then(|entries| entries.get(video.stem.as_str()));
                episodes
                    .chain(adjacent)
                    .chain(segment.directory_entries.values())
                    .flatten()
                    .map(move |&index| &segment.entries[index])
            })
            .filter_map(|entry| Some((entry, self.directory_rank(entry.file.path.parent()?)?)));
        Self::lookup_ranked(video, candidates)
    }
}

// ─── ASSET RESOLVER ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAssets {
    pub subtitle_file: Option<PathBuf>,
    pub embedded_sub_id: Option<u32>,
    pub subtitle_lang: String,
    pub subtitle_label: String,
    pub subtitle_mkv_lang: String,
    pub matched_subtitle: bool,
    pub chapter_file: Option<PathBuf>,
    /// Warning text for same-directory chapter files ignored in favor of `chapter_file` that
    /// match the video's season (equal or unknown); the caller logs it as a warning line.
    pub chapter_warning: Option<String>,
    pub cover_file: Option<PathBuf>,
    pub skip_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct SubResolution {
    subtitle_file: Option<PathBuf>,
    embedded_sub_id: Option<u32>,
    lang: String,
    matched: bool,
    skip_reason: Option<String>,
}

impl SubResolution {
    fn from_assigned(assigned: &Path) -> Self {
        let stem = assigned
            .file_stem()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();
        Self::external(assigned.to_path_buf(), detect_sub_lang(&stem))
    }

    fn external(path: PathBuf, lang: String) -> Self {
        Self {
            subtitle_file: Some(path),
            embedded_sub_id: None,
            lang,
            matched: true,
            skip_reason: None,
        }
    }

    fn embedded(track_id: u32, lang: String) -> Self {
        Self {
            subtitle_file: None,
            embedded_sub_id: Some(track_id),
            lang,
            matched: true,
            skip_reason: None,
        }
    }

    fn unmatched(reason: impl Into<String>) -> Self {
        Self {
            subtitle_file: None,
            embedded_sub_id: None,
            lang: "und".to_string(),
            matched: false,
            skip_reason: Some(reason.into()),
        }
    }
}

/// One directory's subtitle, chapter and cover segments. Immutable once built, so every resolver
/// whose search or fallback directories include the directory can share them.
#[derive(Debug, Clone)]
struct DirIndex {
    subs: Arc<SubSegment>,
    chapters: Arc<ChapterSegment>,
    covers: Arc<CoverSegment>,
}

impl DirIndex {
    /// Indexes the assets `scan_assets` finds directly in `dir`, in scan order, reading and
    /// filling `sub_classification_cache` when given one.
    fn build(
        dir: &Path,
        scan_cache: &mut ScanCache,
        mut sub_classification_cache: Option<&mut HashMap<PathBuf, SubClassification>>,
    ) -> Self {
        let assets = scan_assets(dir, Some(scan_cache));
        let mut subs = SubSegment::default();
        for sub in &assets.subs {
            let classification = match sub_classification_cache.as_deref_mut() {
                Some(cache) if sub.parsed.is_some() => Some(match cache.get(&sub.path) {
                    Some(hit) => hit.clone(),
                    None => {
                        let fresh = classify_sub(sub);
                        cache.insert(sub.path.clone(), fresh.clone());
                        fresh
                    }
                }),
                _ => None,
            };
            subs.add_classified(sub.clone(), classification);
        }
        let mut chapters = ChapterSegment::default();
        for chap in &assets.chapters {
            chapters.add(chap);
        }
        let mut covers = CoverSegment::default();
        for cov in &assets.covers {
            covers.add(cov.clone());
        }
        Self {
            subs: Arc::new(subs),
            chapters: Arc::new(chapters),
            covers: Arc::new(covers),
        }
    }

    /// Appends this directory's segments after the ones already in the indices.
    fn append_to(self, subs: &mut SubIndex, chapters: &mut ChapterIndex, covers: &mut CoverIndex) {
        subs.segments.push(self.subs);
        chapters.segments.push(self.chapters);
        covers.segments.push(self.covers);
    }
}

#[derive(Debug, Clone, Default)]
pub struct AssetResolver {
    pub sub_index: SubIndex,
    pub chap_index: ChapterIndex,
    pub cover_index: CoverIndex,
    pub fallback_sub_index: Option<SubIndex>,
    pub fallback_chap_index: Option<ChapterIndex>,
    pub fallback_cover_index: Option<CoverIndex>,
}

impl AssetResolver {
    pub fn has_subtitles(&self) -> bool {
        !self.sub_index.is_empty()
            || self
                .fallback_sub_index
                .as_ref()
                .is_some_and(|fb| !fb.is_empty())
    }

    /// Indexes the assets of `search_dirs` (ordered root first, indexed deepest first) and,
    /// when given, of `assets_dir` and its parent as the fallback indices. Directory-level
    /// font resolution is owned by `ReleasePreparer`, which also shares each directory's index
    /// across the resolvers of a batch; this builds every directory's index afresh.
    pub fn discover(
        search_dirs: &[PathBuf],
        assets_dir: Option<&Path>,
        scan_cache: &mut ScanCache,
        sub_classification_cache: &mut HashMap<PathBuf, SubClassification>,
    ) -> Self {
        Self::from_dir_indices(search_dirs, assets_dir, |dir| {
            DirIndex::build(dir, scan_cache, Some(&mut *sub_classification_cache))
        })
    }

    /// [`Self::discover`] with each directory's index taken from `index_dir`.
    fn from_dir_indices(
        search_dirs: &[PathBuf],
        assets_dir: Option<&Path>,
        mut index_dir: impl FnMut(&Path) -> DirIndex,
    ) -> Self {
        let mut sub_index = SubIndex::new();
        let mut chap_index = ChapterIndex::new();
        let mut cover_index = CoverIndex::new();

        for d in search_dirs.iter().rev() {
            index_dir(d).append_to(&mut sub_index, &mut chap_index, &mut cover_index);
        }

        let mut fallback_sub_index = None;
        let mut fallback_chap_index = None;
        let mut fallback_cover_index = None;

        if let Some(assets) = assets_dir {
            let mut fb_sub = SubIndex::new();
            let mut fb_chap = ChapterIndex::new();
            let mut fb_cov = CoverIndex::new();

            let parent = assets
                .parent()
                .filter(|&parent| parent != assets && parent.is_dir());
            for fb_dir in std::iter::once(assets).chain(parent) {
                index_dir(fb_dir).append_to(&mut fb_sub, &mut fb_chap, &mut fb_cov);
            }

            fallback_sub_index = Some(fb_sub);
            fallback_chap_index = Some(fb_chap);
            fallback_cover_index = Some(fb_cov);
        }

        Self {
            sub_index,
            chap_index,
            cover_index,
            fallback_sub_index,
            fallback_chap_index,
            fallback_cover_index,
        }
    }

    fn resolve_sub(
        &self,
        video: &ParsedFile,
        parsed: &EpisodeInfo,
        inspected: Option<&InspectedVideo>,
        assigned_subtitle: Option<&Path>,
    ) -> SubResolution {
        // Explicit human assignment overrides automatic matching (ADR-0004).
        if let Some(assigned) = assigned_subtitle {
            return SubResolution::from_assigned(assigned);
        }

        // Same-directory external subtitles precede embedded tracks.
        if let Some((sub_path, lang)) = video
            .path
            .parent()
            .and_then(|dir| self.sub_index.lookup_same_dir(parsed, dir))
        {
            return SubResolution::external(sub_path, lang);
        }

        // Embedded track from source video (ADR-0004).
        if let Some(insp) = inspected {
            if let Some((track_id, lang)) =
                select_best_subtitle_track(&insp.subtitle_tracks(), None)
            {
                return SubResolution::embedded(track_id, lang);
            }
        }
        // Matching ancestors follow embedded tracks, regardless of language score.
        if let Some((sub_path, lang)) = video
            .path
            .parent()
            .and_then(|dir| self.sub_index.lookup_ancestors(parsed, dir))
        {
            return SubResolution::external(sub_path, lang);
        }

        // Explicit assets directory is the final subtitle source.
        if let Some(fb_sub) = &self.fallback_sub_index {
            if let Some((sub_path, lang)) = fb_sub.lookup_fallback(parsed) {
                return SubResolution::external(sub_path, lang);
            }
        }

        let skip_reason = if self.has_subtitles() {
            "subtitle files found but none matched"
        } else {
            "no subtitle files found"
        };
        SubResolution::unmatched(skip_reason)
    }

    /// Release-tree cover for `video`; the assets-directory fallback applies only when the
    /// source carries no cover of its own.
    pub fn resolve_cov(&self, video: &ParsedFile, has_source_cover: bool) -> Option<PathBuf> {
        if let Some(cov) = self.cover_index.lookup(video) {
            return Some(cov);
        }
        if !has_source_cover {
            return self
                .fallback_cover_index
                .as_ref()
                .and_then(|fb| fb.lookup_fallback(video));
        }
        None
    }

    fn resolve_chap(
        &self,
        parsed: &EpisodeInfo,
        has_source_chapters: bool,
        claimed_chapter: Option<&Path>,
    ) -> Option<(PathBuf, Option<String>)> {
        if let Some(chap) = self.chap_index.lookup_with_warning(parsed) {
            return Some(chap);
        }
        if !has_source_chapters {
            if let Some(claimed) = claimed_chapter {
                return Some((claimed.to_path_buf(), None));
            }
            if let Some(fb_chap) = &self.fallback_chap_index {
                return fb_chap.lookup_with_warning(parsed);
            }
        }
        None
    }

    /// Resolves the assets of `video`. `assigned_subtitle` is taken as given: the Remux Plan has
    /// already checked that it exists.
    pub fn resolve_for(
        &self,
        video: &ParsedFile,
        inspected: Option<&InspectedVideo>,
        claimed_chapter: Option<&Path>,
        assigned_subtitle: Option<&Path>,
    ) -> ResolvedAssets {
        let has_source_chapters = inspected.is_some_and(|i| i.has_chapters());
        let has_source_cover = inspected.is_some_and(|i| i.cover_attachment().is_some());

        let (sub_res, chapter) = if let Some(parsed) = &video.parsed {
            let sub = self.resolve_sub(video, parsed, inspected, assigned_subtitle);
            let chap = self.resolve_chap(parsed, has_source_chapters, claimed_chapter);
            (sub, chap)
        } else {
            let sub = match assigned_subtitle {
                Some(assigned) => SubResolution::from_assigned(assigned),
                None => SubResolution::unmatched("unparsed episode"),
            };
            (sub, None)
        };
        let (chapter_file, chapter_warning) = match chapter {
            Some((file, warning)) => (Some(file), warning),
            None => (None, None),
        };
        let cover_file = self.resolve_cov(video, has_source_cover);

        let subtitle_label = lang_label(&sub_res.lang).to_string();
        let subtitle_mkv_lang = lang_mkv_tag(&sub_res.lang).to_string();

        ResolvedAssets {
            subtitle_file: sub_res.subtitle_file,
            embedded_sub_id: sub_res.embedded_sub_id,
            subtitle_lang: sub_res.lang,
            subtitle_label,
            subtitle_mkv_lang,
            matched_subtitle: sub_res.matched,
            chapter_file,
            chapter_warning,
            cover_file,
            skip_reason: sub_res.skip_reason,
        }
    }
}

/// Assigns the Release Directory's only episode-less chapter file to its only episode that has
/// neither a matching chapter file nor embedded chapters. Any ambiguity leaves it unclaimed.
///
/// `videos` are every discovered video of one Release Directory, selected or not, so a subset
/// run judges the directory as a full run does; the batch probes the unselected ones of a
/// directory where a claim is possible. A video absent from `inspection_results`, or whose probe
/// failed, counts as having no embedded chapters. Only a file directly in the videos' directory
/// counts: an ancestor's or an `--assets-dir` file is never claimed (ADR-0004).
pub fn claim_leftover_chapters(
    videos: &[ParsedFile],
    inspection_results: &HashMap<PathBuf, InspectionResult>,
    resolver: &AssetResolver,
) -> HashMap<PathBuf, PathBuf> {
    let Some(sole_chapter) = videos
        .first()
        .and_then(|video| video.path.parent())
        .and_then(|dir| resolver.chap_index.sole_unmatched_in(dir))
    else {
        return HashMap::new();
    };

    let mut candidates = videos.iter().filter(|video| {
        let Some(parsed) = &video.parsed else {
            return false;
        };
        resolver.chap_index.lookup(parsed).is_none()
            && !inspection_results
                .get(&video.path)
                .and_then(|r| r.video.as_ref())
                .is_some_and(|v| v.has_chapters())
    });

    match (candidates.next(), candidates.next()) {
        (Some(video), None) => HashMap::from([(video.path.clone(), sole_chapter.to_path_buf())]),
        _ => HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::scan::create_parsed_file;

    #[test]
    fn test_discover_does_not_classify_unparsed_subtitles() {
        let tmp = tempfile::tempdir().unwrap();
        let season = tmp.path().join("Season 01");
        fs::create_dir_all(&season).unwrap();
        fs::write(season.join("credits_roll.ass"), b"[Script Info]\n").unwrap();
        fs::write(
            season.join("extras.srt"),
            b"1\n00:00:01 --> 00:00:02\nExtra\n",
        )
        .unwrap();

        let mut cache = HashMap::new();
        let resolver = AssetResolver::discover(
            std::slice::from_ref(&season),
            None,
            &mut ScanCache::default(),
            &mut cache,
        );

        // Unparsed subtitles are not eagerly classified, but they still count as discovered.
        assert!(cache.is_empty());
        assert!(resolver.has_subtitles());
    }

    #[test]
    fn test_adding_to_a_clone_leaves_the_original_index_alone() {
        let dir = Path::new("/anime");
        let file = |name: &str| create_parsed_file(dir.join(name));
        let episode = file("Show - 01.mkv");
        let parsed = episode.parsed.clone().unwrap();

        let mut subs = SubIndex::new();
        subs.add(file("Show - 01.ass"));
        let mut chapters = ChapterIndex::new();
        chapters.add(&file("Show - 01.xml"));
        let mut covers = CoverIndex::new();
        covers.add(file("cover.jpg"));
        let (sub_before, chapter_before, cover_before) = (
            subs.lookup_same_dir(&parsed, dir),
            chapters.lookup_with_warning(&parsed),
            covers.lookup_fallback(&episode),
        );

        let (mut more_subs, mut more_chapters, mut more_covers) =
            (subs.clone(), chapters.clone(), covers.clone());
        more_subs.add(file("Show - 01.ind.ass"));
        more_chapters.add(&file("Show - 01.alt.xml"));
        more_covers.add(file("Show - 01_cover.jpg"));

        // The originals still answer as before.
        assert_eq!(subs.lookup_same_dir(&parsed, dir), sub_before);
        assert_eq!(chapters.lookup_with_warning(&parsed), chapter_before);
        assert_eq!(covers.lookup_fallback(&episode), cover_before);
        // The clones answer like one index built with every add.
        let mut all_subs = SubIndex::new();
        all_subs.add(file("Show - 01.ass"));
        all_subs.add(file("Show - 01.ind.ass"));
        let mut all_chapters = ChapterIndex::new();
        all_chapters.add(&file("Show - 01.xml"));
        all_chapters.add(&file("Show - 01.alt.xml"));
        let mut all_covers = CoverIndex::new();
        all_covers.add(file("cover.jpg"));
        all_covers.add(file("Show - 01_cover.jpg"));
        assert_eq!(
            more_subs.lookup_same_dir(&parsed, dir),
            all_subs.lookup_same_dir(&parsed, dir)
        );
        assert_eq!(
            more_chapters.lookup_with_warning(&parsed),
            all_chapters.lookup_with_warning(&parsed)
        );
        assert_eq!(
            more_covers.lookup_fallback(&episode),
            all_covers.lookup_fallback(&episode)
        );
        // Not vacuous: each add changed its clone's answer.
        assert!(more_chapters
            .lookup_with_warning(&parsed)
            .unwrap()
            .1
            .is_some());
        assert_eq!(
            more_covers.lookup_fallback(&episode),
            Some(dir.join("Show - 01_cover.jpg"))
        );
        assert_ne!(more_subs.lookup_same_dir(&parsed, dir), sub_before);
    }

    /// A library root and an assets directory shared by two seasons, with ties inside and
    /// across directories. Returns (root, season 1, season 2, assets directory).
    fn shared_asset_tree(base: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = base.join("Anime");
        let season1 = root.join("Season 01");
        let season2 = root.join("Season 02");
        let assets = base.join("assets").join("Season 01");
        for dir in [&season1, &season2, &assets] {
            fs::create_dir_all(dir).unwrap();
        }
        let chapter_xml =
            b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>";
        let files: [(&Path, &[&str]); 4] = [
            (
                &root,
                &[
                    "Show - 01.ass",
                    "Show - 01.eng.ass",
                    "Show - S01E01.eng.ass",
                    "Show - S02E01.ind.ass",
                    "Show - 02.xml",
                    "Show - S01E02.xml",
                    "chapters.xml",
                    "cover.jpg",
                    "folder.png",
                    "Show - 01_cover.jpg",
                    "Show - 02_cover.jpg",
                ],
            ),
            (
                &season1,
                &[
                    "Show - S01E01.mkv",
                    "Show - S01E02.mkv",
                    "Show - 03.mkv",
                    "Show - S01E04.mkv",
                    "Show - 02.ind.ass",
                    "Show - 01.xml",
                    "Show - 03.png",
                    "Show - 02.cover.jpg",
                ],
            ),
            (
                &assets,
                &[
                    "Show - 03.eng.ass",
                    "Show - 04.xml",
                    "Show - S01E04.xml",
                    "Show - 04_cover.jpg",
                    "folder.jpg",
                ],
            ),
            (
                assets.parent().unwrap(),
                &[
                    "Show - 03.ind.ass",
                    "Show - 04.ass",
                    "folder.jpg",
                    "cover.png",
                ],
            ),
        ];
        for (dir, names) in files {
            for name in names {
                let path = dir.join(name);
                let body: &[u8] = if name.ends_with(".xml") {
                    chapter_xml
                } else {
                    b"x"
                };
                fs::write(path, body).unwrap();
            }
        }
        (root, season1, season2, assets)
    }

    /// Whether two indices hold the very same segments, in the same order.
    fn same_segments<T>(a: &[Arc<T>], b: &[Arc<T>]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| Arc::ptr_eq(x, y))
    }

    #[test]
    fn test_preparer_shares_each_directory_index_across_releases() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, season1, season2, assets) = shared_asset_tree(tmp.path());

        let mut preparer = ReleasePreparer::default();
        let first = preparer
            .prepare(&season1, &root, None, Some(&assets))
            .resolver;
        let second = preparer
            .prepare(&season2, &root, None, Some(&assets))
            .resolver;

        // The release tree is indexed deepest first, so the shared root is the last segment.
        let root_segments = |resolver: &AssetResolver| {
            (
                resolver.sub_index.segments.last().map(Arc::as_ptr),
                resolver.chap_index.segments.last().map(Arc::as_ptr),
                resolver.cover_index.segments.last().map(Arc::as_ptr),
            )
        };
        assert_eq!(root_segments(&first), root_segments(&second));
        // Both releases resolve the same assets directory, so they share its fallback index too.
        let (a, b) = (&first, &second);
        assert_eq!(a.fallback_sub_index.as_ref().unwrap().segments.len(), 2);
        assert!(same_segments(
            &a.fallback_sub_index.as_ref().unwrap().segments,
            &b.fallback_sub_index.as_ref().unwrap().segments
        ));
        assert!(same_segments(
            &a.fallback_chap_index.as_ref().unwrap().segments,
            &b.fallback_chap_index.as_ref().unwrap().segments
        ));
        assert!(same_segments(
            &a.fallback_cover_index.as_ref().unwrap().segments,
            &b.fallback_cover_index.as_ref().unwrap().segments
        ));
    }

    /// Every lookup of one episode, for comparing two resolvers.
    #[derive(Debug, PartialEq)]
    struct Lookups {
        resolved: ResolvedAssets,
        ancestor_sub: Option<(PathBuf, String)>,
        fallback_sub: Option<(PathBuf, String)>,
        chapter: Option<(PathBuf, Option<String>)>,
        fallback_chapter: Option<(PathBuf, Option<String>)>,
        fallback_cover: Option<PathBuf>,
    }

    #[test]
    fn test_shared_segments_resolve_like_one_index_built_by_add() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, season1, season2, assets) = shared_asset_tree(tmp.path());

        // Season 2 goes first, so season 1 reads the root through a shared segment.
        let mut preparer = ReleasePreparer::default();
        preparer.prepare(&season2, &root, None, Some(&assets));
        let segmented = preparer
            .prepare(&season1, &root, None, Some(&assets))
            .resolver;
        assert_eq!(segmented.sub_index.segments.len(), 2);

        // The same files in the same order, each index in one segment of its own.
        let flat_indices = |dirs: &[PathBuf]| {
            let (mut subs, mut chapters, mut covers) =
                (SubIndex::new(), ChapterIndex::new(), CoverIndex::new());
            for dir in dirs {
                let found = scan_assets(dir, None);
                found.subs.iter().for_each(|f| subs.add(f.clone()));
                found.chapters.iter().for_each(|f| chapters.add(f));
                found.covers.iter().for_each(|f| covers.add(f.clone()));
            }
            assert_eq!(subs.segments.len(), 1);
            (subs, chapters, covers)
        };
        let (sub_index, chap_index, cover_index) = flat_indices(&[season1.clone(), root.clone()]);
        let (fb_sub, fb_chap, fb_cov) =
            flat_indices(&[assets.clone(), assets.parent().unwrap().to_path_buf()]);
        let flat = AssetResolver {
            sub_index,
            chap_index,
            cover_index,
            fallback_sub_index: Some(fb_sub),
            fallback_chap_index: Some(fb_chap),
            fallback_cover_index: Some(fb_cov),
        };

        assert_eq!(segmented.has_subtitles(), flat.has_subtitles());
        assert_eq!(
            segmented.chap_index.unmatched().collect::<Vec<_>>(),
            flat.chap_index.unmatched().collect::<Vec<_>>()
        );
        let mut resolved = Vec::new();
        for name in [
            "Show - S01E01.mkv",
            "Show - S01E02.mkv",
            "Show - 03.mkv",
            "Show - S01E04.mkv",
        ] {
            let video = create_parsed_file(season1.join(name));
            let parsed = video.parsed.clone().unwrap();
            let lookups = |resolver: &AssetResolver| Lookups {
                resolved: resolver.resolve_for(&video, None, None, None),
                ancestor_sub: resolver.sub_index.lookup_ancestors(&parsed, &season1),
                fallback_sub: resolver
                    .fallback_sub_index
                    .as_ref()
                    .unwrap()
                    .lookup_fallback(&parsed),
                chapter: resolver.chap_index.lookup_with_warning(&parsed),
                fallback_chapter: resolver
                    .fallback_chap_index
                    .as_ref()
                    .unwrap()
                    .lookup_with_warning(&parsed),
                fallback_cover: resolver
                    .fallback_cover_index
                    .as_ref()
                    .unwrap()
                    .lookup_fallback(&video),
            };
            let result = lookups(&segmented);
            assert_eq!(result, lookups(&flat), "{name}");
            resolved.push(result);
        }
        let [ep1, ep2, ep3, ep4] = resolved.try_into().unwrap();
        // The comparison is not vacuous: the tiers and tie-breaks above all took effect.
        // Within the root, the exact season beats unknown ones of a better language and category.
        assert_eq!(
            ep1.resolved.subtitle_file,
            Some(root.join("Show - S01E01.eng.ass"))
        );
        assert_eq!(
            ep1.resolved.cover_file,
            Some(root.join("Show - 01_cover.jpg"))
        );
        assert_eq!(
            ep2.resolved.subtitle_file,
            Some(season1.join("Show - 02.ind.ass"))
        );
        // The nearer directory beats a better kind: season 1's dot cover over the root's.
        assert_eq!(
            ep2.resolved.cover_file,
            Some(season1.join("Show - 02.cover.jpg"))
        );
        // The exact season beats the unknown one beside it, which the warning names.
        let (chapter, warning) = ep2.chapter.unwrap();
        assert_eq!(chapter, root.join("Show - S01E02.xml"));
        assert!(warning.unwrap().contains("ignoring 'Show - 02.xml'"));
        assert_eq!(
            ep3.fallback_sub.unwrap().0,
            assets.join("Show - 03.eng.ass")
        );
        assert_eq!(ep3.resolved.cover_file, Some(season1.join("Show - 03.png")));
        let (chapter, warning) = ep4.fallback_chapter.unwrap();
        assert_eq!(chapter, assets.join("Show - S01E04.xml"));
        assert!(warning.unwrap().contains("ignoring 'Show - 04.xml'"));
        assert_eq!(ep4.fallback_cover, Some(assets.join("Show - 04_cover.jpg")));
        // The assets directory ranks ahead of its parent, so its folder beats the parent's cover.
        assert_eq!(ep1.fallback_cover, Some(assets.join("folder.jpg")));
    }
}
