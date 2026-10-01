use mkv_core::domain::{
    detect_sub_lang, is_dialogue_track, natural_sort_key, output_stem, parse_episode, promote_file,
    resolve_output_dir, safe_filename, score_subtitle_category, seasons_match, select_audio_track,
    select_best_subtitle_track, unique_file_token, AudioTrack, EpisodeInfo, EpisodeType,
    ItemOutcome, MkvTrack, TrackProperties,
};
use std::collections::HashSet;
use std::path::Path;
use tempfile::tempdir;

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
fn test_parse_episode_cases() {
    let cases = [
        // Dash-separated numbers
        ("[Group] Show - 01 [1080p]", ("", "E", "01")),
        ("Show - 12", ("", "E", "12")),
        ("Show - 100", ("", "E", "100")),
        ("Show  -  05", ("", "E", "05")),
        ("Show - 5", ("", "E", "05")),
        ("Show - 12 END", ("", "E", "12")),
        ("Show_-_01", ("", "E", "01")),
        // SxxExx
        ("Show S02E10", ("2", "E", "10")),
        ("Show S01E03", ("1", "E", "03")),
        ("Show S03E125", ("3", "E", "125")),
        ("Show S01E05v2", ("1", "E", "05")),
        // Bracketed, parenthesised, and keyword numbers
        ("Show [07] [720p]", ("", "E", "07")),
        ("Show (13)", ("", "E", "13")),
        ("Show EP03", ("", "E", "03")),
        ("Show Episode 07", ("", "E", "07")),
        // Decimal episodes
        ("Show - 12.5", ("", "E", "12.5")),
        ("Show - 12.5.mkv", ("", "E", "12.5")),
        ("Show - 0.5.mkv", ("", "E", "00.5")),
        // OVA / Special
        ("Show OVA 3", ("", "OVA", "03")),
        ("Show OVA3", ("", "OVA", "03")),
        ("Show OAD 01", ("", "OVA", "01")),
        ("Show SP01", ("", "SP", "01")),
        ("Show Special 02", ("", "SP", "02")),
        // Subtitle offset suffixes are ignored
        ("Show - 01.[+5s]", ("", "E", "01")),
        ("Show - 02.[-3s]", ("", "E", "02")),
        ("Show - 03.[+1.5s]", ("", "E", "03")),
        ("Show - 04.[-0.25s]", ("", "E", "04")),
        // Release years are not episode numbers
        ("Frieren - 2023 - 05", ("", "E", "05")),
        ("One Piece - 1999 - 12", ("", "E", "12")),
        ("Demon Lord 2099 - 01", ("", "E", "01")),
        ("Demon Lord 2099 - 2024 - 01", ("", "E", "01")),
        ("Show - 1234", ("", "E", "1234")),
        ("Show - 2150", ("", "E", "2150")),
        ("Show - EP2023", ("", "E", "2023")),
        // Real-world release names
        ("[Group] 葬送のフリーレン - 05 [1080p].mkv", ("", "E", "05")),
        ("【推しの子】 第01話 (1080p).mkv", ("", "E", "01")),
        ("SPY×FAMILY - 12 [1080p].mkv", ("", "E", "12")),
        ("[Group] Show - 01-02 [1080p].mkv", ("", "E", "01")),
        ("Show S01E01-E02.mkv", ("1", "E", "01")),
        ("One Piece - 1089 [1080p].mkv", ("", "E", "1089")),
        ("Detective Conan - 1100.mkv", ("", "E", "1100")),
        ("Mob Psycho 100 II - 03.mkv", ("", "E", "03")),
        ("Show 4k 60fps - 02.mkv", ("", "E", "02")),
    ];

    for (filename, (season, ep_type, ep)) in cases {
        assert_eq!(
            parse_episode(filename),
            Some(EpisodeInfo::new(season, ep_type, ep)),
            "Failed on filename: {filename}"
        );
    }
}

