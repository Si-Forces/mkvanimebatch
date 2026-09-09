use mkv_core::domain::{
    detect_sub_lang, ep_type_label, format_ep, is_dialogue_track, natural_sort_key, parse_episode,
    pick_audio, score_subtitle_category, seasons_match, select_best_subtitle_track, AudioTrack,
    EpisodeInfo, MkvTrack, TrackProperties,
};
use std::collections::HashSet;

fn track(id: u32, lang: &str, codec: &str, name: &str) -> MkvTrack {
    MkvTrack {
        id: Some(id),
        track_type: "subtitles".to_string(),
        codec: None,
        properties: TrackProperties {
            codec_id: Some(codec.to_string()),
            language: Some(lang.to_string()),
            track_name: Some(name.to_string()),
            ..Default::default()
        },
    }
}

fn non_sub_track(id: u32, lang: &str) -> MkvTrack {
    MkvTrack {
        id: Some(id),
        track_type: "audio".to_string(),
        codec: None,
        properties: TrackProperties {
            language: Some(lang.to_string()),
            ..Default::default()
        },
    }
}

fn audio(lang: &str, channels: u32, name: &str, pos: u32, codec: &str) -> AudioTrack {
    AudioTrack {
        id: pos,
        pos,
        audio_idx: (pos - 1) as usize,
        lang: lang.to_string(),
        channels,
        name: name.to_string(),
        codec: codec.to_string(),
    }
}

// ─── EPISODE PARSER TESTS ────────────────────────────────────────────────────

#[test]
fn test_dash_episode_basic() {
    assert_eq!(
        parse_episode("[Group] Show - 01 [1080p]"),
        Some(EpisodeInfo::new("", "E", "01"))
    );
}

#[test]
fn test_dash_episode_two_digits() {
    assert_eq!(
        parse_episode("Show - 12"),
        Some(EpisodeInfo::new("", "E", "12"))
    );
}

#[test]
fn test_dash_episode_three_digits() {
    assert_eq!(
        parse_episode("Show - 100"),
        Some(EpisodeInfo::new("", "E", "100"))
    );
}

#[test]
fn test_dash_episode_leading_spaces() {
    assert_eq!(
        parse_episode("Show  -  05"),
        Some(EpisodeInfo::new("", "E", "05"))
    );
}

#[test]
fn test_sxxexx_season_and_episode() {
    assert_eq!(
        parse_episode("Show S02E10"),
        Some(EpisodeInfo::new("2", "E", "10"))
    );
}

#[test]
fn test_sxxexx_season_one() {
    assert_eq!(
        parse_episode("Show S01E03"),
        Some(EpisodeInfo::new("1", "E", "03"))
    );
}

#[test]
fn test_sxxexx_large_episode() {
    assert_eq!(
        parse_episode("Show S03E125"),
        Some(EpisodeInfo::new("3", "E", "125"))
    );
}

#[test]
fn test_sxxexx_version_suffix() {
    assert_eq!(
        parse_episode("Show S01E05v2"),
        Some(EpisodeInfo::new("1", "E", "05"))
    );
}

#[test]
fn test_bracket_episode() {
    assert_eq!(
        parse_episode("Show [07] [720p]"),
        Some(EpisodeInfo::new("", "E", "07"))
    );
}

#[test]
fn test_paren_episode() {
    assert_eq!(
        parse_episode("Show (13)"),
        Some(EpisodeInfo::new("", "E", "13"))
    );
}

#[test]
fn test_ep_keyword() {
    assert_eq!(
        parse_episode("Show EP03"),
        Some(EpisodeInfo::new("", "E", "03"))
    );
}

#[test]
fn test_episode_keyword_long() {
    assert_eq!(
        parse_episode("Show Episode 07"),
        Some(EpisodeInfo::new("", "E", "07"))
    );
}

#[test]
fn test_decimal_episode() {
    let res = parse_episode("Show - 12.5");
    assert!(res.is_some());
    let info = res.unwrap();
    assert_eq!(info.ep_type, "E");
    assert!(info.ep.starts_with("12."));
}

#[test]
fn test_ova_numbered() {
    assert_eq!(
        parse_episode("Show OVA 3"),
        Some(EpisodeInfo::new("", "OVA", "03"))
    );
}

