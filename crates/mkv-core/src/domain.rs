//! Domain logic for episode identity, track scoring, language mapping, and fansub heuristics.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde::{Deserialize, Serialize};

// ─── DOMAIN TYPES ────────────────────────────────────────────────────────────

/// Canonical parsed identity of an episode: season tag, type key, number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum EpisodeType {
    #[default]
    Episode,
    Part,
    Ova,
    Special,
}

impl EpisodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Episode => "E",
            Self::Part => "P",
            Self::Ova => "OVA",
            Self::Special => "SP",
        }
    }

    pub fn label(&self, long: bool) -> &'static str {
        match (self, long) {
            (Self::Episode, false) => "EP",
            (Self::Episode, true) => "Episode",
            (Self::Part, _) => "Part",
            (Self::Ova, _) => "OVA",
            (Self::Special, false) => "SP",
            (Self::Special, true) => "Special",
        }
    }

    pub fn is_special_or_ova(&self) -> bool {
        matches!(self, Self::Ova | Self::Special)
    }
}

impl std::fmt::Display for EpisodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<&str> for EpisodeType {
    fn from(s: &str) -> Self {
        match s {
            "OVA" | "ova" => Self::Ova,
            "SP" | "sp" | "Special" | "special" => Self::Special,
            "P" | "p" | "Part" | "part" => Self::Part,
            _ => Self::Episode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EpisodeInfo {
    pub season: String,
    pub ep_type: EpisodeType,
    pub ep: String,
}

impl EpisodeInfo {
    pub fn new(
        season: impl Into<String>,
        ep_type: impl Into<EpisodeType>,
        ep: impl Into<String>,
    ) -> Self {
        Self {
            season: season.into(),
            ep_type: ep_type.into(),
            ep: ep.into(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemOutcome {
    Completed,
    Skipped,
    Error,
    Cancelled,
    Planned,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackProperties {
    #[serde(default)]
    pub codec_id: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub track_name: Option<String>,
    #[serde(default)]
    pub audio_channels: Option<u32>,
    #[serde(default)]
    pub pixel_dimensions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MkvTrack {
    #[serde(default)]
    pub id: Option<u32>,
    #[serde(rename = "type")]
    pub track_type: String,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub properties: TrackProperties,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioTrack {
    pub id: u32,
    pub pos: u32,
    pub audio_idx: usize,
    pub lang: String,
    pub channels: u32,
    pub name: String,
    pub codec: String,
}

// ─── NATURAL SORTING ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum NaturalSortToken {
    Num(u64),
    Str(String),
}

static NATURAL_SORT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)").unwrap());

/// Key function for sorting paths naturally (e.g. EP2 before EP10).
pub fn natural_sort_key(text: &str) -> Vec<NaturalSortToken> {
    let mut tokens = Vec::new();
    let mut last_idx = 0;

    for mat in NATURAL_SORT_RE.find_iter(text) {
        if mat.start() > last_idx {
            let part = &text[last_idx..mat.start()];
            tokens.push(NaturalSortToken::Str(part.to_lowercase()));
        }
        if let Ok(num) = mat.as_str().parse::<u64>() {
            tokens.push(NaturalSortToken::Num(num));
        } else {
            tokens.push(NaturalSortToken::Str(mat.as_str().to_lowercase()));
        }
        last_idx = mat.end();
    }

    if last_idx < text.len() {
        tokens.push(NaturalSortToken::Str(text[last_idx..].to_lowercase()));
    }

    tokens
}

// ─── LANGUAGE AND MIME CONFIG ────────────────────────────────────────────────

pub const SUBTITLE_LANGS_PRIMARY_ORDER: &[&str] = &["ind", "id", "und"];
pub const SUBTITLE_LANGS_FALLBACK_ORDER: &[&str] = &["eng", "en"];

pub static DIALOGUE_KEYWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "full",
        "dialogue",
        "dialog",
        "main",
        "complete",
        "indo",
        "indonesia",
        "indonesian",
    ]
    .into_iter()
    .collect()
});

pub static KNOWN_LANG_TOKENS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "ind", "id", "eng", "en", "jpn", "ja", "und", "zho", "zh", "kor", "ko", "ara", "ar", "por",
        "pt", "spa", "es", "fra", "fr", "deu", "de", "ita", "it", "rus", "ru", "tha", "th", "vie",
        "vi", "msa", "ms",
    ]
    .into_iter()
    .collect()
});

pub const SUBTITLE_EXTENSIONS: &[&str] = &[".ass", ".srt"];
pub const CHAPTER_EXT: &str = ".xml";
pub const COVER_EXTENSIONS: &[&str] = &[".jpg", ".jpeg", ".png", ".webp", ".gif", ".avif"];
pub const FONT_EXTENSIONS: &[&str] = &[".ttf", ".ttc", ".otf", ".woff", ".woff2", ".eot"];

pub fn cover_mime(ext: &str) -> Option<&'static str> {
    match ext.to_lowercase().as_str() {
        ".jpg" | ".jpeg" => Some("image/jpeg"),
        ".png" => Some("image/png"),
        ".webp" => Some("image/webp"),
        ".gif" => Some("image/gif"),
        ".avif" => Some("image/avif"),
        _ => None,
    }
}

pub fn font_mime(ext: &str) -> Option<&'static str> {
    match ext.to_lowercase().as_str() {
        ".ttf" | ".ttc" => Some("application/x-truetype-font"),
        ".otf" => Some("application/vnd.ms-opentype"),
        ".woff" => Some("application/font-woff"),
        ".woff2" => Some("font/woff2"),
        ".eot" => Some("application/vnd.ms-fontobject"),
        _ => None,
    }
}

