//! Asset discovery, indexing, and resolution for anime releases
//! (subtitles, chapters, covers, fonts).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use tracing::{info, warn};

use crate::domain::{
    ancestor_dirs, detect_sub_lang, is_dialogue_track, lang_label, lang_mkv_tag, natural_sort_key,
    parse_episode, score_subtitle_category, seasons_match, select_best_subtitle_track, EpisodeInfo,
    SUBTITLE_LANGS_FALLBACK_ORDER, SUBTITLE_LANGS_PRIMARY_ORDER,
};
use crate::fonts::{find_fonts_dir, scan_fonts, FontInfo};
use crate::scan::{
    is_chapter_xml, is_cover_filename, scan_assets, InspectedVideo, InspectionResult, ParsedFile,
    ScanResults,
};

fn ep_key(info: &EpisodeInfo) -> String {
    format!("{}|{}", info.ep_type, info.ep)
}

// ─── RELEASE PREPARATION ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontsOrigin {
    Override,
    Local,
    RootFallback,
    ParentFallback,
    Default,
}

#[derive(Debug, Clone)]
pub struct PreparedRelease {
    pub mkv_dir: PathBuf,
    pub videos: Vec<ParsedFile>,
    pub fonts_dir: PathBuf,
    pub fonts_origin: FontsOrigin,
    pub resolver: AssetResolver,
}

#[derive(Default)]
pub struct ReleasePreparationOptions<'a> {
    pub fonts_override: Option<&'a Path>,
    pub assets_dir: Option<&'a Path>,
    pub scan_cache: Option<&'a mut HashMap<PathBuf, ScanResults>>,
    pub fonts_dir_cache: Option<&'a mut HashMap<PathBuf, PathBuf>>,
    pub font_cache: Option<&'a mut HashMap<PathBuf, FontInfo>>,
}

pub fn prepare_release_directory(
    mkv_dir: &Path,
    root_input_dir: &Path,
    videos: &[ParsedFile],
    mut options: ReleasePreparationOptions<'_>,
) -> PreparedRelease {
    let search_dirs = ancestor_dirs(mkv_dir, root_input_dir);
    let (fonts_dir, fonts_origin) = if let Some(over) = options.fonts_override {
        (over.to_path_buf(), FontsOrigin::Override)
    } else {
        let mut resolved = None;
        for d in search_dirs.iter().rev() {
            let candidate = find_fonts_dir(d, None, options.fonts_dir_cache.as_deref_mut());
            let scanned = scan_fonts(&candidate, options.font_cache.as_deref_mut());
            if scanned.count > 0 {
                let origin = if d == mkv_dir {
                    FontsOrigin::Local
                } else {
                    FontsOrigin::ParentFallback
                };
                resolved = Some((candidate, origin));
                break;
            }
        }
        if resolved.is_none() {
            if let Some(assets) = options.assets_dir {
                let candidate =
                    find_fonts_dir(assets, None, options.fonts_dir_cache.as_deref_mut());
                let scanned = scan_fonts(&candidate, options.font_cache.as_deref_mut());
                if scanned.count > 0 {
                    resolved = Some((candidate, FontsOrigin::RootFallback));
                }
            }
        }
        let exact_local = mkv_dir.join("fonts");
        resolved.unwrap_or((exact_local, FontsOrigin::Default))
    };
    let resolver = AssetResolver::discover(
        &search_dirs,
        &fonts_dir,
        options.assets_dir,
        options.scan_cache,
        options.font_cache,
    );
    PreparedRelease {
        mkv_dir: mkv_dir.to_path_buf(),
        videos: videos.to_vec(),
        fonts_dir,
        fonts_origin,
        resolver,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InteractionMode {
    #[default]
    Interactive,
    DryRun,
    NonInteractive,
}

// ─── ASSET INDICES (Subtitles, Chapters, Covers) ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubEntry {
    file: ParsedFile,
    lang: String,
    category_score: i32,
    is_dialogue: bool,
}

fn make_sub_entry(sub_file: ParsedFile) -> SubEntry {
    let lang = detect_sub_lang(&sub_file.stem);
    let is_ass = sub_file
        .path
        .extension()
        .map(|e| e.to_string_lossy().eq_ignore_ascii_case("ass"))
        .unwrap_or(false);
    let category_score = score_subtitle_category(&lang, is_ass);
    let is_dialogue = is_dialogue_track(Some(&sub_file.stem));
    SubEntry {
        file: sub_file,
        lang,
        category_score,
        is_dialogue,
    }
}

fn reject_duplicate(
    existing: &[(PathBuf, String)],
    path: &Path,
    season: &str,
    key: &str,
    warning: &str,
) -> bool {
    for (existing_path, existing_season) in existing {
        if existing_season != season {
            continue;
        }
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
            warn!("{warning}: key {key} in '{parent_name}': keeping '{existing_name}', ignoring '{path_name}'");
        } else {
            let parent_display = path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            info!("Duplicate asset for key {key} also found in '{parent_name}': keeping '{existing_name}', ignoring '{path_name}' from '{parent_display}'");
        }
        return true;
    }
    false
}