#[test]
fn test_ova_no_space() {
    assert_eq!(
        parse_episode("Show OVA3"),
        Some(EpisodeInfo::new("", "OVA", "03"))
    );
}

#[test]
fn test_oad() {
    let res = parse_episode("Show OAD 01");
    assert!(res.is_some());
    assert_eq!(res.unwrap().ep_type, "OVA");
}

#[test]
fn test_sp_numbered() {
    assert_eq!(
        parse_episode("Show SP01"),
        Some(EpisodeInfo::new("", "SP", "01"))
    );
}

#[test]
fn test_special_keyword() {
    assert_eq!(
        parse_episode("Show Special 02"),
        Some(EpisodeInfo::new("", "SP", "02"))
    );
}

#[test]
fn test_end_marker() {
    assert_eq!(
        parse_episode("Show - 12 END"),
        Some(EpisodeInfo::new("", "E", "12"))
    );
}

#[test]
fn test_underscores_treated_as_spaces() {
    assert_eq!(
        parse_episode("Show_-_01"),
        Some(EpisodeInfo::new("", "E", "01"))
    );
}

#[test]
fn test_no_episode_returns_none() {
    assert_eq!(parse_episode("Random Movie Title"), None);
}

#[test]
fn test_empty_string_returns_none() {
    assert_eq!(parse_episode(""), None);
}

#[test]
fn test_parse_episode_strips_offset_suffix() {
    assert_eq!(
        parse_episode("Show - 01.[+5s]"),
        Some(EpisodeInfo::new("", "E", "01"))
    );
    assert_eq!(
        parse_episode("Show - 02.[-3s]"),
        Some(EpisodeInfo::new("", "E", "02"))
    );
    assert_eq!(
        parse_episode("Show - 03.[+1.5s]"),
        Some(EpisodeInfo::new("", "E", "03"))
    );
    assert_eq!(
        parse_episode("Show - 04.[-0.25s]"),
        Some(EpisodeInfo::new("", "E", "04"))
    );
}

#[test]
fn test_single_digit_zero_padded() {
    assert_eq!(
        parse_episode("Show - 5"),
        Some(EpisodeInfo::new("", "E", "05"))
    );
}

#[test]
fn test_double_digit_unchanged() {
    assert_eq!(
        parse_episode("Show - 15"),
        Some(EpisodeInfo::new("", "E", "15"))
    );
}

#[test]
fn test_release_year_guard_ignores_dash_years() {
    assert_eq!(
        parse_episode("Frieren - 2023 - 05"),
        Some(EpisodeInfo::new("", "E", "05"))
    );
    assert_eq!(
        parse_episode("One Piece - 1999 - 12"),
        Some(EpisodeInfo::new("", "E", "12"))
    );
    assert_eq!(
        parse_episode("Demon Lord 2099 - 01"),
        Some(EpisodeInfo::new("", "E", "01"))
    );
    assert_eq!(
        parse_episode("Demon Lord 2099 - 2024 - 01"),
        Some(EpisodeInfo::new("", "E", "01"))
    );
    assert_eq!(
        parse_episode("Show - 1234"),
        Some(EpisodeInfo::new("", "E", "1234"))
    );
    assert_eq!(
        parse_episode("Show - 2150"),
        Some(EpisodeInfo::new("", "E", "2150"))
    );
    assert_eq!(
        parse_episode("Show - EP2023"),
        Some(EpisodeInfo::new("", "E", "2023"))
    );
}

#[test]
fn test_complex_episode_filenames() {
    let cases = vec![
        ("[Group] 葬送のフリーレン - 05 [1080p].mkv", ("", "E", "05")),
        ("【推しの子】 第01話 (1080p).mkv", ("", "E", "01")),
        ("SPY×FAMILY - 12 [1080p].mkv", ("", "E", "12")),
        ("[Group] Show - 01-02 [1080p].mkv", ("", "E", "01")),
        ("Show S01E01-E02.mkv", ("1", "E", "01")),
        ("One Piece - 1089 [1080p].mkv", ("", "E", "1089")),
        ("Detective Conan - 1100.mkv", ("", "E", "1100")),
        ("Mob Psycho 100 II - 03.mkv", ("", "E", "03")),
        ("Demon Lord 2099 - 01.mkv", ("", "E", "01")),
        ("Show 4k 60fps - 02.mkv", ("", "E", "02")),
        ("Show - 12.5.mkv", ("", "E", "12.5")),
        ("Show - 0.5.mkv", ("", "E", "00.5")),
    ];

    for (filename, (s, ep_type, ep)) in cases {
        assert_eq!(
            parse_episode(filename),
            Some(EpisodeInfo::new(s, ep_type, ep)),
            "Failed on filename: {filename}"
        );
    }
}

