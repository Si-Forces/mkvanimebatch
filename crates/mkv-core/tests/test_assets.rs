use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use mkv_core::assets::{
    claim_leftover_chapters, prepare_release_directory, AssetResolver, ChapterIndex, CoverIndex,
    FontsOrigin, ReleasePreparationOptions, SubIndex,
};
use mkv_core::domain::{ancestor_dirs, EpisodeInfo};
use mkv_core::fonts::FontInfo;
use mkv_core::scan::{
    create_parsed_file, InspectedVideo, InspectionResult, MkvChapter, MkvmergeMetadata, ParsedFile,
};
use tempfile::tempdir;

fn make_video(path: &Path) -> ParsedFile {
    create_parsed_file(path)
}

#[test]
fn test_chap_index_lookup() {
    let tmp = tempdir().unwrap();
    let xml_path = tmp.path().join("Show_S01E01_chapters.xml");
    fs::write(
        &xml_path,
        b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
    )
    .unwrap();

    let mut chap_idx = ChapterIndex::new();
    chap_idx.add(make_video(&xml_path));

    let matched = chap_idx.lookup(&EpisodeInfo::new("1", "E", "01"));
    assert_eq!(matched, Some(xml_path));
}

#[test]
fn test_chap_index_cross_dir_duplicate_kept_first() {
    let tmp = tempdir().unwrap();
    let specific_dir = tmp.path().join("Season 01");
    let shared_dir = tmp.path().join("root");
    fs::create_dir_all(&specific_dir).unwrap();
    fs::create_dir_all(&shared_dir).unwrap();

    let first = specific_dir.join("Show_S01E01_chapters.xml");
    let second = shared_dir.join("Show_S01E01_chapters.xml");
    for p in [&first, &second] {
        fs::write(
            p,
            b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
        )
        .unwrap();
    }

    let mut chap_idx = ChapterIndex::new();
    chap_idx.add(make_video(&first));
    chap_idx.add(make_video(&second));

    assert_eq!(
        chap_idx.lookup(&EpisodeInfo::new("1", "E", "01")),
        Some(first)
    );
}

#[test]
fn test_cover_index_global_fallback() {
    let tmp = tempdir().unwrap();
    let cover_path = tmp.path().join("cover.jpg");
    fs::write(&cover_path, b"img").unwrap();

    let mut cover_idx = CoverIndex::new();
    cover_idx.add(make_video(&cover_path));

    let matched = cover_idx.lookup(&EpisodeInfo::new("", "E", "05"));
    assert_eq!(matched, Some(cover_path));
}

#[test]
fn test_cover_index_single_digit_cover_filename() {
    let tmp = tempdir().unwrap();
    let cover_path = tmp.path().join("Show 1_cover.jpg");
    fs::write(&cover_path, b"img").unwrap();

    let mut cover_idx = CoverIndex::new();
    cover_idx.add(make_video(&cover_path));

    let matched = cover_idx.lookup(&EpisodeInfo::new("", "E", "01"));
    assert_eq!(matched, Some(cover_path));
    assert!(cover_idx.global.is_none());
}

#[test]
fn test_resolve_cover_local_overrides_all() {
    let tmp = tempdir().unwrap();
    let local_cover = tmp.path().join("Show 01_cover.jpg");
    fs::write(&local_cover, b"local").unwrap();
    let fallback_cover = tmp.path().join("fallback_cover.jpg");
    fs::write(&fallback_cover, b"fb").unwrap();

    let mut cover_idx = CoverIndex::new();
    cover_idx.add(make_video(&local_cover));

    let mut fallback_idx = CoverIndex::new();
    fallback_idx.add(make_video(&fallback_cover));

    let parsed = EpisodeInfo::new("1", "E", "01");
    let resolver = AssetResolver::new(
        FontInfo::default(),
        SubIndex::new(),
        ChapterIndex::new(),
        cover_idx,
        None,
        None,
        Some(fallback_idx),
    );

    let result = resolver.resolve_cov(&parsed, true);
    assert_eq!(result, Some(local_cover));
}