pub fn lang_label(code: &str) -> &str {
    match code {
        "ind" | "id" => "Indonesian",
        "und" => "Undetermined",
        "eng" | "en" => "English",
        "jpn" | "ja" => "Japanese",
        _ => code,
    }
}

pub fn lang_mkv_tag(code: &str) -> &str {
    match code {
        "id" => "ind",
        "en" => "eng",
        "ja" => "jpn",
        _ => code,
    }
}

// ─── SUBTITLE SCORING TABLES ─────────────────────────────────────────────────

pub fn score_subtitle_category(language: &str, is_ass: bool) -> i32 {
    if is_ass {
        match language {
            "ind" => 12,
            "id" => 11,
            "und" => 10,
            "eng" => 9,
            "en" => 8,
            _ => 7,
        }
    } else {
        match language {
            "ind" => 6,
            "id" => 5,
            "und" => 4,
            "eng" => 3,
            "en" => 2,
            _ => 1,
        }
    }
}

// ─── NAMING & PATH HELPERS ───────────────────────────────────────────────────

static UNSAFE_FN_CHARS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[/:*?"<>|\\]"#).unwrap());

pub fn safe_filename(name: &str) -> String {
    let file_name = Path::new(name)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sanitized = UNSAFE_FN_CHARS.replace_all(&file_name, "_").into_owned();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "unnamed".to_string()
    } else {
        sanitized
    }
}

pub fn output_stem(base: &str, ep_type: &EpisodeType, ep: &str) -> String {
    if ep_type.is_special_or_ova() {
        format!("{}{ep}_{base}", ep_type.as_str())
    } else {
        base.to_string()
    }
}

pub fn resolve_output_dir(
    mkv_path: &Path,
    input_dir: &Path,
    out_override: Option<&Path>,
) -> PathBuf {
    let parent = mkv_path.parent().unwrap_or_else(|| Path::new("."));
    let Some(out_override) = out_override else {
        return parent.to_path_buf();
    };
    match parent.strip_prefix(input_dir) {
        Ok(rel) => out_override.join(rel),
        Err(_) => out_override.to_path_buf(),
    }
}

pub fn seasons_match(mkv_season: &str, sub_season: &str) -> bool {
    if mkv_season.is_empty() || sub_season.is_empty() {
        true
    } else {
        mkv_season == sub_season
    }
}

pub fn ancestor_dirs(mkv_dir: &Path, root_input_dir: &Path) -> Vec<PathBuf> {
    let Ok(rel) = mkv_dir.strip_prefix(root_input_dir) else {
        return vec![mkv_dir.to_path_buf()];
    };

    let mut result = vec![root_input_dir.to_path_buf()];
    let mut current = root_input_dir.to_path_buf();
    for component in rel.components() {
        current.push(component);
        result.push(current.clone());
    }
    result
}

// ─── SUBTITLE LANGUAGE DETECTION ─────────────────────────────────────────────

static OFFSET_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.\[[-+]?\d+(?:\.\d+)?s\]$").unwrap());

static BRACKET_LANG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[(?:(ID|Ind|Indonesian|Indo)|(EN|ENG|English)|(JA|JPN|Japanese))\]").unwrap()
});