// ─── SUBTITLE LANGUAGE DETECTION TESTS ───────────────────────────────────────

#[test]
fn test_detect_sub_lang_brackets() {
    assert_eq!(detect_sub_lang("EP01[ID]"), "ind");
    assert_eq!(detect_sub_lang("EP01[Indonesian]"), "ind");
    assert_eq!(detect_sub_lang("Show - 01 [Indo]"), "ind");
    assert_eq!(detect_sub_lang("EP01[English]"), "eng");
    assert_eq!(detect_sub_lang("EP01[ENG]"), "eng");
    assert_eq!(detect_sub_lang("EP01[Japanese]"), "jpn");
}

#[test]
fn test_detect_sub_lang_track_patterns() {
    assert_eq!(detect_sub_lang("Show_01_ind_track2"), "ind");
    assert_eq!(detect_sub_lang("Show_01_track2.ind"), "ind");
    assert_eq!(detect_sub_lang("Show_01_tso_track2"), "tso");
    assert_eq!(detect_sub_lang("Show_01_track2.ts"), "ts");
}

#[test]
fn test_detect_sub_lang_suffixes() {
    assert_eq!(detect_sub_lang("Show - 01.ind"), "ind");
    assert_eq!(detect_sub_lang("Show - 01_eng"), "eng");
    assert_eq!(detect_sub_lang("Show - 01 (ind)"), "ind");
    assert_eq!(detect_sub_lang("Show - 01"), "und");
    assert_eq!(detect_sub_lang("Show - 01.xyz"), "und");
}

#[test]
fn test_detect_sub_lang_case_and_shortcodes() {
    assert_eq!(detect_sub_lang("EP01[INDONESIAN]"), "ind");
    assert_eq!(detect_sub_lang("EP01[ind]"), "ind");
    assert_eq!(detect_sub_lang("EP01[id]"), "ind");
    assert_eq!(detect_sub_lang("EP01[en]"), "eng");
    assert_eq!(detect_sub_lang("EP01[eng]"), "eng");
    assert_eq!(detect_sub_lang("EP01[ja]"), "jpn");
    assert_eq!(detect_sub_lang("EP01[jpn]"), "jpn");
}

#[test]
fn test_detect_sub_lang_offset_suffix() {
    assert_eq!(detect_sub_lang("episode_01.ind.[+5s]"), "ind");
    assert_eq!(detect_sub_lang("episode_01.eng.[-3s]"), "eng");
    assert_eq!(detect_sub_lang("episode_01.jpn.[+2.5s]"), "jpn");
    assert_eq!(detect_sub_lang("episode_01.[+5s].ind"), "ind");
}

// ─── EP TYPE LABEL & SEASONS MATCH TESTS ─────────────────────────────────────

#[test]
fn test_ep_type_label() {
    assert_eq!(ep_type_label("E", false), "EP");
    assert_eq!(ep_type_label("E", true), "Episode");
    assert_eq!(ep_type_label("P", false), "Part");
    assert_eq!(ep_type_label("P", true), "Part");
    assert_eq!(ep_type_label("OVA", false), "OVA");
    assert_eq!(ep_type_label("OVA", true), "OVA");
    assert_eq!(ep_type_label("SP", false), "SP");
    assert_eq!(ep_type_label("SP", true), "Special");
    assert_eq!(ep_type_label("UNKNOWN", false), "UNKNOWN");
}

#[test]
fn test_seasons_match() {
    assert!(seasons_match("", ""));
    assert!(seasons_match("", "2"));
    assert!(seasons_match("2", ""));
    assert!(seasons_match("1", "1"));
    assert!(!seasons_match("1", "2"));
}

// ─── DIALOGUE TRACK TESTS ────────────────────────────────────────────────────