#[derive(Debug, Clone, Default)]
pub struct SubIndex {
    index: HashMap<String, Vec<SubEntry>>,
    index_paths: HashMap<String, HashSet<PathBuf>>,
    unparsed: Vec<ParsedFile>,
    unparsed_paths: HashSet<PathBuf>,
}

impl SubIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, sub_file: ParsedFile) {
        let Some(parsed) = &sub_file.parsed else {
            if !self.unparsed_paths.contains(&sub_file.path) {
                self.unparsed_paths.insert(sub_file.path.clone());
                self.unparsed.push(sub_file);
            }
            return;
        };

        let key = ep_key(parsed);
        let paths = self.index_paths.entry(key.clone()).or_default();
        if paths.contains(&sub_file.path) {
            return;
        }
        paths.insert(sub_file.path.clone());
        self.index
            .entry(key)
            .or_default()
            .push(make_sub_entry(sub_file));
    }

    pub fn lookup(
        &self,
        parsed: &EpisodeInfo,
        local_dir: Option<&Path>,
    ) -> Option<(PathBuf, String)> {
        let key = ep_key(parsed);
        let entries = self.index.get(&key)?;

        let mut candidates = Vec::new();
        for (idx, entry) in entries.iter().enumerate() {
            if let Some(file_parsed) = &entry.file.parsed {
                if seasons_match(&parsed.season, &file_parsed.season) {
                    let is_local =
                        if local_dir.is_some_and(|dir| entry.file.path.parent() == Some(dir)) {
                            1
                        } else {
                            0
                        };
                    let priority = (
                        is_local,
                        entry.category_score,
                        if entry.is_dialogue { 1 } else { 0 },
                        -(entry.file.name.len() as i64),
                        -(idx as i64),
                    );
                    candidates.push((priority, entry));
                }
            }
        }

        if candidates.is_empty() {
            return None;
        }

        candidates.sort_by_key(|c| c.0);
        let (_, best) = candidates.pop().unwrap();
        Some((best.file.path.clone(), best.lang.clone()))
    }

    pub fn indexed_count(&self) -> usize {
        self.index.values().map(|v| v.len()).sum()
    }

    pub fn primary_count(&self) -> usize {
        self.index
            .values()
            .flat_map(|v| v.iter())
            .filter(|e| SUBTITLE_LANGS_PRIMARY_ORDER.contains(&e.lang.as_str()))
            .count()
    }

    pub fn fallback_count(&self) -> usize {
        self.index
            .values()
            .flat_map(|v| v.iter())
            .filter(|e| SUBTITLE_LANGS_FALLBACK_ORDER.contains(&e.lang.as_str()))
            .count()
    }

    pub fn all_subtitles(&self) -> Vec<PathBuf> {
        let mut discovered: Vec<PathBuf> = self.unparsed.iter().map(|f| f.path.clone()).collect();
        for list in self.index.values() {
            for entry in list {
                discovered.push(entry.file.path.clone());
            }
        }

        let mut unique = Vec::new();
        let mut seen = HashSet::new();
        for p in discovered {
            if seen.insert(p.clone()) {
                unique.push(p);
            }
        }

        unique.sort_by_key(|a| natural_sort_key(&a.to_string_lossy()));
        unique
    }
}