#[test]
fn test_parse_episode_without_number_returns_none() {
    assert_eq!(parse_episode("Random Movie Title"), None);
    assert_eq!(parse_episode(""), None);
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
    assert_eq!(EpisodeType::Episode.label(false), "EP");
    assert_eq!(EpisodeType::Episode.label(true), "Episode");
    assert_eq!(EpisodeType::Part.label(false), "Part");
    assert_eq!(EpisodeType::Part.label(true), "Part");
    assert_eq!(EpisodeType::Ova.label(false), "OVA");
    assert_eq!(EpisodeType::Ova.label(true), "OVA");
    assert_eq!(EpisodeType::Special.label(false), "SP");
    assert_eq!(EpisodeType::Special.label(true), "Special");
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
    assert!(is_dialogue_track("Full Dialogue"));
    assert!(is_dialogue_track("Main"));
    assert!(is_dialogue_track("Indo Sub"));
    assert!(is_dialogue_track("Indonesia"));
    assert!(is_dialogue_track("Complete"));
    assert!(!is_dialogue_track("Signs & Songs"));
    assert!(!is_dialogue_track(""));
    assert!(!is_dialogue_track("Subtitles Track 1"));
}

// ─── SUBTITLE TRACK SELECTION TESTS ──────────────────────────────────────────

#[test]
fn test_select_best_subtitle_track_ignores_non_subtitles() {
    assert_eq!(select_best_subtitle_track(&[], None), None);
    assert_eq!(
        select_best_subtitle_track(&[non_sub_track(1, "jpn")], None),
        None
    );
}

#[test]
fn test_select_best_subtitle_track_language_precedence() {
    let cases = [
        (("eng", "ind"), (2, "ind")),
        (("eng", "id"), (2, "id")),
        (("eng", "und"), (2, "und")),
        (("id", "ind"), (2, "ind")),
        (("en", "eng"), (2, "eng")),
        (("eng", "jpn"), (1, "eng")),
        // Unrecognised languages tie; the lower track ID wins.
        (("zho", "kor"), (1, "zho")),
    ];
    for ((first, second), (tid, lang)) in cases {
        let tracks = [
            track(1, first, "s_text/ass", ""),
            track(2, second, "s_text/ass", ""),
        ];
        assert_eq!(
            select_best_subtitle_track(&tracks, None),
            Some((tid, lang.to_string())),
            "{first} vs {second}"
        );
    }
}

#[test]
fn test_select_best_subtitle_track_prefers_dialogue_within_category() {
    for lang in ["ind", "kor"] {
        let tracks = [
            track(1, lang, "s_text/ass", "Signs"),
            track(2, lang, "s_text/ass", "Full Dialogue"),
        ];
        assert_eq!(
            select_best_subtitle_track(&tracks, None),
            Some((2, lang.to_string()))
        );
    }
}

#[test]
fn test_select_best_subtitle_track_prefers_ass_over_language() {
    let tracks = [
        track(1, "ind", "s_text/utf8", ""),
        track(2, "tso", "s_text/ass", ""),
    ];
    assert_eq!(
        select_best_subtitle_track(&tracks, None),
        Some((2, "tso".to_string()))
    );
}

#[test]
fn test_select_best_subtitle_track_codec_filter() {
    let filter = HashSet::from(["s_text/ass".to_string()]);

    let tracks = [track(1, "ind", "s_text/utf8", "")];
    assert_eq!(select_best_subtitle_track(&tracks, Some(&filter)), None);

    let tracks = [track(1, "ind", "S_TEXT/ASS", "")];
    assert_eq!(
        select_best_subtitle_track(&tracks, Some(&filter)),
        Some((1, "ind".to_string()))
    );
}

#[test]
fn test_score_subtitle_category_ranking() {
    let ranked = [
        ("ind", true),
        ("id", true),
        ("und", true),
        ("eng", true),
        ("en", true),
        ("zho", true),
        ("ind", false),
        ("id", false),
        ("und", false),
        ("eng", false),
        ("en", false),
        ("zho", false),
    ];
    let scores: Vec<i32> = ranked
        .iter()
        .map(|&(lang, is_ass)| score_subtitle_category(lang, is_ass))
        .collect();
    assert!(
        scores.windows(2).all(|w| w[0] > w[1]),
        "scores must strictly decrease in rank order: {scores:?}"
    );
}