#[test]
fn test_resolve_cover_source_cover_gates_fallback() {
    let tmp = tempdir().unwrap();
    let fallback_cover = tmp.path().join("cover.jpg");
    fs::write(&fallback_cover, b"fb").unwrap();

    let cover_idx = CoverIndex::new();
    let mut fallback_idx = CoverIndex::new();
    fallback_idx.add(make_video(&fallback_cover));

    let parsed = EpisodeInfo::new("1", "E", "01");
    let resolver = AssetResolver::new(
        FontInfo::default(),
        SubIndex::new(),
        ChapterIndex::new(),
        cover_idx,
        None,
        None,
        Some(fallback_idx),
    );

    let result = resolver.resolve_cov(&parsed, true);
    assert_eq!(result, None);
}

#[test]
fn test_resolve_cover_fallback_used_when_no_source_cover() {
    let tmp = tempdir().unwrap();
    let fallback_cover = tmp.path().join("cover.jpg");
    fs::write(&fallback_cover, b"fb").unwrap();

    let cover_idx = CoverIndex::new();
    let mut fallback_idx = CoverIndex::new();
    fallback_idx.add(make_video(&fallback_cover));

    let parsed = EpisodeInfo::new("1", "E", "01");
    let resolver = AssetResolver::new(
        FontInfo::default(),
        SubIndex::new(),
        ChapterIndex::new(),
        cover_idx,
        None,
        None,
        Some(fallback_idx),
    );

    let result = resolver.resolve_cov(&parsed, false);
    assert_eq!(result, Some(fallback_cover));
}

#[test]
fn test_sub_index_other_language_matching() {
    let mut sub_index = SubIndex::new();
    let sub_file = ParsedFile {
        path: PathBuf::from("episode_01(ts).ass"),
        name: "episode_01(ts).ass".to_string(),
        stem: "episode_01(ts)".to_string(),
        parsed: Some(EpisodeInfo::new("", "E", "01")),
    };

    sub_index.add(sub_file);

    let result = sub_index.lookup(&EpisodeInfo::new("", "E", "01"), None);
    assert!(result.is_some());
    let (path, lang) = result.unwrap();
    assert_eq!(path, PathBuf::from("episode_01(ts).ass"));
    assert_eq!(lang, "ts");
}

#[test]
fn test_sub_index_different_languages_not_duplicates() {
    let mut sub_index = SubIndex::new();
    let sub_eng = ParsedFile {
        path: PathBuf::from("episode_01.eng.ass"),
        name: "episode_01.eng.ass".to_string(),
        stem: "episode_01.eng".to_string(),
        parsed: Some(EpisodeInfo::new("", "E", "01")),
    };
    let sub_ind = ParsedFile {
        path: PathBuf::from("episode_01.ind.ass"),
        name: "episode_01.ind.ass".to_string(),
        stem: "episode_01.ind".to_string(),
        parsed: Some(EpisodeInfo::new("", "E", "01")),
    };

    sub_index.add(sub_eng);
    sub_index.add(sub_ind);

    let result = sub_index.lookup(&EpisodeInfo::new("", "E", "01"), None);
    assert!(result.is_some());
    let (path, lang) = result.unwrap();
    assert_eq!(path, PathBuf::from("episode_01.ind.ass"));
    assert_eq!(lang, "ind");
}