#[test]
fn test_is_dialogue_track() {
    assert!(is_dialogue_track(Some("Full Dialogue")));
    assert!(is_dialogue_track(Some("Main")));
    assert!(is_dialogue_track(Some("Indo Sub")));
    assert!(is_dialogue_track(Some("Indonesia")));
    assert!(is_dialogue_track(Some("Complete")));
    assert!(!is_dialogue_track(Some("Signs & Songs")));
    assert!(!is_dialogue_track(None));
    assert!(!is_dialogue_track(Some("")));
    assert!(!is_dialogue_track(Some("Subtitles Track 1")));
}

// ─── SUBTITLE TRACK SELECTION TESTS ──────────────────────────────────────────

#[test]
fn test_select_best_subtitle_track_empty() {
    assert_eq!(select_best_subtitle_track(&[], None), None);
}

#[test]
fn test_select_best_subtitle_track_only_audio() {
    let tracks = vec![non_sub_track(1, "jpn")];
    assert_eq!(select_best_subtitle_track(&tracks, None), None);
}

#[test]
fn test_select_best_subtitle_track_picks_ind_over_eng() {
    let tracks = vec![
        track(1, "eng", "s_text/ass", ""),
        track(2, "ind", "s_text/ass", ""),
    ];
    let (tid, lang, _) = select_best_subtitle_track(&tracks, None).unwrap();
    assert_eq!(tid, 2);
    assert_eq!(lang, "ind");
}

#[test]
fn test_select_best_subtitle_track_picks_id_as_primary() {
    let tracks = vec![
        track(1, "eng", "s_text/ass", ""),
        track(2, "id", "s_text/ass", ""),
    ];
    let (tid, lang, _) = select_best_subtitle_track(&tracks, None).unwrap();
    assert_eq!(tid, 2);
    assert_eq!(lang, "id");
}

#[test]
fn test_select_best_subtitle_track_picks_und_over_eng() {
    let tracks = vec![
        track(1, "eng", "s_text/ass", ""),
        track(2, "und", "s_text/ass", ""),
    ];
    let (tid, lang, _) = select_best_subtitle_track(&tracks, None).unwrap();
    assert_eq!(tid, 2);
    assert_eq!(lang, "und");
}

#[test]
fn test_select_best_subtitle_track_prefers_dialogue_same_lang() {
    let tracks = vec![
        track(1, "ind", "s_text/ass", "Signs"),
        track(2, "ind", "s_text/ass", "Full Dialogue"),
    ];
    let (tid, lang, is_diag) = select_best_subtitle_track(&tracks, None).unwrap();
    assert_eq!(tid, 2);
    assert_eq!(lang, "ind");
    assert!(is_diag);
}

#[test]
fn test_select_best_subtitle_track_english_fallback() {
    let tracks = vec![
        track(1, "eng", "s_text/ass", ""),
        track(2, "jpn", "s_text/ass", ""),
    ];
    let (tid, lang, _) = select_best_subtitle_track(&tracks, None).unwrap();
    assert_eq!(tid, 1);
    assert_eq!(lang, "eng");
}

#[test]
fn test_select_best_subtitle_track_last_resort_unrecognised_lang() {
    let tracks = vec![
        track(1, "zho", "s_text/ass", ""),
        track(2, "kor", "s_text/ass", ""),
    ];
    let (tid, lang, _) = select_best_subtitle_track(&tracks, None).unwrap();
    assert_eq!(tid, 1);
    assert_eq!(lang, "zho");
}

#[test]
fn test_select_best_subtitle_track_last_resort_prefers_dialogue() {
    let tracks = vec![
        track(1, "zho", "s_text/ass", "Signs"),
        track(2, "kor", "s_text/ass", "Full Dialogue"),
    ];
    let (tid, lang, is_diag) = select_best_subtitle_track(&tracks, None).unwrap();
    assert_eq!(tid, 2);
    assert_eq!(lang, "kor");
    assert!(is_diag);
}

#[test]
fn test_select_best_subtitle_track_last_resort_prefers_ass() {
    let tracks = vec![
        track(1, "zho", "s_text/utf8", ""),
        track(2, "tso", "s_text/ass", ""),
    ];
    let (tid, lang, _) = select_best_subtitle_track(&tracks, None).unwrap();
    assert_eq!(tid, 2);
    assert_eq!(lang, "tso");
}