// ─── AUDIO PICKER TESTS ──────────────────────────────────────────────────────

fn selected_pos(tracks: &[AudioTrack], video_pos: u32) -> u32 {
    select_audio_track(tracks, video_pos)
        .expect("a track must be selected")
        .pos
}

#[test]
fn test_audio_empty_tracks_returns_none() {
    assert!(select_audio_track(&[], 0).is_none());
}

#[test]
fn test_audio_ranking() {
    let cases = [
        (
            "japanese stereo over english stereo",
            vec![
                audio("eng", 2, "", 1, "A_AAC"),
                audio("jpn", 2, "", 2, "A_AAC"),
            ],
            2,
        ),
        (
            "dialogue over commentary",
            vec![
                audio("jpn", 2, "Commentary", 1, "A_AAC"),
                audio("jpn", 2, "Full Dialogue", 2, "A_AAC"),
            ],
            2,
        ),
        (
            "commentary track skipped",
            vec![
                audio("jpn", 2, "Audio Commentary", 1, "A_AAC"),
                audio("jpn", 2, "Main Audio", 2, "A_AAC"),
            ],
            2,
        ),
        (
            "stereo over surround",
            vec![
                audio("jpn", 6, "", 1, "A_AAC"),
                audio("jpn", 2, "", 2, "A_AAC"),
            ],
            2,
        ),
        (
            "japanese over undetermined",
            vec![
                audio("und", 2, "", 1, "A_AAC"),
                audio("jpn", 2, "", 2, "A_AAC"),
            ],
            2,
        ),
        (
            "stereo fallback without japanese",
            vec![
                audio("eng", 6, "", 1, "A_AAC"),
                audio("eng", 2, "", 2, "A_AAC"),
            ],
            2,
        ),
        (
            "non-commentary english over japanese commentary",
            vec![
                audio("jpn", 2, "Audio Commentary", 1, "A_AAC"),
                audio("eng", 2, "English Stereo", 2, "A_AC3"),
            ],
            2,
        ),
        (
            "earliest of equal tracks",
            vec![
                audio("und", 2, "", 1, "A_AAC"),
                audio("und", 2, "", 2, "A_AAC"),
            ],
            1,
        ),
    ];
    for (label, tracks, expected_pos) in cases {
        assert_eq!(selected_pos(&tracks, 0), expected_pos, "{label}");
    }
}

#[test]
fn test_audio_first_track_after_video_as_last_resort() {
    let tracks = [
        audio("xyz", 6, "Some Audio", 1, "A_AAC"),
        audio("xyz", 6, "Some Audio", 3, "A_AAC"),
    ];
    assert_eq!(selected_pos(&tracks, 2), 3);
}

#[test]
fn test_audio_all_commentary_still_ranks_japanese_first() {
    let tracks = [
        audio("eng", 2, "Commentary", 1, "A_AAC"),
        audio("jpn", 6, "Director Commentary", 2, "A_AAC"),
    ];
    assert_eq!(selected_pos(&tracks, 0), 2);
}

#[test]
fn test_audio_commentary_word_matching() {
    // Both tracks are identical apart from the candidate name, so the earliest-position
    // tie-break only applies when the candidate is NOT excluded as commentary.
    let cases = [
        ("Commentary", 2),
        ("Commentaries", 2),
        ("Comments", 2),
        ("commentary", 2),
        ("Producer Comment", 2),
        ("producer comments", 2),
        ("Audio Description", 2),
        ("AD", 2),
        ("ad", 2),
        ("Extension", 2),
        ("Broadcast", 1),
        ("Addition", 1),
    ];
    for (name, expected_pos) in cases {
        let tracks = [
            audio("eng", 2, name, 1, "A_AAC"),
            audio("eng", 2, "Main Audio", 2, "A_AAC"),
        ];
        assert_eq!(selected_pos(&tracks, 0), expected_pos, "{name:?}");
    }
}