#[test]
fn test_sub_index_different_formats_not_duplicates() {
    let mut sub_index = SubIndex::new();
    let sub_srt = ParsedFile {
        path: PathBuf::from("episode_01.ind.srt"),
        name: "episode_01.ind.srt".to_string(),
        stem: "episode_01.ind".to_string(),
        parsed: Some(EpisodeInfo::new("", "E", "01")),
    };
    let sub_ass = ParsedFile {
        path: PathBuf::from("episode_01.ind.ass"),
        name: "episode_01.ind.ass".to_string(),
        stem: "episode_01.ind".to_string(),
        parsed: Some(EpisodeInfo::new("", "E", "01")),
    };

    sub_index.add(sub_srt);
    sub_index.add(sub_ass);

    let result = sub_index.lookup(&EpisodeInfo::new("", "E", "01"), None);
    assert!(result.is_some());
    let (path, lang) = result.unwrap();
    assert_eq!(path, PathBuf::from("episode_01.ind.ass"));
    assert_eq!(lang, "ind");
}

#[test]
fn test_sub_index_prefers_dialogue_candidate_same_lang() {
    let mut sub_index = SubIndex::new();
    let plain = ParsedFile {
        path: PathBuf::from("episode_01.ass"),
        name: "episode_01.ass".to_string(),
        stem: "episode_01".to_string(),
        parsed: Some(EpisodeInfo::new("", "E", "01")),
    };
    let dialogue = ParsedFile {
        path: PathBuf::from("episode_01_dialogue.ass"),
        name: "episode_01_dialogue.ass".to_string(),
        stem: "episode_01_dialogue".to_string(),
        parsed: Some(EpisodeInfo::new("", "E", "01")),
    };

    sub_index.add(plain);
    sub_index.add(dialogue.clone());

    assert_eq!(
        sub_index.lookup(&EpisodeInfo::new("", "E", "01"), None),
        Some((dialogue.path, "und".to_string()))
    );
}

#[test]
fn test_ancestor_asset_override_precedence() {
    let tmp = tempdir().unwrap();
    let root_dir = tmp.path().join("Anime");
    let season_dir = root_dir.join("Season 01");
    fs::create_dir_all(&season_dir).unwrap();

    fs::write(root_dir.join("cover.jpg"), b"root_cover").unwrap();
    fs::write(root_dir.join("Show - 01.eng.ass"), b"root_eng_sub").unwrap();

    fs::write(season_dir.join("Show - 01_cover.jpg"), b"local_cover").unwrap();
    fs::write(season_dir.join("Show - 01.ind.ass"), b"local_ind_sub").unwrap();

    let search_dirs = ancestor_dirs(&season_dir, &root_dir);
    assert_eq!(search_dirs, vec![root_dir.clone(), season_dir.clone()]);

    let resolver =
        AssetResolver::discover(&search_dirs, &season_dir.join("fonts"), None, None, None);

    let matched_sub = resolver
        .sub_index
        .lookup(&EpisodeInfo::new("1", "E", "01"), None);
    assert!(matched_sub.is_some());
    let (sub_path, lang) = matched_sub.unwrap();
    assert_eq!(sub_path, season_dir.join("Show - 01.ind.ass"));
    assert_eq!(lang, "ind");

    let matched_cover = resolver
        .cover_index
        .lookup(&EpisodeInfo::new("1", "E", "01"));
    assert_eq!(matched_cover, Some(season_dir.join("Show - 01_cover.jpg")));
}

#[test]
fn test_local_subtitle_next_to_mkv_wins_over_ancestor() {
    let tmp = tempdir().unwrap();
    let root_dir = tmp.path().join("Anime");
    let season_dir = root_dir.join("Season 01");
    fs::create_dir_all(&season_dir).unwrap();

    fs::write(root_dir.join("01.ass"), b"ancestor_short").unwrap();
    let local_sub = season_dir.join("[SuperLongFansubGroup] Anime Title - 01 [1080p FLAC BD].ass");
    fs::write(&local_sub, b"local_long").unwrap();

    let search_dirs = ancestor_dirs(&season_dir, &root_dir);
    let resolver =
        AssetResolver::discover(&search_dirs, &season_dir.join("fonts"), None, None, None);

    let matched = resolver
        .sub_index
        .lookup(&EpisodeInfo::new("1", "E", "01"), Some(&season_dir));
    assert!(matched.is_some());
    assert_eq!(matched.unwrap().0, local_sub);
}