// ─── CHAPTER INDEX ───────────────────────────────────────────────────────────

fn find_matching_season_path(
    entries: &[(PathBuf, String)],
    target_season: &str,
) -> Option<PathBuf> {
    entries
        .iter()
        .find_map(|(path, season)| seasons_match(target_season, season).then(|| path.clone()))
}
#[derive(Debug, Clone, Default)]
pub struct ChapterIndex {
    index: HashMap<String, Vec<(PathBuf, String)>>,
    unmatched: Vec<PathBuf>,
    rejected: usize,
}

impl ChapterIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, xml_file: ParsedFile) {
        if !is_chapter_xml(&xml_file.path) {
            self.rejected += 1;
            return;
        }

        if let Some(parsed) = &xml_file.parsed {
            let key = ep_key(parsed);
            let existing = self.index.entry(key.clone()).or_default();
            if reject_duplicate(
                existing,
                &xml_file.path,
                &parsed.season,
                &key,
                "Duplicate chapter file",
            ) {
                return;
            }
            existing.push((xml_file.path, parsed.season.clone()));
        } else {
            self.unmatched.push(xml_file.path);
        }
    }

    pub fn lookup(&self, parsed: &EpisodeInfo) -> Option<PathBuf> {
        let key = ep_key(parsed);
        find_matching_season_path(self.index.get(&key)?, &parsed.season)
    }

    pub fn has_match(&self, parsed: &EpisodeInfo) -> bool {
        self.lookup(parsed).is_some()
    }

    pub fn indexed_count(&self) -> usize {
        self.index.values().map(|v| v.len()).sum()
    }

    pub fn rejected_count(&self) -> usize {
        self.rejected
    }

    pub fn unmatched(&self) -> &[PathBuf] {
        &self.unmatched
    }
}

// ─── COVER INDEX ─────────────────────────────────────────────────────────────

static COVER_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?P<stem>.+)_cover$").unwrap());

#[derive(Debug, Clone, Default)]
pub struct CoverIndex {
    index: HashMap<String, Vec<(PathBuf, String)>>,
    pub global: Option<PathBuf>,
}

impl CoverIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, image_file: ParsedFile) {
        let mut is_global = false;
        let mut parsed = None;

        if is_cover_filename(&image_file.name) {
            is_global = true;
        } else if let Some(caps) = COVER_FILE_RE.captures(&image_file.stem) {
            let base_stem = caps.name("stem").unwrap().as_str();
            parsed = parse_episode(base_stem);
            if parsed.is_none() {
                is_global = true;
            }
        } else {
            return;
        }

        if is_global {
            if self.global.is_none() {
                self.global = Some(image_file.path);
            } else if self.global.as_ref().and_then(|p| p.parent()) == image_file.path.parent() {
                let parent_name = image_file
                    .path
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default();
                let global_name = self
                    .global
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default();
                warn!(
                    "Duplicate global cover in '{parent_name}': keeping '{global_name}', ignoring '{}'",
                    image_file.name
                );
            }
            return;
        }

        let Some(p) = parsed else {
            return;
        };

        let key = ep_key(&p);
        let existing = self.index.entry(key.clone()).or_default();
        if reject_duplicate(
            existing,
            &image_file.path,
            &p.season,
            &key,
            "Duplicate cover image",
        ) {
            return;
        }
        existing.push((image_file.path, p.season));
    }

    pub fn lookup(&self, parsed: &EpisodeInfo) -> Option<PathBuf> {
        let key = ep_key(parsed);
        let matched = self
            .index
            .get(&key)
            .and_then(|entries| find_matching_season_path(entries, &parsed.season));
        matched.or_else(|| self.global.clone())
    }
    pub fn indexed_count(&self) -> usize {
        self.index.values().map(|v| v.len()).sum()
    }

    pub fn has_global(&self) -> bool {
        self.global.is_some()
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
    pub fonts: FontInfo,
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
        let sub_lang = detect_sub_lang(&stem);
        Self {
            subtitle_file: Some(assigned.to_path_buf()),
            embedded_sub_id: None,
            lang: sub_lang,
            matched: true,
            skip_reason: None,
        }
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
    assets: ScanResults,
    sub_index: &mut SubIndex,
    chap_index: &mut ChapterIndex,
    cover_index: &mut CoverIndex,
) {
    for sub in assets.subs {
        sub_index.add(sub);
    }
    for chap in assets.chapters {
        chap_index.add(chap);
    }
    for cov in assets.covers {
        cover_index.add(cov);
    }
}

