//! Season matching in asset resolution (ADR-0004, 2026-10-09 amendment): within one directory
//! an exact season beats an open (unknown-season) one for subtitles, chapters and episode
//! covers, while the directory distance still ranks first.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use mkv_core::assets::{AssetResolver, ChapterIndex, CoverIndex, ReleasePreparer, SubIndex};
use mkv_core::scan::{create_parsed_file, ParsedFile, ScanCache};
use tempfile::tempdir;

const CHAPTER_XML: &[u8] =
    b"<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>";

/// Index-order permutations are covered by also trying each list reversed.
fn both_orders<'a>(names: &[&'a str]) -> [Vec<&'a str>; 2] {
    let mut reversed = names.to_vec();
    reversed.reverse();
    [names.to_vec(), reversed]
}

fn season2_video(dir: &str) -> ParsedFile {
    create_parsed_file(format!("{dir}/Show - S02E01.mkv"))
}

fn resolve_subtitle(video: &ParsedFile, subs: &[&str]) -> Option<PathBuf> {
    let mut sub_index = SubIndex::new();
    for sub in subs {
        sub_index.add(create_parsed_file(sub));
    }
    AssetResolver {
        sub_index,
        ..Default::default()
    }
    .resolve_for(video, None, None, None)
    .subtitle_file
}

#[test]
fn test_subtitle_exact_season_beats_open_season_in_either_name_order() {
    // The open names are shorter or sort first, so only the season can pick the exact file.
    let exact = "/lib/A - S02E01.ass";
    for open in ["/lib/A - 01.ass", "/lib/Z - 01.ass"] {
        for subs in both_orders(&[exact, open]) {
            assert_eq!(
                resolve_subtitle(&season2_video("/lib"), &subs),
                Some(PathBuf::from(exact)),
                "{subs:?}"
            );
        }
    }
}

#[test]
fn test_subtitle_exact_season_beats_a_better_language_score() {
    // An open ASS (score) must not beat an exact SRT in the same directory.
    let subs = ["/lib/Show - 01.ass", "/lib/Show - S02E01.srt"];
    assert_eq!(
        resolve_subtitle(&season2_video("/lib"), &subs),
        Some(PathBuf::from("/lib/Show - S02E01.srt"))
    );
}

#[test]
fn test_subtitle_for_an_s00_episode_with_a_qualifier_word_needs_season_0_or_open() {
    // "S00E01 - Special" is episode 01 of season 0, not Special 00, so it now takes a
    // season-0 or open subtitle, the exact one first, and never a season-1 one.
    let video = create_parsed_file("/lib/Show - S00E01 - Special.mkv");
    let (exact, open) = ("/lib/Show - S00E01.ass", "/lib/Show - 01.ass");
    assert_eq!(resolve_subtitle(&video, &["/lib/Show - S01E01.ass"]), None);
    assert_eq!(resolve_subtitle(&video, &[open]), Some(PathBuf::from(open)));
    for subs in both_orders(&[open, exact, "/lib/Show - S01E01.ass"]) {
        assert_eq!(
            resolve_subtitle(&video, &subs),
            Some(PathBuf::from(exact)),
            "{subs:?}"
        );
    }
}

#[test]
fn test_subtitle_nearer_open_beats_farther_exact() {
    for subs in both_orders(&["/lib/Show - S02E01.ass", "/lib/s2/Show - 01.ass"]) {
        // Same directory first, then ancestors by distance (ADR-0004): the season never
        // overrides the distance.
        assert_eq!(
            resolve_subtitle(&season2_video("/lib/s2"), &subs),
            Some(PathBuf::from("/lib/s2/Show - 01.ass"))
        );
        assert_eq!(
            resolve_subtitle(&season2_video("/lib/s2/ep"), &subs),
            Some(PathBuf::from("/lib/s2/Show - 01.ass"))
        );
    }
}

#[test]
fn test_resolver_on_disk_picks_the_exact_season_subtitle() {
    let tmp = tempdir().unwrap();
    let season = tmp.path().join("Season 02");
    fs::create_dir_all(&season).unwrap();
    for name in ["Show - 01.ass", "Show - S02E01.ass"] {
        fs::write(season.join(name), b"[Script Info]\n").unwrap();
    }
    let video = season.join("Show - S02E01.mkv");
    fs::write(&video, b"vid").unwrap();

    let resolved = ReleasePreparer::default()
        .prepare(&season, tmp.path(), None, None)
        .resolver
        .resolve_for(&create_parsed_file(&video), None, None, None);

    assert_eq!(
        resolved.subtitle_file,
        Some(season.join("Show - S02E01.ass"))
    );
}