#[test]
fn test_assets_dir_multi_level_fallback_hierarchy() {
    let tmp = tempdir().unwrap();
    let root_dir = tmp.path().join("Anime");
    let season_dir = root_dir.join("Season 01");
    fs::create_dir_all(&season_dir).unwrap();

    let assets_override = tmp.path().join("GlobalAssets");
    fs::create_dir_all(assets_override.join("fonts")).unwrap();
    fs::write(assets_override.join("fonts").join("FontA.ttf"), b"font").unwrap();
    fs::write(assets_override.join("cover.jpg"), b"root_cover").unwrap();

    let search_dirs = ancestor_dirs(&season_dir, &root_dir);
    let resolver = AssetResolver::discover(
        &search_dirs,
        &season_dir.join("fonts"),
        Some(&assets_override.join("Season 01")),
        None,
        None,
    );

    assert_eq!(resolver.fonts.count, 1);
    assert!(resolver.fallback_cover_index.is_some());
    assert!(resolver.fallback_cover_index.as_ref().unwrap().has_global());
}

#[test]
fn test_assets_dir_wins_tie_over_its_parent() {
    let tmp = tempdir().unwrap();
    let root_dir = tmp.path().join("Anime");
    let season_dir = root_dir.join("Season 01");
    fs::create_dir_all(&season_dir).unwrap();

    let assets_dir = tmp.path().join("Output").join("Season 01");
    fs::create_dir_all(&assets_dir).unwrap();
    let parent_dir = assets_dir.parent().unwrap();

    fs::write(assets_dir.join("Show - 01.ass"), b"assets_copy").unwrap();
    fs::write(parent_dir.join("Show - 01.ass"), b"parent_copy").unwrap();

    let search_dirs = ancestor_dirs(&season_dir, &root_dir);
    let resolver = AssetResolver::discover(
        &search_dirs,
        &season_dir.join("fonts"),
        Some(&assets_dir),
        None,
        None,
    );

    let matched = resolver
        .fallback_sub_index
        .as_ref()
        .unwrap()
        .lookup(&EpisodeInfo::new("1", "E", "01"), None);
    assert!(matched.is_some());
    assert_eq!(matched.unwrap().0, assets_dir.join("Show - 01.ass"));
}

#[test]
fn test_sole_leftover_claimed_by_first_eligible_video() {
    let tmp = tempdir().unwrap();
    let chap_xml = tmp.path().join("chapters.xml");
    fs::write(
        &chap_xml,
        b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
    )
    .unwrap();

    let ep1 = tmp.path().join("Show - 01.mkv");
    let ep2 = tmp.path().join("Show - 02.mkv");
    let videos = vec![make_video(&ep1), make_video(&ep2)];

    let ext_chap1 = tmp.path().join("Show - 01.xml");
    fs::write(
        &ext_chap1,
        b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
    )
    .unwrap();

    let mut resolver = AssetResolver::discover(
        &[tmp.path().to_path_buf()],
        &tmp.path().join("fonts"),
        None,
        None,
        None,
    );
    resolver.chap_index.add(make_video(&ext_chap1));

    let claims = claim_leftover_chapters(&videos, &HashMap::new(), &resolver);
    assert_eq!(claims.get(&ep2), Some(&chap_xml));
}

