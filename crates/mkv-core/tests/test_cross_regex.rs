//! Regression test pinning episode parsing, subtitle language detection, filename
//! sanitization, dialogue-track detection, natural sorting, and chapter XML sniffing against
//! the reference snapshot in `fixtures/cross_regex_reference.json` (ADR-0010).

use std::fs;

use mkv_core::domain::{
    detect_sub_lang, is_dialogue_track, natural_sort_key, parse_episode, safe_filename,
    EpisodeInfo, EpisodeType,
};
use mkv_core::scan::is_chapter_xml;
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ParsedResult {
    season: String,
    ep_type: String,
    ep: String,
}

impl From<EpisodeInfo> for ParsedResult {
    fn from(info: EpisodeInfo) -> Self {
        Self {
            season: info.season,
            ep_type: match info.ep_type {
                EpisodeType::Episode => "E".to_string(),
                EpisodeType::Part => "P".to_string(),
                EpisodeType::Ova => "OVA".to_string(),
                EpisodeType::Special => "SP".to_string(),
            },
            ep: info.ep,
        }
    }
}

/// Expected outputs, index-aligned with the input lists below.
#[derive(Deserialize)]
struct Reference {
    episodes: Vec<Option<ParsedResult>>,
    sub_stems: Vec<String>,
    safe_names: Vec<String>,
    track_names: Vec<bool>,
    sorted_outputs: Vec<String>,
    chapter_xml_results: Vec<bool>,
}

const EPISODES: &[&str] = &[
    // Basic dashes
    "[Group] Show - 01 [1080p]",
    "Show - 12",
    "Show - 100",
    "Show  -  05",
    "Show - 01v2",
    "Show - 01-02",
    "Show - 01 - 02",
    "Show - 001",
    "Show - 0005",
    // Seasons & SxxExx
    "Show S02E10",
    "Show S01E03",
    "Show S03E125",
    "Show S01E05v2",
    "Show S1E1",
    "Show s01e05",
    "Show S01E01-E02",
    "Show S01P01",
    "Show S02P05v2",
    "Show Season 2 - 01",
    "Show season 2 - 01",
    "Show SEASON 2 - 01",
    "Show Season 01 - 05",
    "Show S2 - 05",
    "Show S02 - 05",
    "Show 2 Part - 01",
    // Keywords
    "Show EP03",
    "Show ep03",
    "Show Ep. 03",
    "Show Episode 07",
    "Show episode 07",
    "Show EPISODE 07",
    // Brackets & Parens
    "Show [07] [720p]",
    "Show (13)",
    // Decimals
    "Show - 12.5",
    "Show - 0.5",
    "Show - 1.25",
    "Show - 00.5",
    "Show EP 12.5",
    "Show - 1.5",
    "Show - 01.5",
    // Typed (OVA, Special)
    "Show OVA 3",
    "Show OVA3",
    "Show ova 3",
    "Show OAD 01",
    "Show ONA 01",
    "Show SP01",
    "Show SP 01",
    "Show Special 02",
    "Show Specials 02",
    "Show SPECIAL 02",
    "Show OVA",
    "Show Special",
    // CJK / Unicode
    "[Group] 葬送のフリーレン - 05 [1080p]",
    "【推しの子】 第01話 (1080p)",
    "SPY×FAMILY - 12 [1080p]",
    "第12.5話",
    "第03話",
    "第 04 話",
    "第100話",
    // Year guard & edge numbers
    "Frieren - 2023 - 05",
    "One Piece - 1999 - 12",
    "Demon Lord 2099 - 01",
    "Demon Lord 2099 - 2024 - 01",
    "Show - 1234",
    "Show - 2150",
    "Show - EP2023",
    "One Piece - 1089 [1080p]",
    "Detective Conan - 1100",
    "Mob Psycho 100 II - 03",
    "Show 4k 60fps - 02",
    "Show (2020) - 01",
    "Show [2021] - 02",
    "Show 2022 - 03",
    "Tokyo 2040 - 01",
    "2001 Nights - 01",
    // End markers
    "Show - 12 END",
    "Show - 12 FIN",
    "Show - 12 FINAL",
    "Show - 24 (End)",
    "Show - 24 [END]",
    // Delimiters
    "Show_-_01",
    "Show.01.1080p.mkv",
    "Show..01.mkv",
    "...Show...01...",
    "Dr. Stone - 01",
    "Fate.Zero - 01",
    "Steins.Gate.01.mkv",
    // Resolution & Codec guards
    "Show 720p - 01",
    "Show 1080p - 01",
    "Show 2160p - 01",
    "Show 480p - 01",
    "Show 10bit - 01",
    "Show FLAC - 01",
    "Show AAC 2.0 - 01",
    "Show x264 - 01",
    "Show x265 - 01",
    "Show HEVC - 01",
    // Subtitle offsets
    "Show - 01.[+5s]",
    "Show - 02.[-3s]",
    "Show - 03.[+1.5s]",
    "Show - 04.[-0.25s]",
    // Non-episodes
    "Random Movie Title",
    "Movie (2020)",
    "",
    "Trailer",
    "OST",
    "Show Complete Series",
    "Show Batch",
    "Show Season 1",
    "Show Movie",
    "Show NCED",
    "Show NCOP",
    "Show Menu",
    "Show PV",
];