#[test]
fn test_select_best_subtitle_track_codec_filter() {
    let mut filter = HashSet::new();
    filter.insert("s_text/ass".to_string());

    let tracks = vec![track(1, "ind", "s_text/utf8", "")];
    assert_eq!(select_best_subtitle_track(&tracks, Some(&filter)), None);

    let tracks2 = vec![track(1, "ind", "s_text/ass", "")];
    let (tid, _, _) = select_best_subtitle_track(&tracks2, Some(&filter)).unwrap();
    assert_eq!(tid, 1);
}

#[test]
fn test_select_best_subtitle_track_order_precedence() {
    let tracks = vec![
        track(1, "id", "s_text/ass", ""),
        track(2, "ind", "s_text/ass", ""),
    ];
    let (tid, lang, _) = select_best_subtitle_track(&tracks, None).unwrap();
    assert_eq!(tid, 2);
    assert_eq!(lang, "ind");

    let tracks2 = vec![
        track(1, "en", "s_text/ass", ""),
        track(2, "eng", "s_text/ass", ""),
    ];
    let (tid2, lang2, _) = select_best_subtitle_track(&tracks2, None).unwrap();
    assert_eq!(tid2, 2);
    assert_eq!(lang2, "eng");
}

// ─── AUDIO PICKER TESTS ──────────────────────────────────────────────────────

#[test]
fn test_audio_prefers_japanese_stereo() {
    let tracks = vec![
        audio("eng", 2, "", 1, "A_AAC"),
        audio("jpn", 2, "", 2, "A_AAC"),
    ];
    let (track, _) = pick_audio(&tracks, 0);
    let track = track.unwrap();
    assert_eq!(track.lang, "jpn");
    assert_eq!(track.pos, 2);
}

#[test]
fn test_audio_prefers_japanese_stereo_dialogue() {
    let tracks = vec![
        audio("jpn", 2, "Commentary", 1, "A_AAC"),
        audio("jpn", 2, "Full Dialogue", 2, "A_AAC"),
    ];
    let (track, _) = pick_audio(&tracks, 0);
    let track = track.unwrap();
    assert_eq!(track.pos, 2);
}

#[test]
fn test_audio_skips_commentary_track() {
    let tracks = vec![
        audio("jpn", 2, "Audio Commentary", 1, "A_AAC"),
        audio("jpn", 2, "Main Audio", 2, "A_AAC"),
    ];
    let (track, _) = pick_audio(&tracks, 0);
    let track = track.unwrap();
    assert_eq!(track.pos, 2);
}

#[test]
fn test_audio_prefers_stereo_over_surround() {
    let tracks = vec![
        audio("jpn", 6, "", 1, "A_AAC"),
        audio("jpn", 2, "", 2, "A_AAC"),
    ];
    let (track, _) = pick_audio(&tracks, 0);
    let track = track.unwrap();
    assert_eq!(track.channels, 2);
    assert_eq!(track.pos, 2);
}

#[test]
fn test_audio_und_stereo_fallback() {
    let tracks = vec![audio("und", 2, "", 1, "A_AAC")];
    let (track, _) = pick_audio(&tracks, 0);
    assert_eq!(track.unwrap().pos, 1);
}

#[test]
fn test_audio_stereo_fallback_no_japanese() {
    let tracks = vec![
        audio("eng", 2, "", 1, "A_AAC"),
        audio("eng", 6, "", 2, "A_AAC"),
    ];
    let (track, _) = pick_audio(&tracks, 0);
    assert_eq!(track.unwrap().channels, 2);
}

#[test]
fn test_audio_empty_tracks_returns_none() {
    let (track, reason) = pick_audio(&[], 0);
    assert!(track.is_none());
    assert_eq!(reason, "");
}

#[test]
fn test_audio_all_commentary_falls_back_to_japanese() {
    let tracks = vec![audio("jpn", 2, "Director Commentary", 1, "A_AAC")];
    let (track, reason) = pick_audio(&tracks, 0);
    assert!(track.is_some());
    assert!(
        reason.contains("Japanese") || reason.contains("fallback") || reason.contains("commentary")
    );
}

#[test]
fn test_audio_first_audio_after_video_as_last_resort() {
    let tracks = vec![audio("xyz", 6, "Some Audio", 5, "A_AAC")];
    let (track, _) = pick_audio(&tracks, 0);
    assert!(track.is_some());
}

#[test]
fn test_audio_japanese_preferred_over_undetermined() {
    let tracks = vec![
        audio("und", 2, "", 1, "A_AAC"),
        audio("jpn", 2, "", 2, "A_AAC"),
    ];
    let (track, _) = pick_audio(&tracks, 0);
    assert_eq!(track.unwrap().pos, 2);
}

