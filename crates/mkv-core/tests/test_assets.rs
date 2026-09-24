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
    create_parsed_file, InspectedVideo, InspectionResult, MkvAttachment, MkvChapter,
    MkvmergeMetadata, ParsedFile,
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

    let matched = cover_idx.lookup(&make_video(&tmp.path().join("Show - 05.mkv")));
    assert_eq!(matched, Some(cover_path));
}

#[test]
fn test_cover_index_single_digit_cover_filename() {
    let tmp = tempdir().unwrap();
    let cover_path = tmp.path().join("Show 1_cover.jpg");
    fs::write(&cover_path, b"img").unwrap();

    let mut cover_idx = CoverIndex::new();
    cover_idx.add(make_video(&cover_path));

    let matched = cover_idx.lookup(&make_video(&tmp.path().join("Show 1.mkv")));
    assert_eq!(matched, Some(cover_path));
    assert!(!cover_idx.has_global());
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

    let video = make_video(&tmp.path().join("Show 01.mkv"));
    let resolver = AssetResolver::new(
        FontInfo::default(),
        SubIndex::new(),
        ChapterIndex::new(),
        cover_idx,
        None,
        None,
        Some(fallback_idx),
    );

    let result = resolver.resolve_cov(&video, true);
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

    let video = make_video(&tmp.path().join("Show - 01.mkv"));
    let resolver = AssetResolver::new(
        FontInfo::default(),
        SubIndex::new(),
        ChapterIndex::new(),
        cover_idx,
        None,
        None,
        Some(fallback_idx),
    );

    let result = resolver.resolve_cov(&video, true);
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

    let video = make_video(&tmp.path().join("Show - 01.mkv"));
    let resolver = AssetResolver::new(
        FontInfo::default(),
        SubIndex::new(),
        ChapterIndex::new(),
        cover_idx,
        None,
        None,
        Some(fallback_idx),
    );

    let result = resolver.resolve_cov(&video, false);
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
        .lookup(&make_video(&season_dir.join("Show - 01.mkv")));
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
        .lookup_fallback(&EpisodeInfo::new("1", "E", "01"));
    assert!(matched.is_some());
    assert_eq!(matched.unwrap().0, assets_dir.join("Show - 01.eng.ass"));
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
    let resolver = AssetResolver::discover(
        &ancestor_dirs(&season, &root),
        &season.join("fonts"),
        Some(&assets),
        None,
        None,
    );
    let video = make_video(&season.join("Show - 01.mkv"));
    assert_eq!(resolver.resolve_cov(&video, false), Some(local));
    assert_eq!(resolver.resolve_cov(&video, true), None);
}

#[test]
fn test_subtitle_source_ladder_uses_embedded_before_ancestors() {
    use mkv_core::domain::{MkvTrack, TrackProperties};

    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let parent = root.join("parent");
    let season = parent.join("Season 01");
    let fallback = tmp.path().join("assets");
    fs::create_dir_all(&season).unwrap();
    fs::create_dir_all(&fallback).unwrap();
    let video = make_video(&season.join("Show - 01.mkv"));
    let local = season.join("Show - 01.eng.ass");
    let near = parent.join("Show - 01.eng.ass");
    let far = root.join("Show - 01.ind.ass");
    let backup = fallback.join("Show - 01.ind.ass");
    let manual = fallback.join("manual.ass");
    for path in [&local, &near, &far, &backup, &manual] {
        fs::write(path, b"subtitle").unwrap();
    }
    let dirs = ancestor_dirs(&season, &root);
    let discover =
        || AssetResolver::discover(&dirs, &season.join("fonts"), Some(&fallback), None, None);
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

    let result = discover()
        .resolve_for(&video, Some(&embedded), None, None)
        .unwrap();
    assert_eq!(result.subtitle_file.as_ref(), Some(&local));
    assert_eq!(result.subtitle_lang, "eng");
    assert_eq!(
        discover()
            .resolve_for(&video, Some(&embedded), None, Some(&manual))
            .unwrap()
            .subtitle_file,
        Some(manual)
    );

    fs::remove_file(&local).unwrap();
    let result = discover()
        .resolve_for(&video, Some(&embedded), None, None)
        .unwrap();
    assert_eq!(result.embedded_sub_id, Some(2));
    assert_eq!(result.subtitle_file, None);
    assert_eq!(result.subtitle_lang, "ind");

    let result = discover().resolve_for(&video, None, None, None).unwrap();
    assert_eq!(result.subtitle_file, Some(near.clone()));
    fs::remove_file(&near).unwrap();
    assert_eq!(
        discover()
            .resolve_for(&video, None, None, None)
            .unwrap()
            .subtitle_file,
        Some(far.clone())
    );
    fs::remove_file(&far).unwrap();
    assert_eq!(
        discover()
            .resolve_for(&video, None, None, None)
            .unwrap()
            .subtitle_file,
        Some(backup)
    );
}