#[derive(Debug, Clone)]
pub struct AssetResolver {
    pub fonts: FontInfo,
    pub sub_index: SubIndex,
    pub chap_index: ChapterIndex,
    pub cover_index: CoverIndex,
    pub fallback_sub_index: Option<SubIndex>,
    pub fallback_chap_index: Option<ChapterIndex>,
    pub fallback_cover_index: Option<CoverIndex>,
}

impl AssetResolver {
    pub fn new(
        fonts: FontInfo,
        sub_index: SubIndex,
        chap_index: ChapterIndex,
        cover_index: CoverIndex,
        fallback_sub_index: Option<SubIndex>,
        fallback_chap_index: Option<ChapterIndex>,
        fallback_cover_index: Option<CoverIndex>,
    ) -> Self {
        Self {
            fonts,
            sub_index,
            chap_index,
            cover_index,
            fallback_sub_index,
            fallback_chap_index,
            fallback_cover_index,
        }
    }

    pub fn all_subtitles(&self) -> Vec<PathBuf> {
        self.sub_index.all_subtitles()
    }

    pub fn use_source_fonts(&self) -> bool {
        self.fonts.count == 0
    }

    pub fn has_local_subtitle(&self, parsed: &EpisodeInfo, local_dir: Option<&Path>) -> bool {
        self.sub_index.lookup(parsed, local_dir).is_some()
    }

    pub fn count_unmatched_chapter_videos(&self, videos: &[ParsedFile]) -> usize {
        videos
            .iter()
            .filter(|v| {
                if let Some(parsed) = &v.parsed {
                    !self.chap_index.has_match(parsed)
                } else {
                    false
                }
            })
            .count()
    }

    pub fn discover(
        search_dirs: &[PathBuf],
        fonts_dir: &Path,
        assets_dir: Option<&Path>,
        mut scan_cache: Option<&mut HashMap<PathBuf, ScanResults>>,
        font_cache: Option<&mut HashMap<PathBuf, FontInfo>>,
    ) -> Self {
        let mut fonts = scan_fonts(fonts_dir, font_cache);
        let mut sub_index = SubIndex::new();
        let mut chap_index = ChapterIndex::new();
        let mut cover_index = CoverIndex::new();

        for d in search_dirs.iter().rev() {
            populate_indices(
                scan_assets(d, scan_cache.as_deref_mut()),
                &mut sub_index,
                &mut chap_index,
                &mut cover_index,
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
                if fonts.count == 0 {
                    let fb_fonts_dir = find_fonts_dir(&fb_dir, None, None);
                    let scanned_fb_fonts = scan_fonts(&fb_fonts_dir, None);
                    if scanned_fb_fonts.count > 0 {
                        fonts = scanned_fb_fonts;
                    }
                }
                populate_indices(
                    scan_assets(&fb_dir, scan_cache.as_deref_mut()),
                    &mut fb_sub,
                    &mut fb_chap,
                    &mut fb_cov,
                );
            }

            fallback_sub_index = Some(fb_sub);
            fallback_chap_index = Some(fb_chap);
            fallback_cover_index = Some(fb_cov);
        }

        Self {
            fonts,
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
        all_subs: &[PathBuf],
        assigned_subtitle: Option<&Path>,
    ) -> SubResolution {
        // 1. Explicit human assignment overrides everything (ADR 0004)
        if let Some(assigned) = assigned_subtitle {
            return SubResolution::from_assigned(assigned);
        }

        // 2. Local subtitle from release directory / ancestor chain (ADR 0004)
        if let Some((sub_path, lang)) = self.sub_index.lookup(parsed, video.path.parent()) {
            return SubResolution::external(sub_path, lang);
        }

        // 3. Embedded track from source video (ADR 0004)
        if let Some(insp) = inspected {
            if let Some((track_id, lang, _)) =
                select_best_subtitle_track(&insp.subtitle_tracks(), None)
            {
                return SubResolution::embedded(track_id, lang);
            }
        }

        // 4. Fallback assets directory (ADR 0004)
        if let Some(ref fb_sub) = self.fallback_sub_index {
            if let Some((sub_path, lang)) = fb_sub.lookup(parsed, video.path.parent()) {
                return SubResolution::external(sub_path, lang);
            }
        }

        // 5. No subtitle match found
        let skip_reason = if all_subs.is_empty() {
            "no subtitle files found"
        } else {
            "subtitle files found but none matched"
        };
        SubResolution::unmatched(skip_reason)
    }

    pub fn resolve_cov(&self, parsed: &EpisodeInfo, has_source_cover: bool) -> Option<PathBuf> {
        if let Some(cov) = self.cover_index.lookup(parsed) {
            return Some(cov);
        }
        if !has_source_cover {
            return self
                .fallback_cover_index
                .as_ref()
                .and_then(|fb| fb.lookup(parsed));
        }
        None
    }

    pub fn resolve_chap(
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
            if let Some(ref fb_chap) = self.fallback_chap_index {
                return fb_chap.lookup(parsed);
            }
        }
        None
    }