// ─── NAMING & PATH HELPER TESTS ──────────────────────────────────────────────

#[test]
fn test_natural_sort_key() {
    let mut list = vec!["EP10", "EP2", "EP1", "EP20"];
    list.sort_by_key(|s| natural_sort_key(s));
    assert_eq!(list, vec!["EP1", "EP2", "EP10", "EP20"]);
}

#[test]
fn test_resolve_output_dir() {
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
    // Outside the input tree: flatten into the override.
    assert_eq!(
        resolve_output_dir(Path::new("/elsewhere/ep01.mkv"), input, Some(out)),
        Path::new("/output")
    );
}

#[test]
fn test_item_outcome_wire_format() {
    for (outcome, wire) in [
        (ItemOutcome::Completed, "\"completed\""),
        (ItemOutcome::Skipped, "\"skipped\""),
        (ItemOutcome::Error, "\"error\""),
        (ItemOutcome::Cancelled, "\"cancelled\""),
        (ItemOutcome::Planned, "\"planned\""),
    ] {
        assert_eq!(serde_json::to_string(&outcome).unwrap(), wire);
    }
}

#[test]
fn test_safe_filename_traversal_prevention() {
    assert_eq!(safe_filename(".."), "unnamed");
    assert_eq!(safe_filename("."), "unnamed");
    assert_eq!(safe_filename(""), "unnamed");
    assert_eq!(safe_filename("../../../etc/passwd"), "passwd");
    assert_eq!(safe_filename("font.ttf"), "font.ttf");
    assert_eq!(safe_filename("font:test?.ttf"), "font_test_.ttf");
}

#[test]
fn test_safe_filename_windows_reserved_names() {
    for device in ["con", "CON", "prn", "aux", "NUL"] {
        assert_eq!(safe_filename(device), format!("_{device}"));
        assert_eq!(
            safe_filename(&format!("{device}.ass")),
            format!("_{device}.ass")
        );
    }
    for i in 1..=9 {
        for name in [
            format!("com{i}"),
            format!("COM{i}.ass"),
            format!("lpt{i}"),
            format!("LPT{i}.ass"),
        ] {
            assert_eq!(safe_filename(&name), format!("_{name}"));
        }
    }
    assert_eq!(safe_filename("CoM1.sub"), "_CoM1.sub");
    assert_eq!(safe_filename("LpT3.ttf"), "_LpT3.ttf");

    // Reserved words as substrings, or out-of-range device numbers, are fine.
    for name in [
        "context.ass",
        "auxiliary.ttf",
        "my_con.ass",
        "com10.txt",
        "lpt0.txt",
    ] {
        assert_eq!(safe_filename(name), name);
    }
}

#[test]
fn test_output_stem_restores_base() {
    assert_eq!(
        output_stem("[Group] Title - OVA 01", &EpisodeType::Ova, "01"),
        "OVA01_[Group] Title - OVA 01"
    );
    assert_eq!(
        output_stem("[Group] Title - Special 02", &EpisodeType::Special, "02"),
        "SP02_[Group] Title - Special 02"
    );
    assert_eq!(
        output_stem("[Group] Title - 01", &EpisodeType::Episode, "01"),
        "[Group] Title - 01"
    );
}

// ─── FILE PROMOTION TESTS ────────────────────────────────────────────────────

#[test]
fn test_promote_file_basic_and_overwrite() {
    let tmp = tempdir().unwrap();
    let staged = tmp.path().join("staged.txt");
    let dest = tmp.path().join("sub_dir").join("final.txt");

    std::fs::write(&staged, b"initial content").unwrap();
    assert!(promote_file(&staged, &dest).is_ok());
    assert!(!staged.exists());
    assert_eq!(std::fs::read(&dest).unwrap(), b"initial content");

    let staged2 = tmp.path().join("staged2.txt");
    std::fs::write(&staged2, b"updated content").unwrap();
    assert!(promote_file(&staged2, &dest).is_ok());
    assert!(!staged2.exists());
    assert_eq!(std::fs::read(&dest).unwrap(), b"updated content");
}