#[test]
fn test_no_claim_when_candidate_has_source_chapters() {
    let tmp = tempdir().unwrap();
    let chap_xml = tmp.path().join("chapters.xml");
    fs::write(
        &chap_xml,
        b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
    )
    .unwrap();

    let ep1 = tmp.path().join("Show - 01.mkv");
    let videos = vec![make_video(&ep1)];

    let resolver = AssetResolver::discover(
        &[tmp.path().to_path_buf()],
        &tmp.path().join("fonts"),
        None,
        None,
        None,
    );

    let mut inspections = HashMap::new();
    let inspected = InspectedVideo::new(MkvmergeMetadata {
        tracks: vec![],
        attachments: vec![],
        chapters: vec![MkvChapter {
            num_entries: Some(5),
        }],
    });
    inspections.insert(
        ep1.clone(),
        InspectionResult {
            video: Some(inspected),
            error: None,
        },
    );

    let claims = claim_leftover_chapters(&videos, &inspections, &resolver);
    assert!(claims.is_empty());
}

#[test]
fn test_prepare_release_directory() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let season = root.join("Season 01");
    fs::create_dir_all(&season).unwrap();
    fs::create_dir_all(season.join("fonts")).unwrap();
    fs::write(season.join("fonts").join("f.ttf"), b"font").unwrap();

    let ep1 = season.join("Show - 01.mkv");
    fs::write(&ep1, b"vid").unwrap();
    let videos = vec![make_video(&ep1)];

    let prepared = prepare_release_directory(
        &season,
        &root,
        &videos,
        ReleasePreparationOptions::default(),
    );

    assert_eq!(prepared.fonts_origin, FontsOrigin::Local);
    assert_eq!(prepared.videos.len(), 1);
}

#[test]
fn test_sub_index_is_empty_and_has_subtitles() {
    let mut sub_idx = SubIndex::new();
    assert!(sub_idx.is_empty());

    let resolver_empty = AssetResolver::new(
        FontInfo::default(),
        sub_idx.clone(),
        ChapterIndex::new(),
        CoverIndex::new(),
        None,
        None,
        None,
    );
    assert!(!resolver_empty.has_subtitles());

    let tmp = tempdir().unwrap();
    let sub_file = tmp.path().join("Show_S01E01.ass");
    fs::write(&sub_file, b"[Script Info]\n").unwrap();
    sub_idx.add(make_video(&sub_file));
    assert!(!sub_idx.is_empty());

    let resolver_with_sub = AssetResolver::new(
        FontInfo::default(),
        sub_idx,
        ChapterIndex::new(),
        CoverIndex::new(),
        None,
        None,
        None,
    );
    assert!(resolver_with_sub.has_subtitles());
}

#[test]
fn test_resolve_for_skip_reason_when_no_subs_vs_unmatched() {
    let tmp = tempdir().unwrap();
    let vid_path = tmp.path().join("Show_S01E01.mkv");
    fs::write(&vid_path, b"dummy").unwrap();
    let vid = make_video(&vid_path);

    // 1. Completely empty subtitles
    let resolver_no_subs = AssetResolver::new(
        FontInfo::default(),
        SubIndex::new(),
        ChapterIndex::new(),
        CoverIndex::new(),
        None,
        None,
        None,
    );
    let res_no_subs = resolver_no_subs.resolve_for(&vid, None, None, None);
    assert!(!res_no_subs.matched_subtitle);
    assert_eq!(
        res_no_subs.skip_reason.as_deref(),
        Some("no subtitle files found")
    );

    // 2. Subtitle present for E02 but video is E01
    let mut sub_idx = SubIndex::new();
    let sub_path = tmp.path().join("Show_S01E02.ass");
    fs::write(&sub_path, b"[Script Info]\n").unwrap();
    sub_idx.add(make_video(&sub_path));

    let resolver_unmatched = AssetResolver::new(
        FontInfo::default(),
        sub_idx,
        ChapterIndex::new(),
        CoverIndex::new(),
        None,
        None,
        None,
    );
    let res_unmatched = resolver_unmatched.resolve_for(&vid, None, None, None);
    assert!(!res_unmatched.matched_subtitle);
    assert_eq!(
        res_unmatched.skip_reason.as_deref(),
        Some("subtitle files found but none matched")
    );
}