    pub fn resolve_for(
        &self,
        video: &ParsedFile,
        inspected: Option<&InspectedVideo>,
        all_subs: Option<&[PathBuf]>,
        claimed_chapter: Option<&Path>,
        assigned_subtitle: Option<&Path>,
    ) -> ResolvedAssets {
        let owned_all_subs;
        let subs_slice = match all_subs {
            Some(s) => s,
            None => {
                owned_all_subs = self.all_subtitles();
                &owned_all_subs
            }
        };

        let has_source_chapters = inspected.map(|i| i.has_chapters()).unwrap_or(false);
        let has_source_cover = inspected
            .map(|i| i.cover_attachment().is_some())
            .unwrap_or(false);

        let (sub_res, chapter_file, cover_file) = if let Some(parsed) = &video.parsed {
            let sub = self.resolve_sub(video, parsed, inspected, subs_slice, assigned_subtitle);
            let chap = self.resolve_chap(parsed, has_source_chapters, claimed_chapter);
            let cov = self.resolve_cov(parsed, has_source_cover);
            (sub, chap, cov)
        } else {
            let sub = match assigned_subtitle {
                Some(assigned) => SubResolution::from_assigned(assigned),
                None => SubResolution::unmatched("unparsed episode"),
            };
            let cov = if !has_source_cover {
                self.cover_index.global.clone()
            } else {
                None
            };
            (sub, None, cov)
        };

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
            cover_file,
            fonts: self.fonts.clone(),
            skip_reason: sub_res.skip_reason,
        }
    }
}

pub fn claim_leftover_chapters(
    videos: &[ParsedFile],
    inspection_results: &HashMap<PathBuf, InspectionResult>,
    resolver: &AssetResolver,
) -> HashMap<PathBuf, PathBuf> {
    if resolver.chap_index.unmatched().len() != 1 {
        return HashMap::new();
    }

    let sole_chapter = resolver.chap_index.unmatched()[0].clone();

    let mut candidate_videos: Vec<&ParsedFile> = Vec::new();
    for video in videos {
        let Some(ref parsed) = video.parsed else {
            continue;
        };
        if resolver.chap_index.has_match(parsed) {
            continue;
        }
        let has_embedded_chapters = inspection_results
            .get(&video.path)
            .and_then(|r| r.video.as_ref())
            .map(|v| v.has_chapters())
            .unwrap_or(false);
        if !has_embedded_chapters {
            candidate_videos.push(video);
        }
    }

    if candidate_videos.len() != 1 {
        return HashMap::new();
    }

    let mut claims = HashMap::new();
    claims.insert(candidate_videos[0].path.clone(), sole_chapter);
    claims
}
