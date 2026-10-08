use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use mkv_core::assets::{
    claim_leftover_chapters, AssetResolver, ChapterIndex, CoverIndex, ReleasePreparer,
    ResolvedAssets, SubIndex,
};
use mkv_core::domain::{ancestor_dirs, EpisodeInfo, MkvTrack, TrackProperties};
use mkv_core::scan::{
    create_parsed_file, InspectedVideo, InspectionResult, MkvAttachment, MkvChapter,
    MkvmergeMetadata, ParsedFile, ScanCache,
};
use tempfile::tempdir;

const CHAPTER_XML: &[u8] =
    b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>";

fn with_episode(path: impl Into<PathBuf>, parsed: EpisodeInfo) -> ParsedFile {
    let mut file = create_parsed_file(path);
    file.parsed = Some(parsed);
    file
}

fn discover(search_dirs: &[PathBuf], assets_dir: Option<&Path>) -> AssetResolver {
    AssetResolver::discover(
        search_dirs,
        assets_dir,
        &mut ScanCache::default(),
        &mut HashMap::new(),
    )
}

/// Resolves `episode_01.mkv` against subtitles that all index as episode 01.
fn resolve_episode_01_subtitle(sub_paths: &[&str]) -> ResolvedAssets {
    let episode = || EpisodeInfo::new("", "E", "01");
    let mut sub_index = SubIndex::new();
    for path in sub_paths {
        sub_index.add(with_episode(*path, episode()));
    }
    AssetResolver {
        sub_index,
        ..Default::default()
    }
    .resolve_for(&with_episode("episode_01.mkv", episode()), None, None, None)
}

fn source_with_embedded_cover() -> InspectedVideo {
    InspectedVideo::new(MkvmergeMetadata {
        attachments: vec![MkvAttachment {
            file_name: Some("cover.jpg".into()),
            content_type: Some("image/jpeg".into()),
            ..Default::default()
        }],
        ..Default::default()
    })
}

#[test]
fn test_chap_index_lookup() {
    let tmp = tempdir().unwrap();
    let xml_path = tmp.path().join("Show_S01E01_chapters.xml");
    fs::write(&xml_path, CHAPTER_XML).unwrap();

    let mut chap_idx = ChapterIndex::new();
    chap_idx.add(&create_parsed_file(&xml_path));

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
        fs::write(p, CHAPTER_XML).unwrap();
    }

    let mut chap_idx = ChapterIndex::new();
    chap_idx.add(&create_parsed_file(&first));
    chap_idx.add(&create_parsed_file(&second));

    assert_eq!(
        chap_idx.lookup(&EpisodeInfo::new("1", "E", "01")),
        Some(first)
    );
}

#[test]
fn test_discover_scan_cache_hit_keeps_chapter_without_resniffing() {
    let tmp = tempdir().unwrap();
    let xml_path = tmp.path().join("Show - 01_chapters.xml");
    fs::write(&xml_path, CHAPTER_XML).unwrap();
    let search_dirs = [tmp.path().to_path_buf()];
    let episode = EpisodeInfo::new("", "E", "01");
    let mut scan_cache = ScanCache::default();
    let mut sub_classification_cache = HashMap::new();

    let first = AssetResolver::discover(
        &search_dirs,
        None,
        &mut scan_cache,
        &mut sub_classification_cache,
    );
    assert_eq!(first.chap_index.lookup(&episode), Some(xml_path.clone()));

    // The file no longer sniffs as chapter XML; a cache hit must not read it again.
    fs::write(&xml_path, b"<?xml version='1.0'?><Tags></Tags>").unwrap();
    let second = AssetResolver::discover(
        &search_dirs,
        None,
        &mut scan_cache,
        &mut sub_classification_cache,
    );
    assert_eq!(second.chap_index.lookup(&episode), Some(xml_path));
}

#[test]
fn test_cover_index_directory_cover() {
    let tmp = tempdir().unwrap();
    let cover_path = tmp.path().join("cover.jpg");

    let mut cover_idx = CoverIndex::new();
    cover_idx.add(create_parsed_file(&cover_path));

    let matched = cover_idx.lookup(&create_parsed_file(tmp.path().join("Show - 05.mkv")));
    assert_eq!(matched, Some(cover_path));
}