const SUB_STEMS: &[&str] = &[
    "EP01[ID]",
    "EP01[id]",
    "EP01[Ind]",
    "EP01[Indonesian]",
    "Show - 01 [Indo]",
    "Show - 01 [indo]",
    "EP01[INDONESIAN]",
    "EP01[English]",
    "EP01[ENG]",
    "EP01[en]",
    "EP01[eng]",
    "EP01[Japanese]",
    "EP01[ja]",
    "EP01[jpn]",
    "Show_01_ind_track2",
    "Show_01_track2.ind",
    "Show_01_tso_track2",
    "Show_01_track2.ts",
    "Show - 01.ind",
    "Show - 01_eng",
    "Show - 01 (ind)",
    "Show - 01",
    "Show - 01.xyz",
    "episode_01.ind.[+5s]",
    "episode_01.eng.[-3s]",
    "episode_01.jpn.[+2.5s]",
    "episode_01.[+5s].ind",
];

const SAFE_NAMES: &[&str] = &[
    "foo/bar",
    "foo:bar",
    "foo*bar",
    "foo?bar",
    "foo\"bar",
    "foo<bar",
    "foo>bar",
    "foo|bar",
    "foo\\bar",
    "  hello world  ",
    "/:*?\"<>|\\",
    "",
    "   ",
    ".hidden",
    "..",
    ".",
    "CON",
    "prn.txt",
    "aux",
    "NUL.tar.gz",
    "normal_name-123",
];

const TRACK_NAMES: &[&str] = &[
    "Dialogue",
    "dialogue",
    "DIALOGUE",
    "Main Audio",
    "Full Subtitles",
    "Sign & Song",
    "Signs/Songs",
    "Signs",
    "Songs",
    "Complete",
    "Indonesian Dialogue",
    "Indo Sub",
    "English",
    "Commentary",
    "Audio Description",
];

const SORT_INPUTS: &[&str] = &[
    "EP10",
    "EP2",
    "EP1",
    "Show - 01.mkv",
    "Show - 1.mkv",
    "Show - 2.mkv",
    "Show - 10.mkv",
    "Show - 1.5.mkv",
    "Show - 01v2.mkv",
    "Show - 01v1.mkv",
    "Episode 100",
    "Episode 99",
    "Episode 02",
    "Season 1/EP1",
    "Season 10/EP1",
    "Season 2/EP1",
];

const CHAPTER_XML_SAMPLES: &[&str] = &[
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Chapters>\n  <EditionEntry>\n",
    "<chapters>",
    "<Chapters >",
    "<CHAPTERS>",
    "<Chapters xmlns=\"http://www.matroska.org\">",
    "<ChapterAtom>",
    "<other xml>",
    "no xml at all",
    "",
];

/// Pairs each input with its expected output, failing loudly if the fixture is out of step.
fn paired<'a, T>(
    section: &str,
    inputs: &'a [&'a str],
    expected: &'a [T],
) -> impl Iterator<Item = (&'a str, &'a T)> {
    assert_eq!(
        inputs.len(),
        expected.len(),
        "fixture section `{section}` does not match its input list"
    );
    inputs.iter().copied().zip(expected)
}

#[test]
fn test_reference_regression() {
    let reference: Reference =
        serde_json::from_str(include_str!("fixtures/cross_regex_reference.json"))
            .expect("deserialize cross regex reference fixture");
    let mut mismatches = Vec::new();

    for (name, expected) in paired("episodes", EPISODES, &reference.episodes) {
        let actual = parse_episode(name).map(ParsedResult::from);
        if actual != *expected {
            mismatches.push(format!(
                "Filename: '{name}'\n  Actual:   {actual:?}\n  Expected: {expected:?}"
            ));
        }
    }

    for (stem, expected) in paired("sub_stems", SUB_STEMS, &reference.sub_stems) {
        let actual = detect_sub_lang(stem);
        if actual != *expected {
            mismatches.push(format!(
                "Sub stem: '{stem}'\n  Actual:   {actual:?}\n  Expected: {expected:?}"
            ));
        }
    }

    for (name, expected_fixture) in paired("safe_names", SAFE_NAMES, &reference.safe_names) {
        let actual = safe_filename(name);

        // On Windows, backslash is a path separator: the final component of `foo\bar` is
        // `bar` and trailing backslashes are stripped. On POSIX/Linux, backslash is not a
        // separator, so `foo\bar` remains a single filename.
        #[cfg(windows)]
        let expected = match name {
            "foo\\bar" => "bar",
            "/:*?\"<>|\\" => "_______",
            _ => expected_fixture.as_str(),
        };
        #[cfg(not(windows))]
        let expected = expected_fixture.as_str();

        if actual != expected {
            mismatches.push(format!(
                "Safe filename: '{name}'\n  Actual:   {actual:?}\n  Expected: {expected:?}"
            ));
        }
    }

    for (name, expected) in paired("track_names", TRACK_NAMES, &reference.track_names) {
        let actual = is_dialogue_track(name);
        if actual != *expected {
            mismatches.push(format!(
                "Track name: '{name}'\n  Actual:   {actual:?}\n  Expected: {expected:?}"
            ));
        }
    }

    let mut sorted = SORT_INPUTS.to_vec();
    sorted.sort_by_key(|s| natural_sort_key(s));
    if sorted != reference.sorted_outputs {
        mismatches.push(format!(
            "Natural sort mismatch:\n  Actual:   {sorted:?}\n  Expected: {:?}",
            reference.sorted_outputs
        ));
    }

    let tmp = tempfile::tempdir().unwrap();
    for (i, (sample, expected)) in paired(
        "chapter_xml_results",
        CHAPTER_XML_SAMPLES,
        &reference.chapter_xml_results,
    )
    .enumerate()
    {
        let path = tmp.path().join(format!("sample_{i}.xml"));
        fs::write(&path, sample).unwrap();
        let actual = is_chapter_xml(&path);
        if actual != *expected {
            mismatches.push(format!(
                "Chapter XML sample: '{sample}'\n  Actual:   {actual:?}\n  Expected: {expected:?}"
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "Reference regression mismatches found ({}):\n{}",
        mismatches.len(),
        mismatches.join("\n\n")
    );
}