#[test]
fn test_cover_nearest_directory_then_kind_and_natural_ties() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let season = root.join("Season 01");
    fs::create_dir_all(&season).unwrap();
    let video = make_video(&season.join("Show - 01.mkv"));
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
    let discover = || AssetResolver::discover(&dirs, &season.join("fonts"), None, None, None);
    assert_eq!(
        discover()
            .resolve_for(&video, None, None, None)
            .unwrap()
            .cover_file,
        Some(numbered[2].clone())
    );

    let mut forward = CoverIndex::new();
    let mut reverse = CoverIndex::new();
    for path in &numbered {
        forward.add(make_video(path));
    }
    for path in numbered.iter().rev() {
        reverse.add(make_video(path));
    }
    assert_eq!(forward.lookup(&video), Some(numbered[2].clone()));
    assert_eq!(reverse.lookup(&video), Some(numbered[2].clone()));

    for path in &numbered {
        fs::remove_file(path).unwrap();
    }
    assert_eq!(
        discover()
            .resolve_for(&video, None, None, None)
            .unwrap()
            .cover_file,
        Some(dot.clone())
    );
    fs::remove_file(&dot).unwrap();
    assert_eq!(
        discover()
            .resolve_for(&video, None, None, None)
            .unwrap()
            .cover_file,
        Some(bare.clone())
    );
    fs::remove_file(&bare).unwrap();
    assert_eq!(
        discover()
            .resolve_for(&video, None, None, None)
            .unwrap()
            .cover_file,
        Some(local_cover.clone())
    );
    fs::remove_file(&local_cover).unwrap();
    assert_eq!(
        discover()
            .resolve_for(&video, None, None, None)
            .unwrap()
            .cover_file,
        Some(local_folder.clone())
    );
    fs::remove_file(&local_folder).unwrap();
    assert_eq!(
        discover()
            .resolve_for(&video, None, None, None)
            .unwrap()
            .cover_file,
        Some(ancestor_episode)
    );
}

