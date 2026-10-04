//! Asset discovery, indexing, and resolution for anime releases
//! (subtitles, chapters, covers, fonts).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::domain::{
    ancestor_dirs, detect_sub_lang, is_dialogue_track, lang_label, lang_mkv_tag, natural_sort_key,
    parse_episode, score_subtitle_category, seasons_match, select_best_subtitle_track, EpisodeInfo,
    EpisodeType, NaturalSortToken, COVER_EXTENSIONS,
};
use crate::fonts::{find_fonts_dir, scan_fonts, FontInfo};
use crate::scan::{
    path_ext_lower, scan_assets, InspectedVideo, InspectionResult, ParsedFile, ScanCache,
    ScanResults,
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

#[derive(Default)]
pub struct ReleasePreparationOptions<'a> {
    pub fonts_override: Option<&'a Path>,
    pub assets_dir: Option<&'a Path>,
    pub scan_cache: Option<&'a mut ScanCache>,
    pub fonts_dir_cache: Option<&'a mut HashMap<PathBuf, PathBuf>>,
    pub font_cache: Option<&'a mut HashMap<PathBuf, FontInfo>>,
    pub sub_classification_cache: Option<&'a mut HashMap<PathBuf, SubClassification>>,
}

/// Resolves the fonts directory for `mkv_dir` and indexes the assets of every directory from
/// `root_input_dir` down to it.
///
/// Fonts come from `fonts_override`, else the nearest directory (from `mkv_dir` upwards) whose
/// fonts directory holds fonts, else the assets directory, else `mkv_dir/fonts`.
pub fn prepare_release_directory(
    mkv_dir: &Path,
    root_input_dir: &Path,
    mut options: ReleasePreparationOptions<'_>,
) -> PreparedRelease {
    let search_dirs = ancestor_dirs(mkv_dir, root_input_dir);
    let fonts_dir = match options.fonts_override {
        Some(over) => over.to_path_buf(),
        None => {
            let mut fonts_dir_with_fonts = |dir: &Path| -> Option<PathBuf> {
                let candidate = find_fonts_dir(dir, None, options.fonts_dir_cache.as_deref_mut());
                let scanned = scan_fonts(&candidate, options.font_cache.as_deref_mut());
                (scanned.count > 0).then_some(candidate)
            };
            search_dirs
                .iter()
                .rev()
                .find_map(|d| fonts_dir_with_fonts(d))
                .or_else(|| options.assets_dir.and_then(&mut fonts_dir_with_fonts))
                .unwrap_or_else(|| mkv_dir.join("fonts"))
        }
    };
    let resolver = AssetResolver::discover(
        &search_dirs,
        options.assets_dir,
        options.scan_cache,
        options.sub_classification_cache,
    );
    PreparedRelease {
        fonts_dir,
        resolver,
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
/// Every release directory re-indexes the same shared ancestor assets, so these
/// are derived once per file and memoized in a run-wide cache rather than
/// recomputed for each (asset, release directory) pair.
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

/// Whether `path` duplicates an already indexed chapter of the same season. The first file
/// indexed wins; the rejection is logged.
fn is_duplicate_chapter(
    existing: &[(PathBuf, String)],
    path: &Path,
    season: &str,
    key: &str,
) -> bool {
    let Some((existing_path, _)) = existing
        .iter()
        .find(|(_, existing_season)| existing_season == season)
    else {
        return false;
    };
    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let existing_name = existing_path
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let path_name = path
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();

    if existing_path.parent() == path.parent() {
        warn!("Duplicate chapter file: key {key} in '{parent_name}': keeping '{existing_name}', ignoring '{path_name}'");
    } else {
        let parent_display = path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        info!("Duplicate asset for key {key} also found in '{parent_name}': keeping '{existing_name}', ignoring '{path_name}' from '{parent_display}'");
    }
    true
}

#[derive(Debug, Clone, Default)]
pub struct SubIndex {
    index: HashMap<String, Vec<SubEntry>>,
    index_paths: HashMap<String, HashSet<PathBuf>>,
    directories: Vec<PathBuf>,
    has_unparsed: bool,
}

impl SubIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, sub_file: ParsedFile) {
        self.add_classified(&sub_file, None);
    }

    /// Adds a subtitle, reusing `classification` from the run-wide memo when the caller
    /// has one. Pass `None` to derive the classification on the spot.
    fn add_classified(&mut self, sub_file: &ParsedFile, classification: Option<SubClassification>) {
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
        let classification = classification.unwrap_or_else(|| classify_sub(sub_file));
        self.index.entry(key).or_default().push(SubEntry {
            file: sub_file.clone(),
            classification,
        });
    }

    fn lookup_ranked(
        &self,
        parsed: &EpisodeInfo,
        rank: impl Fn(&Path) -> Option<usize>,
    ) -> Option<(PathBuf, String)> {
        let entries = self.index.get(&ep_key(parsed))?;
        entries
            .iter()
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

    fn lookup_fallback(&self, parsed: &EpisodeInfo) -> Option<(PathBuf, String)> {
        self.lookup_ranked(parsed, |candidate| {
            self.directories.iter().position(|dir| dir == candidate)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty() && !self.has_unparsed
    }
}

// ─── CHAPTER INDEX ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ChapterIndex {
    index: HashMap<String, Vec<(PathBuf, String)>>,
    unmatched: Vec<PathBuf>,
}

impl ChapterIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Indexes `xml_file` as chapter XML without re-reading it: `scan_assets` is the only
    /// place a file is sniffed for chapter content.
    pub fn add(&mut self, xml_file: ParsedFile) {
        self.add_borrowed(&xml_file);
    }

    /// Like [`Self::add`], cloning only the path and season the index keeps.
    fn add_borrowed(&mut self, xml_file: &ParsedFile) {
        if let Some(parsed) = &xml_file.parsed {
            let key = ep_key(parsed);
            let existing = self.index.entry(key.clone()).or_default();
            if is_duplicate_chapter(existing, &xml_file.path, &parsed.season, &key) {
                return;
            }
            existing.push((xml_file.path.clone(), parsed.season.clone()));
        } else {
            self.unmatched.push(xml_file.path.clone());
        }
    }

    pub fn lookup(&self, parsed: &EpisodeInfo) -> Option<PathBuf> {
        self.index
            .get(&ep_key(parsed))?
            .iter()
            .find_map(|(path, season)| seasons_match(&parsed.season, season).then(|| path.clone()))
    }

    /// Chapter files whose names carry no episode number.
    pub fn unmatched(&self) -> &[PathBuf] {
        &self.unmatched
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

fn compare_ranked_covers(
    a_rank: usize,
    a: &CoverEntry,
    b_rank: usize,
    b: &CoverEntry,
) -> std::cmp::Ordering {
    a_rank
        .cmp(&b_rank)
        .then_with(|| a.kind.cmp(&b.kind))
        .then_with(|| a.natural_name.cmp(&b.natural_name))
        .then_with(|| a.file.path.cmp(&b.file.path))
}

#[derive(Debug, Clone, Default)]
pub struct CoverIndex {
    entries: Vec<CoverEntry>,
    paths: HashSet<PathBuf>,
    directory_ranks: HashMap<PathBuf, usize>,
    episode_entries: HashMap<EpisodeType, HashMap<String, HashMap<PathBuf, Vec<usize>>>>,
    adjacent_entries: HashMap<PathBuf, HashMap<String, Vec<usize>>>,
    directory_entries: HashMap<PathBuf, Vec<usize>>,
    // (first-seen fallback directory rank, entry index)
    directory_winner: Option<(usize, usize)>,
}

impl CoverIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, image_file: ParsedFile) {
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
        let mut directory_rank = None;
        if let Some(dir) = image_file.path.parent() {
            let dir = dir.to_path_buf();
            let next_rank = self.directory_ranks.len();
            directory_rank = Some(*self.directory_ranks.entry(dir.clone()).or_insert(next_rank));
            match kind {
                CoverKind::Underscore | CoverKind::Dot => {
                    if let Some(episode) = &episode {
                        self.episode_entries
                            .entry(episode.ep_type)
                            .or_default()
                            .entry(episode.ep.clone())
                            .or_default()
                            .entry(dir.clone())
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
        if matches!(kind, CoverKind::Cover | CoverKind::Folder) {
            if let Some(rank) = directory_rank {
                let candidate = &self.entries[entry_index];
                let is_best = self.directory_winner.is_none_or(|(best_rank, best_index)| {
                    compare_ranked_covers(rank, candidate, best_rank, &self.entries[best_index])
                        == std::cmp::Ordering::Less
                });
                if is_best {
                    self.directory_winner = Some((rank, entry_index));
                }
            }
        }
    }

    fn lookup_ranked(
        &self,
        video: &ParsedFile,
        candidates: impl Iterator<Item = (usize, usize)>,
    ) -> Option<PathBuf> {
        candidates
            .filter_map(|(index, distance)| {
                let entry = &self.entries[index];
                if matches!(entry.kind, CoverKind::Underscore | CoverKind::Dot) {
                    let applies = video.parsed.as_ref().is_some_and(|target| {
                        entry.episode.as_ref().is_some_and(|candidate| {
                            seasons_match(&target.season, &candidate.season)
                        })
                    });
                    if !applies {
                        return None;
                    }
                }
                Some((distance, entry))
            })
            .min_by(|(a_rank, a), (b_rank, b)| compare_ranked_covers(*a_rank, a, *b_rank, b))
            .map(|(_, entry)| entry.file.path.clone())
    }

    pub fn lookup(&self, video: &ParsedFile) -> Option<PathBuf> {
        let parent = video.path.parent()?;
        let parsed = video.parsed.as_ref();
        let directory_candidates = parent
            .ancestors()
            .enumerate()
            .flat_map(|(distance, dir)| {
                let episodes = parsed.and_then(|parsed| {
                    self.episode_entries
                        .get(&parsed.ep_type)?
                        .get(&parsed.ep)?
                        .get(dir)
                });
                let covers = self.directory_entries.get(dir);
                episodes
                    .into_iter()
                    .flat_map(move |indices| {
                        indices.iter().copied().map(move |index| (index, distance))
                    })
                    .chain(covers.into_iter().flat_map(move |indices| {
                        indices.iter().copied().map(move |index| (index, distance))
                    }))
            })
            .chain(
                self.adjacent_entries
                    .get(parent)
                    .and_then(|entries| entries.get(video.stem.as_str()))
                    .into_iter()
                    .flat_map(|indices| indices.iter().copied().map(|index| (index, 0))),
            );
        self.lookup_ranked(video, directory_candidates)
    }

    fn lookup_fallback(&self, video: &ParsedFile) -> Option<PathBuf> {
        let episode_candidates = video
            .parsed
            .as_ref()
            .and_then(|parsed| {
                self.episode_entries
                    .get(&parsed.ep_type)
                    .and_then(|entries| entries.get(&parsed.ep))
            })
            .into_iter()
            .flat_map(|entries| entries.values())
            .flat_map(|indices| indices.iter().copied());
        let adjacent_candidates = video
            .path
            .parent()
            .and_then(|dir| self.adjacent_entries.get(dir))
            .and_then(|entries| entries.get(video.stem.as_str()))
            .into_iter()
            .flat_map(|indices| indices.iter().copied());
        let candidates = episode_candidates
            .chain(adjacent_candidates)
            .filter_map(|index| {
                let entry = &self.entries[index];
                self.directory_ranks
                    .get(entry.file.path.parent()?)
                    .copied()
                    .map(|rank| (index, rank))
            })
            .chain(self.directory_winner);
        self.lookup_ranked(video, candidates)
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

fn populate_indices(
    assets: &ScanResults,
    sub_index: &mut SubIndex,
    chap_index: &mut ChapterIndex,
    cover_index: &mut CoverIndex,
    mut sub_classification_cache: Option<&mut HashMap<PathBuf, SubClassification>>,
) {
    for sub in &assets.subs {
        let classification = if sub.parsed.is_some() {
            sub_classification_cache.as_deref_mut().map(|cache| {
                if let Some(hit) = cache.get(&sub.path) {
                    hit.clone()
                } else {
                    let fresh = classify_sub(sub);
                    cache.insert(sub.path.clone(), fresh.clone());
                    fresh
                }
            })
        } else {
            None
        };
        sub_index.add_classified(sub, classification);
    }
    for chap in &assets.chapters {
        chap_index.add_borrowed(chap);
    }
    for cov in &assets.covers {
        cover_index.add(cov.clone());
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
    /// font resolution is owned by `prepare_release_directory`.
    pub fn discover(
        search_dirs: &[PathBuf],
        assets_dir: Option<&Path>,
        mut scan_cache: Option<&mut ScanCache>,
        mut sub_classification_cache: Option<&mut HashMap<PathBuf, SubClassification>>,
    ) -> Self {
        let mut sub_index = SubIndex::new();
        let mut chap_index = ChapterIndex::new();
        let mut cover_index = CoverIndex::new();

        for d in search_dirs.iter().rev() {
            populate_indices(
                &scan_assets(d, scan_cache.as_deref_mut()),
                &mut sub_index,
                &mut chap_index,
                &mut cover_index,
                sub_classification_cache.as_deref_mut(),
            );
        }

        let mut fallback_sub_index = None;
        let mut fallback_chap_index = None;
        let mut fallback_cover_index = None;

        if let Some(assets) = assets_dir {
            let mut fb_sub = SubIndex::new();
            let mut fb_chap = ChapterIndex::new();
            let mut fb_cov = CoverIndex::new();

            let mut fallback_dirs = vec![assets.to_path_buf()];
            if let Some(parent) = assets.parent() {
                if parent != assets && parent.is_dir() {
                    fallback_dirs.push(parent.to_path_buf());
                }
            }

            for fb_dir in fallback_dirs {
                populate_indices(
                    &scan_assets(&fb_dir, scan_cache.as_deref_mut()),
                    &mut fb_sub,
                    &mut fb_chap,
                    &mut fb_cov,
                    sub_classification_cache.as_deref_mut(),
                );
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
        // Explicit human assignment overrides automatic matching (ADR 0004).
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

        // Embedded track from source video (ADR 0004).
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
    ) -> Option<PathBuf> {
        if let Some(chap) = self.chap_index.lookup(parsed) {
            return Some(chap);
        }
        if !has_source_chapters {
            if let Some(claimed) = claimed_chapter {
                return Some(claimed.to_path_buf());
            }
            if let Some(fb_chap) = &self.fallback_chap_index {
                return fb_chap.lookup(parsed);
            }
        }
        None
    }

    pub fn resolve_for(
        &self,
        video: &ParsedFile,
        inspected: Option<&InspectedVideo>,
        claimed_chapter: Option<&Path>,
        assigned_subtitle: Option<&Path>,
    ) -> Result<ResolvedAssets, String> {
        if let Some(assigned) = assigned_subtitle {
            if !assigned.is_file() {
                return Err(format!(
                    "Assigned subtitle '{}' no longer exists",
                    assigned.display()
                ));
            }
        }

        let has_source_chapters = inspected.is_some_and(|i| i.has_chapters());
        let has_source_cover = inspected.is_some_and(|i| i.cover_attachment().is_some());

        let (sub_res, chapter_file) = if let Some(parsed) = &video.parsed {
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
        let cover_file = self.resolve_cov(video, has_source_cover);

        let subtitle_label = lang_label(&sub_res.lang).to_string();
        let subtitle_mkv_lang = lang_mkv_tag(&sub_res.lang).to_string();

        Ok(ResolvedAssets {
            subtitle_file: sub_res.subtitle_file,
            embedded_sub_id: sub_res.embedded_sub_id,
            subtitle_lang: sub_res.lang,
            subtitle_label,
            subtitle_mkv_lang,
            matched_subtitle: sub_res.matched,
            chapter_file,
            cover_file,
            skip_reason: sub_res.skip_reason,
        })
    }
}

/// Assigns the release's only episode-less chapter file to its only episode that has neither a
/// matching chapter file nor embedded chapters. Any ambiguity leaves it unclaimed.
pub fn claim_leftover_chapters(
    videos: &[ParsedFile],
    inspection_results: &HashMap<PathBuf, InspectionResult>,
    resolver: &AssetResolver,
) -> HashMap<PathBuf, PathBuf> {
    let [sole_chapter] = resolver.chap_index.unmatched() else {
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
        (Some(video), None) => HashMap::from([(video.path.clone(), sole_chapter.clone())]),
        _ => HashMap::new(),
    }
}