#[test]
fn test_cover_index_single_digit_cover_filename() {
    let tmp = tempdir().unwrap();
    let cover_path = tmp.path().join("Show 1_cover.jpg");

    let mut cover_idx = CoverIndex::new();
    cover_idx.add(create_parsed_file(&cover_path));

    let matched = cover_idx.lookup(&create_parsed_file(tmp.path().join("Show 1.mkv")));
    assert_eq!(matched, Some(cover_path));
}

#[test]
fn test_resolve_cover_local_first_then_fallback_gated_by_source_cover() {
    let tmp = tempdir().unwrap();
    let local_cover = tmp.path().join("Show 01_cover.jpg");
    let fallback_cover = tmp.path().join("assets").join("cover.jpg");
    let video = create_parsed_file(tmp.path().join("Show 01.mkv"));

    let mut fallback_idx = CoverIndex::new();
    fallback_idx.add(create_parsed_file(&fallback_cover));
    let mut resolver = AssetResolver {
        fallback_cover_index: Some(fallback_idx),
        ..Default::default()
    };

    assert_eq!(resolver.resolve_cov(&video, false), Some(fallback_cover));
    assert_eq!(resolver.resolve_cov(&video, true), None);

    resolver.cover_index.add(create_parsed_file(&local_cover));
    assert_eq!(
        resolver.resolve_cov(&video, false),
        Some(local_cover.clone())
    );
    assert_eq!(resolver.resolve_cov(&video, true), Some(local_cover));
}

#[test]
fn test_resolver_matches_other_language_subtitle() {
    let result = resolve_episode_01_subtitle(&["episode_01(ts).ass"]);
    assert_eq!(
        result.subtitle_file,
        Some(PathBuf::from("episode_01(ts).ass"))
    );
    assert_eq!(result.subtitle_lang, "ts");
}

#[test]
fn test_resolver_keeps_different_subtitle_languages_distinct() {
    let result = resolve_episode_01_subtitle(&["episode_01.eng.ass", "episode_01.ind.ass"]);
    assert_eq!(
        result.subtitle_file,
        Some(PathBuf::from("episode_01.ind.ass"))
    );
    assert_eq!(result.subtitle_lang, "ind");
}

#[test]
fn test_resolver_prefers_ass_over_srt_for_same_subtitle() {
    let result = resolve_episode_01_subtitle(&["episode_01.ind.srt", "episode_01.ind.ass"]);
    assert_eq!(
        result.subtitle_file,
        Some(PathBuf::from("episode_01.ind.ass"))
    );
    assert_eq!(result.subtitle_lang, "ind");
}

#[test]
fn test_resolver_prefers_dialogue_candidate_for_same_language() {
    let result = resolve_episode_01_subtitle(&["episode_01.ass", "episode_01_dialogue.ass"]);
    assert_eq!(
        result.subtitle_file,
        Some(PathBuf::from("episode_01_dialogue.ass"))
    );
    assert_eq!(result.subtitle_lang, "und");
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

    let resolver = discover(&search_dirs, None);

    let video = with_episode(
        season_dir.join("Show - 01.mkv"),
        EpisodeInfo::new("1", "E", "01"),
    );
    let resolved = resolver.resolve_for(&video, None, None, None);
    assert_eq!(
        resolved.subtitle_file,
        Some(season_dir.join("Show - 01.ind.ass"))
    );
    assert_eq!(resolved.subtitle_lang, "ind");
    assert_eq!(
        resolved.cover_file,
        Some(season_dir.join("Show - 01_cover.jpg"))
    );
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
    let resolver = discover(&search_dirs, None);

    let video_path = season_dir.join("[SuperLongFansubGroup] Anime Title - 01 [1080p FLAC BD].mkv");
    let video = with_episode(video_path, EpisodeInfo::new("1", "E", "01"));
    let resolved = resolver.resolve_for(&video, None, None, None);
    assert_eq!(resolved.subtitle_file, Some(local_sub));
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
    let resolver = discover(&search_dirs, Some(&assets_override.join("Season 01")));

    let video = create_parsed_file(season_dir.join("Show - 01.mkv"));
    assert_eq!(
        resolver.resolve_cov(&video, false),
        Some(assets_override.join("cover.jpg"))
    );
}