#[test]
fn test_unparsed_cover_and_source_art_fallback_boundary() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("root");
    let local = root.join("season");
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&local).unwrap();
    fs::create_dir_all(&assets).unwrap();
    let video = make_video(&local.join("unknown.mkv"));
    fs::write(&video.path, b"video").unwrap();
    let fallback = assets.join("folder.png");
    let ancestor = root.join("folder.jpg");
    let bare = local.join("unknown.webp");
    for path in [&fallback, &ancestor, &bare] {
        fs::write(path, b"image").unwrap();
    }
    let source = InspectedVideo::new(MkvmergeMetadata {
        attachments: vec![MkvAttachment {
            file_name: Some("cover.jpg".into()),
            content_type: Some("image/jpeg".into()),
            ..Default::default()
        }],
        ..Default::default()
    });
    let dirs = ancestor_dirs(&local, &root);
    let discover =
        || AssetResolver::discover(&dirs, &local.join("fonts"), Some(&assets), None, None);
    assert_eq!(
        discover()
            .resolve_for(&video, Some(&source), None, None)
            .unwrap()
            .cover_file,
        Some(bare.clone())
    );
    fs::remove_file(&bare).unwrap();
    assert_eq!(
        discover()
            .resolve_for(&video, Some(&source), None, None)
            .unwrap()
            .cover_file,
        Some(ancestor.clone())
    );
    fs::remove_file(&ancestor).unwrap();
    assert_eq!(
        discover()
            .resolve_for(&video, Some(&source), None, None)
            .unwrap()
            .cover_file,
        None
    );
    assert_eq!(
        discover()
            .resolve_for(&video, None, None, None)
            .unwrap()
            .cover_file,
        Some(fallback)
    );
    let suffix_video = make_video(&local.join("logo_cover.mkv"));
    fs::write(&suffix_video.path, b"video").unwrap();
    let suffix_bare = local.join("logo_cover.jpg");
    fs::write(&suffix_bare, b"image").unwrap();
    assert_eq!(
        discover()
            .resolve_for(&suffix_video, Some(&source), None, None)
            .unwrap()
            .cover_file,
        Some(suffix_bare)
    );
}

#[test]
fn test_two_ambiguous_chapters_remain_unclaimed() {
    let tmp = tempdir().unwrap();
    let videos: Vec<_> = ["Show - 01.mkv", "Show - 02.mkv"]
        .iter()
        .map(|name| make_video(&tmp.path().join(name)))
        .collect();
    for name in ["chapters_a.xml", "chapters_b.xml"] {
        fs::write(
            tmp.path().join(name),
            b"<Chapters><EditionEntry/></Chapters>",
        )
        .unwrap();
    }
    let resolver = AssetResolver::discover(
        &[tmp.path().to_path_buf()],
        &tmp.path().join("fonts"),
        None,
        None,
        None,
    );
    assert_eq!(resolver.chap_index.unmatched().len(), 2);
    assert!(claim_leftover_chapters(&videos, &HashMap::new(), &resolver).is_empty());
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
    let res_no_subs = resolver_no_subs
        .resolve_for(&vid, None, None, None)
        .unwrap();
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
    let res_unmatched = resolver_unmatched
        .resolve_for(&vid, None, None, None)
        .unwrap();
    assert!(!res_unmatched.matched_subtitle);
    assert_eq!(
        res_unmatched.skip_reason.as_deref(),
        Some("subtitle files found but none matched")
    );
}

#[test]
fn test_stale_assigned_subtitle_fails_closed_for_parsed_and_unparsed_videos() {
    let tmp = tempdir().unwrap();
    let automatic = tmp.path().join("Show_S01E01.ass");
    fs::write(&automatic, b"[Script Info]\n").unwrap();
    let manual = tmp.path().join("chosen_manual.ass");
    fs::write(&manual, b"[Script Info]\n").unwrap();
    let stale = tmp.path().join("deleted_manual.ass");

    let mut sub_index = SubIndex::new();
    sub_index.add(make_video(&automatic));
    let resolver = AssetResolver::new(
        FontInfo::default(),
        sub_index,
        ChapterIndex::new(),
        CoverIndex::new(),
        None,
        None,
        None,
    );

    let parsed = make_video(&tmp.path().join("Show_S01E01.mkv"));
    assert!(parsed.parsed.is_some());
    assert_eq!(
        resolver
            .resolve_for(&parsed, None, None, None)
            .unwrap()
            .subtitle_file,
        Some(automatic.clone())
    );
    let unparsed = make_video(&tmp.path().join("unknown.mkv"));
    assert!(unparsed.parsed.is_none());

    for video in [&parsed, &unparsed] {
        let error = resolver
            .resolve_for(video, None, None, Some(&stale))
            .unwrap_err();
        assert!(error.contains(&stale.display().to_string()), "{error}");
        assert_eq!(
            resolver
                .resolve_for(video, None, None, Some(&manual))
                .unwrap()
                .subtitle_file,
            Some(manual.clone())
        );
    }
}
