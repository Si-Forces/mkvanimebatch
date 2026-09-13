//! Cross-language verification test: Rust (mkv-core) vs Python (mkv_domain, mkv_assets, mkv_scan, mkv_remux_job).
//!
//! Ensures regex matching, episode parsing, language detection, filename sanitization,
//! natural sorting, track classification, chapter XML sniffing, and cover pattern matching
//! behave identically across both implementations.

use mkv_core::domain::{
    detect_sub_lang, is_dialogue_track, natural_sort_key, parse_episode, safe_filename,
    EpisodeInfo, EpisodeType,
};
use regex::bytes::Regex as BytesRegex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::LazyLock;

static COMMENTARY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(commentary|comment|audio\s+description|ad|extension)\b").unwrap()
});

static CHAPTER_TAG_RE: LazyLock<BytesRegex> =
    LazyLock::new(|| BytesRegex::new(r"(?i)<Chapters[ >]").unwrap());

static COVER_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?P<stem>.+)_cover$").unwrap());

static BITRATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+)([kM]?)$").unwrap());

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Serialize, Deserialize)]
struct CrossCheckPayload {
    episodes: Vec<String>,
    sub_stems: Vec<String>,
    safe_names: Vec<String>,
    track_names: Vec<String>,
    commentary_names: Vec<String>,
    sort_inputs: Vec<String>,
    chapter_xml_samples: Vec<String>,
    cover_stems: Vec<String>,
    bitrate_strings: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CrossCheckResponse {
    episodes: Vec<Option<ParsedResult>>,
    sub_stems: Vec<String>,
    safe_names: Vec<String>,
    track_names: Vec<bool>,
    commentary_names: Vec<bool>,
    sorted_outputs: Vec<String>,
    chapter_xml_results: Vec<bool>,
    cover_stem_results: Vec<Option<String>>,
    bitrate_results: Vec<Option<(u64, String)>>,
}

fn query_python(payload: &CrossCheckPayload) -> CrossCheckResponse {
    let json_input = serde_json::to_string(payload).expect("serialize payload");
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(manifest_dir);

    let script = r#"
import json, os, re, sys
from pathlib import Path

repo_root = os.environ.get("REPO_ROOT")
if repo_root:
    sys.path.insert(0, repo_root)

from mkv_domain import parse_episode, detect_sub_lang, safe_filename, is_dialogue_track, natural_sort_key
from mkv_remux_job import _COMMENTARY_RE
from mkv_scan import _CHAPTER_TAG_RE
from mkv_assets import _COVER_FILE_RE

# Read raw UTF-8 bytes directly to avoid Windows console/pipe code page mangling
payload = json.loads(sys.stdin.buffer.read().decode("utf-8"))

ep_results = []
for name in payload["episodes"]:
    res = parse_episode(name)
    if res is None:
        ep_results.append(None)
    else:
        ep_results.append({
            "season": res.season,
            "ep_type": res.ep_type,
            "ep": res.ep,
        })

sub_results = [detect_sub_lang(s) for s in payload["sub_stems"]]
safe_results = [safe_filename(n) for n in payload["safe_names"]]
track_results = [is_dialogue_track(t) for t in payload["track_names"]]
commentary_results = [bool(_COMMENTARY_RE.search(c.lower())) for c in payload["commentary_names"]]

# Natural sort order
sorted_outputs = sorted(payload["sort_inputs"], key=natural_sort_key)

# Chapter XML sniffing
chapter_results = [bool(_CHAPTER_TAG_RE.search(s.encode("utf-8"))) for s in payload["chapter_xml_samples"]]

# Cover file matching
cover_results = []
for stem in payload["cover_stems"]:
    m = _COVER_FILE_RE.match(stem)
    cover_results.append(m.group("stem") if m else None)

# Bitrate parsing
bitrate_re = re.compile(r"^(\d+)([kM]?)$")
bitrate_results = []
for b in payload["bitrate_strings"]:
    m = bitrate_re.match(b)
    if m:
        num = int(m.group(1))
        unit = m.group(2)
        bitrate_results.append([num, unit] if num > 0 else None)
    else:
        bitrate_results.append(None)

response = {
    "episodes": ep_results,
    "sub_stems": sub_results,
    "safe_names": safe_results,
    "track_names": track_results,
    "commentary_names": commentary_results,
    "sorted_outputs": sorted_outputs,
    "chapter_xml_results": chapter_results,
    "cover_stem_results": cover_results,
    "bitrate_results": bitrate_results,
}
sys.stdout.buffer.write(json.dumps(response).encode("utf-8"))
sys.stdout.buffer.flush()
"#;

    let (bin, args) = if Command::new("uv").arg("--version").output().is_ok() {
        ("uv", vec!["run", "python", "-c", script])
    } else {
        let python = if Command::new("python3").arg("--version").output().is_ok() {
            "python3"
        } else {
            "python"
        };
        (python, vec!["-c", script])
    };

    let mut child = Command::new(bin)
        .args(&args)
        .env("REPO_ROOT", repo_root)
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .current_dir(repo_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn python process");

    use std::io::Write;
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(json_input.as_bytes())
        .expect("write payload");

    let output = child.wait_with_output().expect("wait uv python");
    if !output.status.success() {
        panic!(
            "Python process failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout).expect("deserialize python response")
}

#[test]
fn test_cross_language_regex_and_domain() {
    let payload = CrossCheckPayload {
        episodes: vec![
            // Basic dashes
            "[Group] Show - 01 [1080p]".to_string(),
            "Show - 12".to_string(),
            "Show - 100".to_string(),
            "Show  -  05".to_string(),
            "Show - 01v2".to_string(),
            "Show - 01-02".to_string(),
            "Show - 01 - 02".to_string(),
            "Show - 001".to_string(),
            "Show - 0005".to_string(),
            // Seasons & SxxExx
            "Show S02E10".to_string(),
            "Show S01E03".to_string(),
            "Show S03E125".to_string(),
            "Show S01E05v2".to_string(),
            "Show S1E1".to_string(),
            "Show s01e05".to_string(),
            "Show S01E01-E02".to_string(),
            "Show S01P01".to_string(),
            "Show S02P05v2".to_string(),
            "Show Season 2 - 01".to_string(),
            "Show season 2 - 01".to_string(),
            "Show SEASON 2 - 01".to_string(),
            "Show Season 01 - 05".to_string(),
            "Show S2 - 05".to_string(),
            "Show S02 - 05".to_string(),
            "Show 2 Part - 01".to_string(),
            // Keywords
            "Show EP03".to_string(),
            "Show ep03".to_string(),
            "Show Ep. 03".to_string(),
            "Show Episode 07".to_string(),
            "Show episode 07".to_string(),
            "Show EPISODE 07".to_string(),
            // Brackets & Parens
            "Show [07] [720p]".to_string(),
            "Show (13)".to_string(),
            // Decimals
            "Show - 12.5".to_string(),
            "Show - 0.5".to_string(),
            "Show - 1.25".to_string(),
            "Show - 00.5".to_string(),
            "Show EP 12.5".to_string(),
            "Show - 1.5".to_string(),
            "Show - 01.5".to_string(),
            // Typed (OVA, Special)
            "Show OVA 3".to_string(),
            "Show OVA3".to_string(),
            "Show ova 3".to_string(),
            "Show OAD 01".to_string(),
            "Show ONA 01".to_string(),
            "Show SP01".to_string(),
            "Show SP 01".to_string(),
            "Show Special 02".to_string(),
            "Show Specials 02".to_string(),
            "Show SPECIAL 02".to_string(),
            "Show OVA".to_string(),
            "Show Special".to_string(),
            // CJK / Unicode
            "[Group] 葬送のフリーレン - 05 [1080p]".to_string(),
            "【推しの子】 第01話 (1080p)".to_string(),
            "SPY×FAMILY - 12 [1080p]".to_string(),
            "第12.5話".to_string(),
            "第03話".to_string(),
            "第 04 話".to_string(),
            "第100話".to_string(),
            // Year guard & edge numbers
            "Frieren - 2023 - 05".to_string(),
            "One Piece - 1999 - 12".to_string(),
            "Demon Lord 2099 - 01".to_string(),
            "Demon Lord 2099 - 2024 - 01".to_string(),
            "Show - 1234".to_string(),
            "Show - 2150".to_string(),
            "Show - EP2023".to_string(),
            "One Piece - 1089 [1080p]".to_string(),
            "Detective Conan - 1100".to_string(),
            "Mob Psycho 100 II - 03".to_string(),
            "Show 4k 60fps - 02".to_string(),
            "Show (2020) - 01".to_string(),
            "Show [2021] - 02".to_string(),
            "Show 2022 - 03".to_string(),
            "Tokyo 2040 - 01".to_string(),
            "2001 Nights - 01".to_string(),
            // End markers
            "Show - 12 END".to_string(),
            "Show - 12 FIN".to_string(),
            "Show - 12 FINAL".to_string(),
            "Show - 24 (End)".to_string(),
            "Show - 24 [END]".to_string(),
            // Delimiters
            "Show_-_01".to_string(),
            "Show.01.1080p.mkv".to_string(),
            "Show..01.mkv".to_string(),
            "...Show...01...".to_string(),
            "Dr. Stone - 01".to_string(),
            "Fate.Zero - 01".to_string(),
            "Steins.Gate.01.mkv".to_string(),
            // Resolution & Codec guards
            "Show 720p - 01".to_string(),
            "Show 1080p - 01".to_string(),
            "Show 2160p - 01".to_string(),
            "Show 480p - 01".to_string(),
            "Show 10bit - 01".to_string(),
            "Show FLAC - 01".to_string(),
            "Show AAC 2.0 - 01".to_string(),
            "Show x264 - 01".to_string(),
            "Show x265 - 01".to_string(),
            "Show HEVC - 01".to_string(),
            // Subtitle offsets
            "Show - 01.[+5s]".to_string(),
            "Show - 02.[-3s]".to_string(),
            "Show - 03.[+1.5s]".to_string(),
            "Show - 04.[-0.25s]".to_string(),
            // Non-episodes
            "Random Movie Title".to_string(),
            "Movie (2020)".to_string(),
            "".to_string(),
            "Trailer".to_string(),
            "OST".to_string(),
            "Show Complete Series".to_string(),
            "Show Batch".to_string(),
            "Show Season 1".to_string(),
            "Show Movie".to_string(),
            "Show NCED".to_string(),
            "Show NCOP".to_string(),
            "Show Menu".to_string(),
            "Show PV".to_string(),
        ],
        sub_stems: vec![
            "EP01[ID]".to_string(),
            "EP01[id]".to_string(),
            "EP01[Ind]".to_string(),
            "EP01[Indonesian]".to_string(),
            "Show - 01 [Indo]".to_string(),
            "Show - 01 [indo]".to_string(),
            "EP01[INDONESIAN]".to_string(),
            "EP01[English]".to_string(),
            "EP01[ENG]".to_string(),
            "EP01[en]".to_string(),
            "EP01[eng]".to_string(),
            "EP01[Japanese]".to_string(),
            "EP01[ja]".to_string(),
            "EP01[jpn]".to_string(),
            "Show_01_ind_track2".to_string(),
            "Show_01_track2.ind".to_string(),
            "Show_01_tso_track2".to_string(),
            "Show_01_track2.ts".to_string(),
            "Show - 01.ind".to_string(),
            "Show - 01_eng".to_string(),
            "Show - 01 (ind)".to_string(),
            "Show - 01".to_string(),
            "Show - 01.xyz".to_string(),
            "episode_01.ind.[+5s]".to_string(),
            "episode_01.eng.[-3s]".to_string(),
            "episode_01.jpn.[+2.5s]".to_string(),
            "episode_01.[+5s].ind".to_string(),
        ],
        safe_names: vec![
            "foo/bar".to_string(),
            "foo:bar".to_string(),
            "foo*bar".to_string(),
            "foo?bar".to_string(),
            "foo\"bar".to_string(),
            "foo<bar".to_string(),
            "foo>bar".to_string(),
            "foo|bar".to_string(),
            "foo\\bar".to_string(),
            "  hello world  ".to_string(),
            "/:*?\"<>|\\".to_string(),
            "".to_string(),
            "   ".to_string(),
            ".hidden".to_string(),
            "..".to_string(),
            ".".to_string(),
            "CON".to_string(),
            "prn.txt".to_string(),
            "aux".to_string(),
            "NUL.tar.gz".to_string(),
            "normal_name-123".to_string(),
        ],
        track_names: vec![
            "Dialogue".to_string(),
            "dialogue".to_string(),
            "DIALOGUE".to_string(),
            "Main Audio".to_string(),
            "Full Subtitles".to_string(),
            "Sign & Song".to_string(),
            "Signs/Songs".to_string(),
            "Signs".to_string(),
            "Songs".to_string(),
            "Complete".to_string(),
            "Indonesian Dialogue".to_string(),
            "Indo Sub".to_string(),
            "English".to_string(),
            "Commentary".to_string(),
            "Audio Description".to_string(),
        ],
        commentary_names: vec![
            "Audio Commentary".to_string(),
            "Director's Comment".to_string(),
            "audio description".to_string(),
            "ad".to_string(),
            "extension".to_string(),
            "broadcast".to_string(),
            "main".to_string(),
            "Japanese Stereo".to_string(),
            "broadcasting".to_string(),
            "addition".to_string(),
        ],
        sort_inputs: vec![
            "EP10".to_string(),
            "EP2".to_string(),
            "EP1".to_string(),
            "Show - 01.mkv".to_string(),
            "Show - 1.mkv".to_string(),
            "Show - 2.mkv".to_string(),
            "Show - 10.mkv".to_string(),
            "Show - 1.5.mkv".to_string(),
            "Show - 01v2.mkv".to_string(),
            "Show - 01v1.mkv".to_string(),
            "Episode 100".to_string(),
            "Episode 99".to_string(),
            "Episode 02".to_string(),
            "Season 1/EP1".to_string(),
            "Season 10/EP1".to_string(),
            "Season 2/EP1".to_string(),
        ],
        chapter_xml_samples: vec![
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Chapters>\n  <EditionEntry>\n"
                .to_string(),
            "<chapters>".to_string(),
            "<Chapters >".to_string(),
            "<CHAPTERS>".to_string(),
            "<Chapters xmlns=\"http://www.matroska.org\">".to_string(),
            "<ChapterAtom>".to_string(),
            "<other xml>".to_string(),
            "no xml at all".to_string(),
            "".to_string(),
        ],
        cover_stems: vec![
            "Show - 01_cover".to_string(),
            "Show S01E02_cover".to_string(),
            "Movie_cover".to_string(),
            "cover".to_string(),
            "COVER".to_string(),
            "not_a_cov".to_string(),
            "just_a_stem".to_string(),
        ],
        bitrate_strings: vec![
            "128k".to_string(),
            "192k".to_string(),
            "1M".to_string(),
            "320".to_string(),
            "0k".to_string(),
            "0".to_string(),
            "128kbps".to_string(),
            "-128k".to_string(),
            "128K".to_string(),
            "abc".to_string(),
            "".to_string(),
        ],
    };

    let py_response = query_python(&payload);

    // 1. Verify episode parsing
    let mut mismatches = Vec::new();
    for (i, name) in payload.episodes.iter().enumerate() {
        let rust_res = parse_episode(name).map(ParsedResult::from);
        let py_res = &py_response.episodes[i];
        if rust_res != *py_res {
            mismatches.push(format!(
                "Filename: '{name}'\n  Rust:   {:?}\n  Python: {:?}",
                rust_res, py_res
            ));
        }
    }

    // 2. Verify sub language detection
    for (i, stem) in payload.sub_stems.iter().enumerate() {
        let rust_res = detect_sub_lang(stem);
        let py_res = &py_response.sub_stems[i];
        if rust_res != *py_res {
            mismatches.push(format!(
                "Sub stem: '{stem}'\n  Rust:   {:?}\n  Python: {:?}",
                rust_res, py_res
            ));
        }
    }

    // 3. Verify safe filename
    for (i, name) in payload.safe_names.iter().enumerate() {
        let rust_res = safe_filename(name);
        let py_res = &py_response.safe_names[i];
        if rust_res != *py_res {
            mismatches.push(format!(
                "Safe filename: '{name}'\n  Rust:   {:?}\n  Python: {:?}",
                rust_res, py_res
            ));
        }
    }

    // 4. Verify is_dialogue_track
    for (i, name) in payload.track_names.iter().enumerate() {
        let rust_res = is_dialogue_track(Some(name));
        let py_res = py_response.track_names[i];
        if rust_res != py_res {
            mismatches.push(format!(
                "Track name: '{name}'\n  Rust:   {:?}\n  Python: {:?}",
                rust_res, py_res
            ));
        }
    }

    // 5. Verify commentary detection
    for (i, name) in payload.commentary_names.iter().enumerate() {
        let rust_res = COMMENTARY_RE.is_match(name);
        let py_res = py_response.commentary_names[i];
        if rust_res != py_res {
            mismatches.push(format!(
                "Commentary name: '{name}'\n  Rust:   {:?}\n  Python: {:?}",
                rust_res, py_res
            ));
        }
    }

    // 6. Verify natural sort ordering
    let mut rust_sorted = payload.sort_inputs.clone();
    rust_sorted.sort_by_key(|s| natural_sort_key(s));
    if rust_sorted != py_response.sorted_outputs {
        mismatches.push(format!(
            "Natural sort mismatch:\n  Rust:   {:?}\n  Python: {:?}",
            rust_sorted, py_response.sorted_outputs
        ));
    }

    // 7. Verify chapter XML sniffing
    for (i, sample) in payload.chapter_xml_samples.iter().enumerate() {
        let rust_res = CHAPTER_TAG_RE.is_match(sample.as_bytes());
        let py_res = py_response.chapter_xml_results[i];
        if rust_res != py_res {
            mismatches.push(format!(
                "Chapter XML sample: '{sample}'\n  Rust:   {:?}\n  Python: {:?}",
                rust_res, py_res
            ));
        }
    }

    // 8. Verify cover file pattern matching
    for (i, stem) in payload.cover_stems.iter().enumerate() {
        let rust_res = COVER_FILE_RE
            .captures(stem)
            .and_then(|c| c.name("stem").map(|m| m.as_str().to_string()));
        let py_res = &py_response.cover_stem_results[i];
        if rust_res != *py_res {
            mismatches.push(format!(
                "Cover stem: '{stem}'\n  Rust:   {:?}\n  Python: {:?}",
                rust_res, py_res
            ));
        }
    }

    // 9. Verify bitrate regex validation
    for (i, b) in payload.bitrate_strings.iter().enumerate() {
        let rust_res = BITRATE_RE.captures(b).and_then(|caps| {
            let num: u64 = caps.get(1).map_or("", |m| m.as_str()).parse().unwrap_or(0);
            if num == 0 {
                None
            } else {
                let unit = caps.get(2).map_or("", |m| m.as_str()).to_string();
                Some((num, unit))
            }
        });
        let py_res = py_response.bitrate_results[i]
            .as_ref()
            .map(|(n, u)| (*n, u.clone()));
        if rust_res != py_res {
            mismatches.push(format!(
                "Bitrate string: '{b}'\n  Rust:   {:?}\n  Python: {:?}",
                rust_res, py_res
            ));
        }
    }

    if !mismatches.is_empty() {
        panic!(
            "Cross-language regex/domain mismatches found ({}):\n{}",
            mismatches.len(),
            mismatches.join("\n\n")
        );
    }
}