fn resolve_cover(video: &ParsedFile, covers: &[&str]) -> Option<PathBuf> {
    let mut cover_index = CoverIndex::new();
    for cover in covers {
        cover_index.add(create_parsed_file(cover));
    }
    cover_index.lookup(video)
}

#[test]
fn test_cover_exact_season_beats_open_season_in_either_name_order() {
    // The open `_cover` kind ranks before the exact `.cover` kind, and its name sorts either
    // side of the exact one, so only the season can pick the exact file.
    let exact = "/lib/M - S02E01.cover.jpg";
    for open in ["/lib/A - 01_cover.jpg", "/lib/Z - 01_cover.jpg"] {
        for covers in both_orders(&[exact, open]) {
            assert_eq!(
                resolve_cover(&season2_video("/lib"), &covers),
                Some(PathBuf::from(exact)),
                "{covers:?}"
            );
        }
    }
}

#[test]
fn test_cover_nearer_open_beats_farther_exact() {
    for covers in both_orders(&[
        "/lib/Show - S02E01_cover.jpg",
        "/lib/s2/Show - 01_cover.jpg",
    ]) {
        assert_eq!(
            resolve_cover(&season2_video("/lib/s2"), &covers),
            Some(PathBuf::from("/lib/s2/Show - 01_cover.jpg"))
        );
    }
}

fn chapter_index(files: &[&str]) -> ChapterIndex {
    let mut index = ChapterIndex::new();
    for file in files {
        index.add(&create_parsed_file(file));
    }
    index
}

fn resolve_chapter(
    index: ChapterIndex,
    fallback: bool,
    video: &ParsedFile,
) -> (Option<PathBuf>, Option<String>) {
    let resolver = if fallback {
        AssetResolver {
            fallback_chap_index: Some(index),
            ..Default::default()
        }
    } else {
        AssetResolver {
            chap_index: index,
            ..Default::default()
        }
    };
    let resolved = resolver.resolve_for(video, None, None, None);
    (resolved.chapter_file, resolved.chapter_warning)
}

#[test]
fn test_chapter_exact_season_beats_open_season_in_either_index_order() {
    let exact = "/lib/Show - S02E01.xml";
    for files in both_orders(&[exact, "/lib/Show - 01.xml"]) {
        for fallback in [false, true] {
            let (chosen, warning) =
                resolve_chapter(chapter_index(&files), fallback, &season2_video("/lib"));
            assert_eq!(chosen, Some(PathBuf::from(exact)), "{files:?}");
            // The open file matches the video's season too, so it is named as passed over.
            assert_eq!(
                warning.as_deref(),
                Some("More than one chapter file for this episode in '/lib': using 'Show - S02E01.xml', ignoring 'Show - 01.xml'"),
                "{files:?} fallback={fallback}"
            );
        }
    }
}

#[test]
fn test_chapter_nearer_open_beats_farther_exact_without_a_warning() {
    // The release-tree index lists the nearest directory first.
    let near_first = ["/lib/s2/Show - 01.xml", "/lib/Show - S02E01.xml"];
    for fallback in [false, true] {
        let (chosen, warning) = resolve_chapter(
            chapter_index(&near_first),
            fallback,
            &season2_video("/lib/s2"),
        );
        assert_eq!(chosen, Some(PathBuf::from("/lib/s2/Show - 01.xml")));
        assert_eq!(warning, None);
    }
}

#[test]
fn test_chapter_exact_names_the_open_file_and_another_season_names_nothing() {
    // [(A, "1"), (B, "")]: a season-1 video takes the exact A and names the open B ...
    let index = || chapter_index(&["/lib/Show - S01E01.xml", "/lib/Show - 01.xml"]);
    for fallback in [false, true] {
        let (chosen, warning) = resolve_chapter(
            index(),
            fallback,
            &create_parsed_file("/lib/Show - S01E01.mkv"),
        );
        assert_eq!(chosen, Some(PathBuf::from("/lib/Show - S01E01.xml")));
        assert!(warning.unwrap().ends_with("ignoring 'Show - 01.xml'"));

        // ... and a season-2 video takes B alone: A is another known season, never named.
        let (chosen, warning) = resolve_chapter(index(), fallback, &season2_video("/lib"));
        assert_eq!(chosen, Some(PathBuf::from("/lib/Show - 01.xml")));
        assert_eq!(warning, None);
    }
}