#[test]
fn test_assets_dir_subtitle_source_beats_parent_language_score() {
    let tmp = tempdir().unwrap();
    let root_dir = tmp.path().join("Anime");
    let season_dir = root_dir.join("Season 01");
    fs::create_dir_all(&season_dir).unwrap();

    let assets_dir = tmp.path().join("Output").join("Season 01");
    fs::create_dir_all(&assets_dir).unwrap();
    let parent_dir = assets_dir.parent().unwrap();

    fs::write(assets_dir.join("Show - 01.eng.ass"), b"assets_copy").unwrap();
    fs::write(parent_dir.join("Show - 01.ind.ass"), b"parent_copy").unwrap();

    let search_dirs = ancestor_dirs(&season_dir, &root_dir);
    let resolver = discover(&search_dirs, Some(&assets_dir));

    let video = with_episode(
        season_dir.join("Show - 01.mkv"),
        EpisodeInfo::new("1", "E", "01"),
    );
    assert_eq!(
        resolver.resolve_for(&video, None, None, None).subtitle_file,
        Some(assets_dir.join("Show - 01.eng.ass"))
    );
}

#[test]
fn test_cover_assets_directory_beats_parent_episode_name() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("input");
    let season = root.join("Season 01");
    let assets = tmp.path().join("assets/Season 01");
    fs::create_dir_all(&season).unwrap();
    fs::create_dir_all(&assets).unwrap();
    let local = assets.join("folder.jpg");
    let parent = assets.parent().unwrap().join("Show - 01_cover.jpg");
    fs::write(&local, b"local").unwrap();
    fs::write(&parent, b"parent").unwrap();
    let resolver = discover(&ancestor_dirs(&season, &root), Some(&assets));
    let video = create_parsed_file(season.join("Show - 01.mkv"));
    assert_eq!(resolver.resolve_cov(&video, false), Some(local));
    assert_eq!(resolver.resolve_cov(&video, true), None);
}

#[test]
fn test_subtitle_source_ladder_uses_embedded_before_ancestors() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let parent = root.join("parent");
    let season = parent.join("Season 01");
    let fallback = tmp.path().join("assets");
    fs::create_dir_all(&season).unwrap();
    fs::create_dir_all(&fallback).unwrap();
    let video = create_parsed_file(season.join("Show - 01.mkv"));
    let local = season.join("Show - 01.eng.ass");
    let near = parent.join("Show - 01.eng.ass");
    let far = root.join("Show - 01.ind.ass");
    let backup = fallback.join("Show - 01.ind.ass");
    let manual = fallback.join("manual.ass");
    for path in [&local, &near, &far, &backup, &manual] {
        fs::write(path, b"subtitle").unwrap();
    }
    let dirs = ancestor_dirs(&season, &root);
    let resolve = |inspected: Option<&InspectedVideo>, assigned: Option<&Path>| {
        discover(&dirs, Some(&fallback)).resolve_for(&video, inspected, None, assigned)
    };
    let embedded = InspectedVideo::new(MkvmergeMetadata {
        tracks: vec![MkvTrack {
            id: Some(2),
            track_type: "subtitles".into(),
            codec: None,
            properties: TrackProperties {
                language: Some("ind".into()),
                ..Default::default()
            },
        }],
        ..Default::default()
    });

    let result = resolve(Some(&embedded), None);
    assert_eq!(result.subtitle_file.as_ref(), Some(&local));
    assert_eq!(result.subtitle_lang, "eng");
    assert_eq!(
        resolve(Some(&embedded), Some(manual.as_path())).subtitle_file,
        Some(manual.clone())
    );

    fs::remove_file(&local).unwrap();
    let result = resolve(Some(&embedded), None);
    assert_eq!(result.embedded_sub_id, Some(2));
    assert_eq!(result.subtitle_file, None);
    assert_eq!(result.subtitle_lang, "ind");

    assert_eq!(resolve(None, None).subtitle_file, Some(near.clone()));
    fs::remove_file(&near).unwrap();
    assert_eq!(resolve(None, None).subtitle_file, Some(far.clone()));
    fs::remove_file(&far).unwrap();
    assert_eq!(resolve(None, None).subtitle_file, Some(backup));
}