static LANG_BEFORE_TRACK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[_.]([a-zA-Z]{2,3})_track\d+").unwrap());

static LANG_AFTER_TRACK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"track\d+[_.]([a-zA-Z]{2,3})(?:$|[^a-zA-Z])").unwrap());

static LANG_PAREN_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(([a-zA-Z]{2,3})\)$").unwrap());

static LANG_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[._]([a-zA-Z]{2,3})$").unwrap());

pub fn detect_sub_lang(sub_stem: &str) -> String {
    let cleaned = OFFSET_TAG_RE.replace(sub_stem, "");

    if let Some(caps) = BRACKET_LANG_RE.captures(&cleaned) {
        if caps.get(1).is_some() {
            return "ind".to_string();
        }
        if caps.get(2).is_some() {
            return "eng".to_string();
        }
        if caps.get(3).is_some() {
            return "jpn".to_string();
        }
    }

    for (re, require_known) in [
        (&*LANG_BEFORE_TRACK_RE, false),
        (&*LANG_AFTER_TRACK_RE, false),
        (&*LANG_PAREN_SUFFIX_RE, false),
        (&*LANG_SUFFIX_RE, true),
    ] {
        if let Some(caps) = re.captures(&cleaned) {
            if let Some(mat) = caps.get(1) {
                let token = mat.as_str().to_lowercase();
                if !require_known || KNOWN_LANG_TOKENS.contains(token.as_str()) {
                    return token;
                }
            }
        }
    }

    "und".to_string()
}

// ─── EPISODE PARSER ──────────────────────────────────────────────────────────

static OVA_PATTERN: LazyLock<FancyRegex> =
    LazyLock::new(|| FancyRegex::new(r"(?i)(?<!\w)(OVA|OAD|ONA)(?=\d|\W|$)").unwrap());

static SPECIAL_PATTERN: LazyLock<FancyRegex> =
    LazyLock::new(|| FancyRegex::new(r"(?i)(?<!\w)(Specials?|SP)(?=\d|\W|$)").unwrap());

static SXXEXX_PATTERN: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(
        r"[Ss](\d{1,2})[Ee](\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:-[Ee]?\d{1,4})?(?:[vV]\d)?",
    )
    .unwrap()
});

static SXXPXX_PATTERN: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(
        r"[Ss](\d{1,2})[Pp](\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:-[Pp]?\d{1,4})?(?:[vV]\d)?",
    )
    .unwrap()
});

const YEAR_GUARD: &str = r"(?!(?:19|20)\d{2}(?!\d))";

static EP_NUM_MARKER_RE: LazyLock<FancyRegex> = LazyLock::new(|| {
    let pattern = format!(
        r"(?i)(?:[-\u2013]\s*){YEAR_GUARD}(\d{{1,4}}(?:\.\d{{1,2}}(?![0-9pib]))?)(?:-\d{{1,4}})?(?![0-9pib])(?:\s*(?:end|fin|final))?(?:[vV]\d)?|[\[(](\d{{1,3}}(?:\.\d{{1,2}}(?![0-9pib]))?)[\])](?:[vV]\d)?|\b[Ee][Pp](?:isode)?\.?\s*(\d{{1,4}}(?:\.\d{{1,2}}(?![0-9pib]))?)(?:[vV]\d)?\b|第\s*(\d{{1,4}}(?:\.\d{{1,2}})?)\s*話?"
    );
    FancyRegex::new(&pattern).unwrap()
});

static EP_NUM_FALLBACK_RE: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(
        r"(?i)(?:^|\s)(\d{1,3}(?:\.\d{1,2}(?![0-9pib]))?)(?![0-9pib])(?:\s*(?:end|fin|final))?(?:[vV]\d)?\b",
    )
    .unwrap()
});

static SEASON_PATTERNS: LazyLock<Vec<FancyRegex>> = LazyLock::new(|| {
    vec![
        FancyRegex::new(r"(?i)[Ss]eason\s*(\d{1,2})").unwrap(),
        FancyRegex::new(r"\bS(\d{1,2})\b(?![EePp])").unwrap(),
        FancyRegex::new(r"(?<=[a-zA-Z])\s+(\d{1})\s+(?=[a-zA-Z])").unwrap(),
    ]
});

static TYPED_EP_GLUED: LazyLock<FancyRegex> =
    LazyLock::new(|| FancyRegex::new(r"^\s*(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)").unwrap());