/// A missing `staged` fails the opening `rename` with `NotFound` and creates no destination.
/// `std::fs::copy` would fail the same way, so this cannot show whether the copy fallback
/// ran; `test_promote_file_readonly_source_parent_returns_error` covers the gate itself.
#[test]
fn test_promote_file_missing_source_creates_no_destination() {
    let tmp = tempdir().unwrap();
    let staged = tmp.path().join("does_not_exist.bin");
    let dest = tmp.path().join("out").join("final.bin");

    let err = promote_file(&staged, &dest).expect_err("missing staged file must not promote");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert!(
        !dest.exists(),
        "failed promotion must not create the destination"
    );
}

/// The one case where gating the copy fallback on a device boundary changes the outcome.
///
/// `rename(2)` requires write permission on both parents. With the staged file's directory
/// read-only, the rename fails `PermissionDenied` although a copy could still read the
/// payload; `promote_file` deliberately reports the error instead of copying.
///
/// Root bypasses the mode bits, which would let the rename succeed, so the test skips there.
#[cfg(unix)]
#[test]
fn test_promote_file_readonly_source_parent_returns_error() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0) {
        eprintln!("skipped: root bypasses directory permissions");
        return;
    }

    let tmp = tempdir().unwrap();
    let staged_dir = tmp.path().join("staged_dir");
    std::fs::create_dir(&staged_dir).unwrap();
    let staged = staged_dir.join("payload.bin");
    std::fs::write(&staged, b"payload").unwrap();
    std::fs::set_permissions(&staged_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let dest = tmp.path().join("final.bin");
    let err = promote_file(&staged, &dest).expect_err("a read-only source parent must not promote");

    // Restore write permission so the tempdir can be torn down.
    std::fs::set_permissions(&staged_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        !dest.exists(),
        "a refused promotion must not create the destination"
    );
}

/// Covers the copy fallback for a genuine `EXDEV`. Skips where the host offers no second
/// filesystem, which is the only portable way to force one.
#[cfg(unix)]
#[test]
fn test_promote_file_cross_device_copy() {
    use std::os::unix::fs::MetadataExt;
    use tempfile::Builder;

    let src_dir = tempdir().unwrap();
    let src_dev = std::fs::metadata(src_dir.path()).unwrap().dev();
    let dest_dir = ["/var/tmp", "/dev/shm", "/run"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find_map(|p| {
            if !p.is_dir() || !std::fs::metadata(&p).is_ok_and(|m| m.dev() != src_dev) {
                return None;
            }
            Builder::new().prefix("mkvcore_test_").tempdir_in(&p).ok()
        });
    let Some(dest_dir) = dest_dir else {
        eprintln!(
            "skipped: no writable second filesystem available to force a cross-device rename"
        );
        return;
    };

    let staged = src_dir.path().join("staged.bin");
    std::fs::write(&staged, b"cross-device payload").unwrap();
    let dest = dest_dir.path().join("final.bin");

    promote_file(&staged, &dest).expect("a genuine EXDEV must fall through to the copy");
    assert_eq!(std::fs::read(&dest).unwrap(), b"cross-device payload");
    assert!(!staged.exists(), "the copy path consumes the staged file");

    // Count entries rather than filtering on a prefix, so a leaked temporary sibling or
    // `.bak.` backup is caught whatever it is named.
    let leftovers: Vec<_> = std::fs::read_dir(dest_dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name != "final.bin")
        .collect();
    assert!(
        leftovers.is_empty(),
        "promotion must leave only 'final.bin', found: {leftovers:?}"
    );
}

#[test]
fn test_unique_file_token_distinct() {
    let tokens: HashSet<String> = (0..64).map(|_| unique_file_token()).collect();
    assert_eq!(tokens.len(), 64);
}