#[test]
fn test_cover_nearest_directory_then_kind_and_natural_ties() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let season = root.join("Season 01");
    fs::create_dir_all(&season).unwrap();
    let video = create_parsed_file(season.join("Show - 01.mkv"));
    fs::write(&video.path, b"video").unwrap();
    let ancestor_episode = root.join("Show - 01_cover.jpg");
    let local_folder = season.join("folder.jpg");
    let local_cover = season.join("cover.png");
    let bare = season.join("Show - 01.webp");
    let dot = season.join("Show - 01.cover.jpg");
    let numbered = [
        season.join("Show - 01 10_cover.jpg"),
        season.join("Show - 01 2_cover.jpg"),
        season.join("Show - 01 02_cover.jpg"),
    ];
    for path in [&ancestor_episode, &local_folder, &local_cover, &bare, &dot]
        .into_iter()
        .chain(numbered.iter())
    {
        fs::write(path, b"cover").unwrap();
    }
    let dirs = ancestor_dirs(&season, &root);
    let cover = || {
        discover(&dirs, None)
            .resolve_for(&video, None, None, None)
            .cover_file
    };
    assert_eq!(cover(), Some(numbered[2].clone()));

    let mut forward = CoverIndex::new();
    let mut reverse = CoverIndex::new();
    for path in &numbered {
        forward.add(create_parsed_file(path));
    }
    for path in numbered.iter().rev() {
        reverse.add(create_parsed_file(path));
    }
    assert_eq!(forward.lookup(&video), Some(numbered[2].clone()));
    assert_eq!(reverse.lookup(&video), Some(numbered[2].clone()));

    for path in &numbered {
        fs::remove_file(path).unwrap();
    }
    // Each removal exposes the next-ranked candidate.
    for expected in [&dot, &bare, &local_cover, &local_folder] {
        assert_eq!(cover().as_ref(), Some(expected));
        fs::remove_file(expected).unwrap();
    }
    assert_eq!(cover(), Some(ancestor_episode));
}

#[test]
fn test_bare_cover_must_be_adjacent_sibling_of_video() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let season = root.join("Season 01");
    fs::create_dir_all(&season).unwrap();
    let root_video = create_parsed_file(root.join("Show - 01.mkv"));
    let nested_video = create_parsed_file(season.join("Show - 01.mkv"));
    let root_bare = root.join("Show - 01.jpg");
    for path in [&root_video.path, &nested_video.path, &root_bare] {
        fs::write(path, b"media").unwrap();
    }
    let source = source_with_embedded_cover();
    let dirs = ancestor_dirs(&season, &root);
    let cover = |video: &ParsedFile, inspected: Option<&InspectedVideo>| {
        discover(&dirs, None)
            .resolve_for(video, inspected, None, None)
            .cover_file
    };

    // An exact sibling bare cover still satisfies the adjacent form.
    assert_eq!(cover(&root_video, None), Some(root_bare.clone()));
    // A nested same-stem video keeps its embedded art instead of the root bare cover.
    assert_eq!(cover(&nested_video, Some(&source)), None);
    // Episode-aware ancestor matching is unaffected by the adjacency rule.
    let ancestor_episode = root.join("Show - 01_cover.jpg");
    fs::write(&ancestor_episode, b"image").unwrap();
    assert_eq!(
        cover(&nested_video, Some(&source)),
        Some(ancestor_episode.clone())
    );
    // Directory-wide `cover`/`folder` ancestors still apply to the nested video.
    fs::remove_file(&ancestor_episode).unwrap();
    let ancestor_cover = root.join("cover.jpg");
    fs::write(&ancestor_cover, b"image").unwrap();
    assert_eq!(cover(&nested_video, Some(&source)), Some(ancestor_cover));
}

#[test]
fn test_unparsed_cover_and_source_art_fallback_boundary() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let local = root.join("season");
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&local).unwrap();
    fs::create_dir_all(&assets).unwrap();
    let video = create_parsed_file(local.join("unknown.mkv"));
    fs::write(&video.path, b"video").unwrap();
    let fallback = assets.join("folder.png");
    let ancestor = root.join("folder.jpg");
    let bare = local.join("unknown.webp");
    for path in [&fallback, &ancestor, &bare] {
        fs::write(path, b"image").unwrap();
    }
    let source = source_with_embedded_cover();
    let dirs = ancestor_dirs(&local, &root);
    let cover = |video: &ParsedFile, inspected: Option<&InspectedVideo>| {
        discover(&dirs, Some(&assets))
            .resolve_for(video, inspected, None, None)
            .cover_file
    };
    assert_eq!(cover(&video, Some(&source)), Some(bare.clone()));
    fs::remove_file(&bare).unwrap();
    assert_eq!(cover(&video, Some(&source)), Some(ancestor.clone()));
    fs::remove_file(&ancestor).unwrap();
    assert_eq!(cover(&video, Some(&source)), None);
    assert_eq!(cover(&video, None), Some(fallback));

    let suffix_video = create_parsed_file(local.join("logo_cover.mkv"));
    fs::write(&suffix_video.path, b"video").unwrap();
    let suffix_bare = local.join("logo_cover.jpg");
    fs::write(&suffix_bare, b"image").unwrap();
    assert_eq!(cover(&suffix_video, Some(&source)), Some(suffix_bare));
}