pub fn format_ep(val: &str) -> String {
    if let Some((int_part, frac_part)) = val.split_once('.') {
        let int_val: u32 = int_part.parse().unwrap_or(0);
        format!("{:02}.{}", int_val, frac_part)
    } else {
        let int_val: u32 = val.parse().unwrap_or(0);
        format!("{:02}", int_val)
    }
}

fn first_captured_group<'a>(caps: &'a fancy_regex::Captures<'a, str>) -> Option<&'a str> {
    for i in 1..caps.len() {
        if let Some(m) = caps.get(i) {
            return Some(m.as_str());
        }
    }
    None
}

fn last_fallback_ep(text: &str) -> Option<&str> {
    EP_NUM_FALLBACK_RE
        .captures_iter(text)
        .flatten()
        .filter_map(|m| m.get(1).map(|v| v.as_str()))
        .last()
}

fn parse_typed_ep(name: &str, pattern: &FancyRegex) -> String {
    let Ok(Some(m)) = pattern.find(name) else {
        return "00".to_string();
    };

    let remainder = &name[m.end()..];

    if let Ok(Some(caps)) = TYPED_EP_GLUED.captures(remainder) {
        if let Some(val) = caps.get(1) {
            return format_ep(val.as_str());
        }
    }

    if let Ok(Some(caps)) = EP_NUM_MARKER_RE.captures(remainder) {
        if let Some(val) = first_captured_group(&caps) {
            return format_ep(val);
        }
    }

    if let Some(val) = last_fallback_ep(remainder) {
        return format_ep(val);
    }

    "00".to_string()
}

pub fn parse_episode(name: &str) -> Option<EpisodeInfo> {
    let without_offset = OFFSET_TAG_RE.replace(name, "");
    let mut normalized = without_offset.replace('_', " ");

    // Replace non-decimal '.' with space
    let mut chars: Vec<char> = normalized.chars().collect();
    for i in 0..chars.len() {
        if chars[i] == '.' {
            let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
            let next_digit = i + 1 < chars.len() && chars[i + 1].is_ascii_digit();
            if !(prev_digit && next_digit) {
                chars[i] = ' ';
            }
        }
    }
    normalized = chars.into_iter().collect();

    let mut season = String::new();
    for pat in SEASON_PATTERNS.iter() {
        if let Ok(Some(caps)) = pat.captures(&normalized) {
            if let Some(m) = caps.get(1) {
                if let Ok(s_num) = m.as_str().parse::<u32>() {
                    season = s_num.to_string();
                    let match_range = caps.get(0).unwrap().range();
                    normalized = format!(
                        "{}{}",
                        &normalized[..match_range.start],
                        &normalized[match_range.end..]
                    );
                    break;
                }
            }
        }
    }

    if let Ok(Some(_)) = OVA_PATTERN.find(&normalized) {
        let ep = parse_typed_ep(&normalized, &OVA_PATTERN);
        return Some(EpisodeInfo::new(season, EpisodeType::Ova, ep));
    }

    if let Ok(Some(_)) = SPECIAL_PATTERN.find(&normalized) {
        let ep = parse_typed_ep(&normalized, &SPECIAL_PATTERN);
        return Some(EpisodeInfo::new(season, EpisodeType::Special, ep));
    }

    if let Ok(Some(caps)) = SXXEXX_PATTERN.captures(&normalized) {
        let s_num: u32 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
        let ep = format_ep(caps.get(2).unwrap().as_str());
        return Some(EpisodeInfo::new(
            s_num.to_string(),
            EpisodeType::Episode,
            ep,
        ));
    }

    if let Ok(Some(caps)) = SXXPXX_PATTERN.captures(&normalized) {
        let s_num: u32 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
        let ep = format_ep(caps.get(2).unwrap().as_str());
        return Some(EpisodeInfo::new(s_num.to_string(), EpisodeType::Part, ep));
    }

    if let Ok(Some(caps)) = EP_NUM_MARKER_RE.captures(&normalized[..]) {
        if let Some(val) = first_captured_group(&caps) {
            return Some(EpisodeInfo::new(
                season,
                EpisodeType::Episode,
                format_ep(val),
            ));
        }
    }

    if let Some(val) = last_fallback_ep(&normalized) {
        return Some(EpisodeInfo::new(
            season,
            EpisodeType::Episode,
            format_ep(val),
        ));
    }

    None
}

pub fn ep_type_label(ep_type: &EpisodeType, long: bool) -> &str {
    ep_type.label(long)
}