#[test]
fn test_audio_non_commentary_preferred_over_commentary_even_if_commentary_is_japanese() {
    let tracks = vec![
        audio("jpn", 2, "Audio Commentary", 1, "A_AAC"),
        audio("eng", 2, "English Stereo", 2, "A_AC3"),
    ];
    let (track, reason) = pick_audio(&tracks, 0);
    let track = track.unwrap();
    assert_eq!(track.id, 2);
    assert!(reason.contains("Stereo fallback"));
}

#[test]
fn test_format_ep() {
    assert_eq!(format_ep("1"), "01");
    assert_eq!(format_ep("05"), "05");
    assert_eq!(format_ep("12.5"), "12.5");
    assert_eq!(format_ep("0.5"), "00.5");
}

#[test]
fn test_natural_sort_key() {
    let mut list = vec!["EP10", "EP2", "EP1", "EP20"];
    list.sort_by_key(|s| natural_sort_key(s));
    assert_eq!(list, vec!["EP1", "EP2", "EP10", "EP20"]);
}

#[test]
fn test_score_subtitle_category() {
    assert_eq!(score_subtitle_category("ind", true), 12);
    assert_eq!(score_subtitle_category("id", true), 11);
    assert_eq!(score_subtitle_category("und", true), 10);
    assert_eq!(score_subtitle_category("eng", true), 9);
    assert_eq!(score_subtitle_category("en", true), 8);
    assert_eq!(score_subtitle_category("other", true), 7);

    assert_eq!(score_subtitle_category("ind", false), 6);
    assert_eq!(score_subtitle_category("id", false), 5);
    assert_eq!(score_subtitle_category("und", false), 4);
    assert_eq!(score_subtitle_category("eng", false), 3);
    assert_eq!(score_subtitle_category("en", false), 2);
    assert_eq!(score_subtitle_category("other", false), 1);
}

#[test]
fn test_resolve_output_dir() {
    use mkv_core::domain::resolve_output_dir;
    use std::path::Path;

    let mkv = Path::new("/anime/season01/ep01.mkv");
    let input = Path::new("/anime");
    let out = Path::new("/output");

    assert_eq!(
        resolve_output_dir(mkv, input, Some(out)),
        Path::new("/output/season01")
    );
    assert_eq!(
        resolve_output_dir(mkv, input, None),
        Path::new("/anime/season01")
    );
}

#[test]
fn test_item_outcome_variants() {
    use mkv_core::domain::ItemOutcome;
    assert_eq!(
        serde_json::to_string(&ItemOutcome::Completed).unwrap(),
        "\"completed\""
    );
    assert_eq!(
        serde_json::to_string(&ItemOutcome::Skipped).unwrap(),
        "\"skipped\""
    );
    assert_eq!(
        serde_json::to_string(&ItemOutcome::Error).unwrap(),
        "\"error\""
    );
    assert_eq!(
        serde_json::to_string(&ItemOutcome::Cancelled).unwrap(),
        "\"cancelled\""
    );
    assert_eq!(
        serde_json::to_string(&ItemOutcome::Planned).unwrap(),
        "\"planned\""
    );
}

#[test]
fn test_safe_filename_traversal_prevention() {
    use mkv_core::domain::safe_filename;
    assert_eq!(safe_filename(".."), "unnamed");
    assert_eq!(safe_filename("."), "unnamed");
    assert_eq!(safe_filename(""), "unnamed");
    assert_eq!(safe_filename("../../../etc/passwd"), "passwd");
    assert_eq!(safe_filename("font.ttf"), "font.ttf");
    assert_eq!(safe_filename("font:test?.ttf"), "font_test_.ttf");
}

#[test]
fn test_output_stem_restores_base() {
    use mkv_core::domain::output_stem;
    assert_eq!(
        output_stem("[Group] Title - OVA 01", "OVA", "01"),
        "OVA01_[Group] Title - OVA 01"
    );
    assert_eq!(
        output_stem("[Group] Title - Special 02", "SP", "02"),
        "SP02_[Group] Title - Special 02"
    );
    assert_eq!(
        output_stem("[Group] Title - 01", "E", "01"),
        "[Group] Title - 01"
    );
}