#[test]
fn test_two_ambiguous_chapters_remain_unclaimed() {
    let tmp = tempdir().unwrap();
    let videos: Vec<_> = ["Show - 01.mkv", "Show - 02.mkv"]
        .iter()
        .map(|name| create_parsed_file(tmp.path().join(name)))
        .collect();
    for name in ["chapters_a.xml", "chapters_b.xml"] {
        fs::write(tmp.path().join(name), CHAPTER_XML).unwrap();
    }
    let resolver = discover(&[tmp.path().to_path_buf()], None);
    assert_eq!(resolver.chap_index.unmatched().len(), 2);
    assert!(claim_leftover_chapters(&videos, &HashMap::new(), &resolver).is_empty());
}

#[test]
fn test_sole_leftover_claimed_by_only_eligible_video() {
    let tmp = tempdir().unwrap();
    let chap_xml = tmp.path().join("chapters.xml");
    fs::write(&chap_xml, CHAPTER_XML).unwrap();
    // Episode 01 has its own chapter file, leaving episode 02 as the only candidate.
    fs::write(tmp.path().join("Show - 01.xml"), CHAPTER_XML).unwrap();

    let ep1 = tmp.path().join("Show - 01.mkv");
    let ep2 = tmp.path().join("Show - 02.mkv");
    let videos = vec![create_parsed_file(&ep1), create_parsed_file(&ep2)];

    let resolver = discover(&[tmp.path().to_path_buf()], None);

    let claims = claim_leftover_chapters(&videos, &HashMap::new(), &resolver);
    assert_eq!(claims, HashMap::from([(ep2, chap_xml)]));
}

#[test]
fn test_no_claim_when_candidate_has_source_chapters() {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("chapters.xml"), CHAPTER_XML).unwrap();

    let ep1 = tmp.path().join("Show - 01.mkv");
    let videos = vec![create_parsed_file(&ep1)];

    let resolver = discover(&[tmp.path().to_path_buf()], None);

    let inspections = HashMap::from([(
        ep1,
        InspectionResult::success(MkvmergeMetadata {
            chapters: vec![MkvChapter {
                num_entries: Some(5),
            }],
            ..Default::default()
        }),
    )]);

    let claims = claim_leftover_chapters(&videos, &inspections, &resolver);
    assert!(claims.is_empty());
}

#[test]
fn test_release_preparer_uses_nearest_fonts_dir_with_fonts() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let season = root.join("Season 01");
    for dir in [&root, &season] {
        fs::create_dir_all(dir.join("fonts")).unwrap();
        fs::write(dir.join("fonts").join("f.ttf"), b"font").unwrap();
    }
    let fonts_dir = || {
        ReleasePreparer::default()
            .prepare(&season, &root, None, None)
            .fonts_dir
    };

    assert_eq!(fonts_dir(), season.join("fonts"));
    fs::remove_dir_all(season.join("fonts")).unwrap();
    assert_eq!(fonts_dir(), root.join("fonts"));
}

#[test]
fn test_release_preparer_fonts_override_beats_release_and_ancestor_fonts() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let season = root.join("Season 01");
    for dir in [&root, &season] {
        fs::create_dir_all(dir.join("fonts")).unwrap();
        fs::write(dir.join("fonts").join("f.ttf"), b"font").unwrap();
    }
    let override_dir = tmp.path().join("custom_fonts");

    // The override wins even before it exists: prepare() never scans it.
    let mut preparer = ReleasePreparer::default();
    let prepared = preparer.prepare(&season, &root, Some(&override_dir), None);
    assert_eq!(prepared.fonts_dir, override_dir);

    fs::create_dir_all(&override_dir).unwrap();
    fs::write(override_dir.join("custom.otf"), b"font").unwrap();
    let fonts = preparer.fonts(&prepared.fonts_dir);
    assert_eq!(fonts.count, 1);
    assert!(fonts.args[3].ends_with("custom.otf"));
}