// ─── TRACK SELECTION & HEURISTICS ────────────────────────────────────────────

static SPLIT_WORDS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s\-_]+").unwrap());

pub fn is_dialogue_track(name: Option<&str>) -> bool {
    let Some(name) = name else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    let lower = name.to_lowercase();
    for token in SPLIT_WORDS_RE.split(&lower) {
        if DIALOGUE_KEYWORDS.contains(token) {
            return true;
        }
    }
    false
}

pub fn select_best_subtitle_track(
    tracks: &[MkvTrack],
    codec_filter: Option<&HashSet<String>>,
) -> Option<(u32, String, bool)> {
    let mut candidates = Vec::new();

    for t in tracks {
        if t.track_type != "subtitles" {
            continue;
        }
        let Some(tid) = t.id else {
            continue;
        };

        let codec_id = t
            .properties
            .codec_id
            .as_deref()
            .unwrap_or("")
            .to_lowercase();
        let language = t.properties.language.as_deref().unwrap_or("und");
        let name = t.properties.track_name.as_deref().unwrap_or("");

        if let Some(filter) = codec_filter {
            if !filter.contains(&codec_id) {
                continue;
            }
        }

        let is_diag = is_dialogue_track(Some(name));
        let is_ass = codec_id == "s_text/ass";
        let category_score = score_subtitle_category(language, is_ass);

        let priority_key = (category_score, if is_diag { 1 } else { 0 }, -(tid as i64));
        candidates.push((priority_key, tid, language.to_string(), is_diag));
    }

    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by_key(|c| c.0);
    let (_, tid, language, is_diag) = candidates.pop().unwrap();
    Some((tid, language, is_diag))
}

static COMMENTARY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(commentary|comment|audio\s+description|ad|extension)\b").unwrap()
});

pub fn select_audio_track(
    audio_tracks: &[AudioTrack],
    video_pos: u32,
) -> (Option<AudioTrack>, String) {
    if audio_tracks.is_empty() {
        return (None, String::new());
    }

    let primary_tracks: Vec<AudioTrack> = audio_tracks
        .iter()
        .filter(|t| !COMMENTARY_RE.is_match(&t.name))
        .cloned()
        .collect();

    let (pool_to_use, is_fallback_pool) = if !primary_tracks.is_empty() {
        (primary_tracks, false)
    } else {
        (audio_tracks.to_vec(), true)
    };

    let mut candidates: Vec<((i32, i64), AudioTrack)> = Vec::new();

    for track in pool_to_use {
        let is_ja = track.lang.starts_with("ja") || track.lang.starts_with("jpn");
        let is_und = track.lang == "und";
        let is_stereo = track.channels == 2;
        let is_diag = is_dialogue_track(Some(&track.name));

        let mut score = 0;
        if is_ja && is_stereo && is_diag {
            score = 10;
        } else if is_ja && is_stereo {
            score = 9;
        } else if is_ja {
            score = 8;
        } else if is_und && is_stereo && is_diag {
            score = 7;
        } else if is_und && is_stereo {
            score = 6;
        } else if is_und {
            score = 5;
        } else if is_stereo {
            score = 4;
        } else if track.pos > video_pos {
            score = 3;
        }

        if score == 0 {
            score = if is_ja { 2 } else { 1 };
        }

        let priority_key = (score, -(track.pos as i64));
        candidates.push((priority_key, track));
    }

    candidates.sort_by_key(|c| c.0);
    let (priority_key, best_track) = candidates.pop().unwrap();
    let score = priority_key.0;

    if is_fallback_pool {
        if best_track.lang.starts_with("ja") || best_track.lang.starts_with("jpn") {
            return (
                Some(best_track),
                "Japanese audio fallback (commentary-possible)".to_string(),
            );
        }
        return (Some(best_track), "First audio (last resort)".to_string());
    }

    let reason = match score {
        10 => "Japanese stereo (dialogue-priority)",
        9 => "Japanese stereo",
        8 => "Japanese multi-channel (no stereo available)",
        7 => "Undetermined stereo (dialogue-priority, likely Japanese)",
        6 => "Undetermined stereo (likely Japanese)",
        5 => "Undetermined multi-channel (likely Japanese)",
        4 => "Stereo fallback (no Japanese audio)",
        3 => "First audio after video",
        2 => "Japanese audio fallback (commentary-possible)",
        1 => "First audio (last resort)",
        _ => "Selected audio",
    };

    (Some(best_track), reason.to_string())
}