#[test]
fn test_chapter_unseasoned_video_takes_the_first_and_names_the_other() {
    for fallback in [false, true] {
        let (chosen, warning) = resolve_chapter(
            chapter_index(&["/lib/Show - S01E01.xml", "/lib/Show - S02E01.xml"]),
            fallback,
            &create_parsed_file("/lib/Show - 01.mkv"),
        );
        assert_eq!(chosen, Some(PathBuf::from("/lib/Show - S01E01.xml")));
        assert_eq!(
            warning.as_deref(),
            Some("More than one chapter file for this episode in '/lib': using 'Show - S01E01.xml', ignoring 'Show - S02E01.xml'")
        );
    }
}

#[test]
fn test_chapter_ladder_through_discover_prefers_the_nearer_open_file() {
    // The ancestor holds the exact chapter, the deeper directory an open one. `discover` fills
    // the release-tree index deepest first, so the deeper file wins and nothing is named.
    let tmp = tempdir().unwrap();
    let deeper = tmp.path().join("s2");
    fs::create_dir_all(&deeper).unwrap();
    fs::write(tmp.path().join("Show - S02E01.xml"), CHAPTER_XML).unwrap();
    fs::write(deeper.join("Show - 01.xml"), CHAPTER_XML).unwrap();

    let resolver = AssetResolver::discover(
        &[tmp.path().to_path_buf(), deeper.clone()],
        None,
        &mut ScanCache::default(),
        &mut HashMap::new(),
    );
    let resolved = resolver.resolve_for(&season2_video(deeper.to_str().unwrap()), None, None, None);

    assert_eq!(resolved.chapter_file, Some(deeper.join("Show - 01.xml")));
    assert_eq!(resolved.chapter_warning, None);
}

fn resolve_fallback_subtitle(video: &ParsedFile, subs: &[&str]) -> Option<PathBuf> {
    let mut sub_index = SubIndex::new();
    for sub in subs {
        sub_index.add(create_parsed_file(sub));
    }
    AssetResolver {
        fallback_sub_index: Some(sub_index),
        ..Default::default()
    }
    .resolve_for(video, None, None, None)
    .subtitle_file
}

#[test]
fn test_assets_dir_subtitle_exact_season_beats_open_season() {
    // The open ASS has the better category score and sorts first; only the season picks the SRT.
    for subs in both_orders(&["/assets/A - 01.ass", "/assets/Z - S02E01.srt"]) {
        assert_eq!(
            resolve_fallback_subtitle(&season2_video("/lib"), &subs),
            Some(PathBuf::from("/assets/Z - S02E01.srt")),
            "{subs:?}"
        );
    }
}

fn resolve_fallback_cover(video: &ParsedFile, covers: &[&str]) -> Option<PathBuf> {
    let mut cover_index = CoverIndex::new();
    for cover in covers {
        cover_index.add(create_parsed_file(cover));
    }
    AssetResolver {
        fallback_cover_index: Some(cover_index),
        ..Default::default()
    }
    .resolve_cov(video, false)
}

#[test]
fn test_assets_dir_cover_exact_season_beats_open_season() {
    // Both are `.cover` covers, so kind cannot decide; the open one sorts first by name.
    for covers in both_orders(&["/assets/A - 01.cover.jpg", "/assets/Z - S02E01.cover.jpg"]) {
        assert_eq!(
            resolve_fallback_cover(&season2_video("/lib"), &covers),
            Some(PathBuf::from("/assets/Z - S02E01.cover.jpg")),
            "{covers:?}"
        );
    }
}

#[test]
fn test_cover_exact_season_beats_open_season_of_the_same_kind() {
    // An exact `.cover` file outranks an open one of the same kind; the open name sorts first.
    // A same-stem bare cover is a different kind and loses on kind, so it does not test the
    // open key; see the bare-stem test below.
    for covers in both_orders(&["/lib/A - 01.cover.jpg", "/lib/Z - S02E01.cover.jpg"]) {
        assert_eq!(
            resolve_cover(&season2_video("/lib"), &covers),
            Some(PathBuf::from("/lib/Z - S02E01.cover.jpg")),
            "{covers:?}"
        );
    }
}

#[test]
fn test_cover_dot_cover_beats_the_same_stem_bare_cover() {
    // The bare stem is open and the `.cover` file is exact, but kind order decides too: this
    // locks the bare-stem position in either index order.
    for covers in both_orders(&["/lib/Show - S02E01.cover.jpg", "/lib/Show - S02E01.jpg"]) {
        assert_eq!(
            resolve_cover(&season2_video("/lib"), &covers),
            Some(PathBuf::from("/lib/Show - S02E01.cover.jpg")),
            "{covers:?}"
        );
    }
}