#[test]
fn test_sub_index_is_empty_and_has_subtitles() {
    let mut sub_idx = SubIndex::new();
    assert!(sub_idx.is_empty());
    let resolver_empty = AssetResolver {
        sub_index: sub_idx.clone(),
        ..Default::default()
    };
    assert!(!resolver_empty.has_subtitles());

    let tmp = tempdir().unwrap();
    let sub_file = tmp.path().join("Show_S01E01.ass");
    fs::write(&sub_file, b"[Script Info]\n").unwrap();
    sub_idx.add(create_parsed_file(&sub_file));
    assert!(!sub_idx.is_empty());

    let resolver_with_sub = AssetResolver {
        sub_index: sub_idx,
        ..Default::default()
    };
    assert!(resolver_with_sub.has_subtitles());
}

#[test]
fn test_resolve_for_skip_reason_when_no_subs_vs_unmatched() {
    let tmp = tempdir().unwrap();
    let vid_path = tmp.path().join("Show_S01E01.mkv");
    fs::write(&vid_path, b"dummy").unwrap();
    let vid = create_parsed_file(&vid_path);

    let res_no_subs = AssetResolver::default().resolve_for(&vid, None, None, None);
    assert!(!res_no_subs.matched_subtitle);
    assert_eq!(
        res_no_subs.skip_reason.as_deref(),
        Some("no subtitle files found")
    );

    // Subtitle present for E02 but video is E01
    let mut sub_idx = SubIndex::new();
    let sub_path = tmp.path().join("Show_S01E02.ass");
    fs::write(&sub_path, b"[Script Info]\n").unwrap();
    sub_idx.add(create_parsed_file(&sub_path));

    let res_unmatched = AssetResolver {
        sub_index: sub_idx,
        ..Default::default()
    }
    .resolve_for(&vid, None, None, None);
    assert!(!res_unmatched.matched_subtitle);
    assert_eq!(
        res_unmatched.skip_reason.as_deref(),
        Some("subtitle files found but none matched")
    );
}

#[test]
fn test_release_preparer_resolves_each_release_directory_from_shared_ancestor_subtitles() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("Anime");
    let season1 = root.join("Season 01");
    let season2 = root.join("Season 02");
    fs::create_dir_all(&season1).unwrap();
    fs::create_dir_all(&season2).unwrap();

    let sub_s01e01 = root.join("Show - S01E01.ind.ass");
    let sub_s02e01 = root.join("Show - S02E01.eng.ass");
    fs::write(&sub_s01e01, b"[Script Info]\nTitle: Test S1\n").unwrap();
    fs::write(&sub_s02e01, b"[Script Info]\nTitle: Test S2\n").unwrap();

    let ep_s1 = season1.join("Show - S01E01.mkv");
    let ep_s2 = season2.join("Show - S02E01.mkv");
    fs::write(&ep_s1, b"vid1").unwrap();
    fs::write(&ep_s2, b"vid2").unwrap();

    // One preparer serves both seasons, so the second reuses the root subtitles' classification.
    // The cache itself is asserted in the `assets` unit tests.
    let mut preparer = ReleasePreparer::default();

    let prepared1 = preparer.prepare(&season1, &root, None, None);
    let resolved1 = prepared1
        .resolver
        .resolve_for(&create_parsed_file(&ep_s1), None, None, None);
    assert_eq!(resolved1.subtitle_file, Some(sub_s01e01.clone()));
    assert_eq!(resolved1.subtitle_lang, "ind");

    let prepared2 = preparer.prepare(&season2, &root, None, None);
    let resolved2 = prepared2
        .resolver
        .resolve_for(&create_parsed_file(&ep_s2), None, None, None);
    assert_eq!(resolved2.subtitle_file, Some(sub_s02e01.clone()));
    assert_eq!(resolved2.subtitle_lang, "eng");
}

#[test]
fn test_release_preparer_discovers_unparsed_subtitles() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("Anime");
    let season = root.join("Season 01");
    fs::create_dir_all(&season).unwrap();

    fs::write(season.join("credits_roll.ass"), b"[Script Info]\n").unwrap();
    fs::write(
        season.join("extras.srt"),
        b"1\n00:00:01 --> 00:00:02\nExtra\n",
    )
    .unwrap();

    // They are not eagerly classified (the `assets` unit tests assert the cache stays empty),
    // but they still count as discovered subtitles.
    let prepared = ReleasePreparer::default().prepare(&season, &root, None, None);
    assert!(prepared.resolver.has_subtitles());
}

#[test]
fn test_sub_classification_cache_preserves_resolution_identity() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("Anime");
    let season = root.join("Season 01");
    fs::create_dir_all(&season).unwrap();

    let sub_path = root.join("Show - 01.ass");
    fs::write(&sub_path, b"[Script Info]\n").unwrap();
    let vid_path = season.join("Show - 01.mkv");
    fs::write(&vid_path, b"video").unwrap();
    let vid = create_parsed_file(&vid_path);

    let uncached = ReleasePreparer::default().prepare(&season, &root, None, None);
    let uncached_res = uncached.resolver.resolve_for(&vid, None, None, None);

    // A preparer that already prepared a sibling Release Directory holds the root subtitle's
    // classification, so this run reads it from the cache.
    let sibling = root.join("Season 02");
    fs::create_dir_all(&sibling).unwrap();
    let mut preparer = ReleasePreparer::default();
    preparer.prepare(&sibling, &root, None, None);
    let cached = preparer.prepare(&season, &root, None, None);
    let cached_res = cached.resolver.resolve_for(&vid, None, None, None);

    assert_eq!(uncached_res, cached_res);
}

#[test]
fn test_release_preparer_reuse_across_shared_ancestor_matches_fresh_preparers() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("Anime");
    let season1 = root.join("Season 01");
    let season2 = root.join("Season 02");
    let assets = tmp.path().join("assets");
    for dir in [&season1, &season2, &assets] {
        fs::create_dir_all(dir).unwrap();
    }
    // Both seasons share the root fonts and root subtitles; season 2 has nearer fonts of its own.
    for fonts in [root.join("fonts"), season2.join("fonts")] {
        fs::create_dir_all(&fonts).unwrap();
        fs::write(fonts.join("f.ttf"), b"font").unwrap();
    }
    let root_sub_s1 = root.join("Show - S01E01.ind.ass");
    let root_sub_s2 = root.join("Show - S02E01.eng.ass");
    let fallback_sub = assets.join("Show - S01E02.eng.ass");
    for sub in [&root_sub_s1, &root_sub_s2, &fallback_sub] {
        fs::write(sub, b"[Script Info]\n").unwrap();
    }
    let s1_videos = [
        season1.join("Show - S01E01.mkv"),
        season1.join("Show - S01E02.mkv"),
    ];
    let s2_videos = [season2.join("Show - S02E01.mkv")];

    let prepare = |preparer: &mut ReleasePreparer, dir: &Path, videos: &[PathBuf]| {
        let prepared = preparer.prepare(dir, &root, None, Some(&assets));
        let fonts = preparer.fonts(&prepared.fonts_dir);
        let resolved: Vec<ResolvedAssets> = videos
            .iter()
            .map(|video| {
                prepared
                    .resolver
                    .resolve_for(&create_parsed_file(video), None, None, None)
            })
            .collect();
        (prepared.fonts_dir, fonts, resolved)
    };

    let mut shared = ReleasePreparer::default();
    let shared_s1 = prepare(&mut shared, &season1, &s1_videos);
    let shared_s2 = prepare(&mut shared, &season2, &s2_videos);
    let fresh_s1 = prepare(&mut ReleasePreparer::default(), &season1, &s1_videos);
    let fresh_s2 = prepare(&mut ReleasePreparer::default(), &season2, &s2_videos);

    assert_eq!(shared_s1, fresh_s1);
    assert_eq!(shared_s2, fresh_s2);
    // The comparison above is not vacuous: each season resolved its own fonts and subtitles.
    assert_eq!(shared_s1.0, root.join("fonts"));
    assert_eq!(shared_s2.0, season2.join("fonts"));
    assert_eq!(shared_s1.2[0].subtitle_file, Some(root_sub_s1));
    assert_eq!(shared_s1.2[1].subtitle_file, Some(fallback_sub));
    assert_eq!(shared_s2.2[0].subtitle_file, Some(root_sub_s2));
}
