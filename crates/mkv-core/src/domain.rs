//! Domain logic for episode identity, track scoring, language mapping, and fansub heuristics.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde::{Deserialize, Serialize};

// ─── DOMAIN TYPES ────────────────────────────────────────────────────────────

/// Kind of numbered release an episode belongs to; `as_str` is its identity key.
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

    pub fn label(&self) -> &'static str {
        match self {
            Self::Episode => "EP",
            Self::Part => "Part",
            Self::Ova => "OVA",
            Self::Special => "SP",
        }
    }

    fn is_special_or_ova(&self) -> bool {
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

/// Canonical parsed identity of an episode: season tag, type key, number.
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

const DIALOGUE_KEYWORDS: &[&str] = &[
    "full",
    "dialogue",
    "dialog",
    "main",
    "complete",
    "indo",
    "indonesia",
    "indonesian",
];

const KNOWN_LANG_TOKENS: &[&str] = &[
    "ind", "id", "eng", "en", "jpn", "ja", "und", "zho", "zh", "kor", "ko", "ara", "ar", "por",
    "pt", "spa", "es", "fra", "fr", "deu", "de", "ita", "it", "rus", "ru", "tha", "th", "vie",
    "vi", "msa", "ms",
];

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

const WINDOWS_RESERVED_DEVICES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn is_windows_reserved_device(stem: &str) -> bool {
    WINDOWS_RESERVED_DEVICES
        .iter()
        .any(|device| device.eq_ignore_ascii_case(stem))
}

pub fn safe_filename(name: &str) -> String {
    let file_name = Path::new(name)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sanitized = UNSAFE_FN_CHARS.replace_all(&file_name, "_").into_owned();
    let base_name = if sanitized.trim().is_empty() || sanitized == "." || sanitized == ".." {
        "unnamed".to_string()
    } else {
        sanitized
    };

    let check_stem = base_name.split('.').next().unwrap_or(&base_name);
    if is_windows_reserved_device(check_stem) {
        format!("_{base_name}")
    } else {
        base_name
    }
}

/// Upper bound on any single mkvmerge/mkvextract/ffmpeg invocation.
pub const SUBPROCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

/// Item error when the ADR-0011 I/O semaphore is closed: the item fails rather than
/// running without the disk-write limit.
pub(crate) const IO_GOVERNOR_CLOSED: &str =
    "I/O governor closed; refusing to run without the disk-write limit";

pub fn output_stem(base: &str, ep_type: &EpisodeType, ep: &str) -> String {
    if ep_type.is_special_or_ova() {
        format!("{}{ep}_{base}", ep_type.as_str())
    } else {
        base.to_string()
    }
}

/// `path` relative to `root`, compared lexically first and then by canonical form.
fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(rel) = path.strip_prefix(root) {
        return Some(rel.to_path_buf());
    }
    let path = path.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    path.strip_prefix(&root).ok().map(Path::to_path_buf)
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
    match relative_to(parent, input_dir) {
        Some(rel) => out_override.join(rel),
        None => out_override.to_path_buf(),
    }
}

pub fn seasons_match(mkv_season: &str, sub_season: &str) -> bool {
    if mkv_season.is_empty() || sub_season.is_empty() {
        true
    } else {
        mkv_season == sub_season
    }
}

/// Both seasons are known and equal: the closer kind of `seasons_match`. An unknown season on
/// either side only matches openly.
pub fn season_is_exact(mkv_season: &str, sub_season: &str) -> bool {
    !mkv_season.is_empty() && mkv_season == sub_season
}

pub fn ancestor_dirs(mkv_dir: &Path, root_input_dir: &Path) -> Vec<PathBuf> {
    let Some(rel) = relative_to(mkv_dir, root_input_dir) else {
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
                if !require_known || KNOWN_LANG_TOKENS.contains(&token.as_str()) {
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

fn format_ep(val: &str) -> String {
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
    let normalized = without_offset.replace('_', " ");

    // Replace non-decimal '.' with space
    let mut norm_bytes = normalized.into_bytes();
    for i in 0..norm_bytes.len() {
        if norm_bytes[i] == b'.' {
            let prev_digit = i > 0 && norm_bytes[i - 1].is_ascii_digit();
            let next_digit = i + 1 < norm_bytes.len() && norm_bytes[i + 1].is_ascii_digit();
            if !(prev_digit && next_digit) {
                norm_bytes[i] = b' ';
            }
        }
    }
    let mut normalized = String::from_utf8(norm_bytes).unwrap_or_default();

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

    if let Ok(Some(caps)) = EP_NUM_MARKER_RE.captures(normalized.as_str()) {
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

// ─── TRACK SELECTION & HEURISTICS ────────────────────────────────────────────

static SPLIT_WORDS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s\-_]+").unwrap());

pub fn is_dialogue_track(name: &str) -> bool {
    let lower = name.to_lowercase();
    SPLIT_WORDS_RE
        .split(&lower)
        .any(|token| DIALOGUE_KEYWORDS.contains(&token))
}

/// Picks the embedded subtitle track to use, returning its track ID and language.
pub fn select_best_subtitle_track(
    tracks: &[MkvTrack],
    codec_filter: Option<&HashSet<String>>,
) -> Option<(u32, String)> {
    tracks
        .iter()
        .filter(|t| t.track_type == "subtitles")
        .filter_map(|t| {
            let tid = t.id?;
            let codec_id = t
                .properties
                .codec_id
                .as_deref()
                .unwrap_or("")
                .to_lowercase();
            if codec_filter.is_some_and(|filter| !filter.contains(&codec_id)) {
                return None;
            }
            let language = t.properties.language.as_deref().unwrap_or("und");
            let is_dialogue = is_dialogue_track(t.properties.track_name.as_deref().unwrap_or(""));
            let score = score_subtitle_category(language, codec_id == "s_text/ass");
            Some(((score, is_dialogue, Reverse(tid)), tid, language))
        })
        .max_by_key(|(key, ..)| *key)
        .map(|(_, tid, language)| (tid, language.to_string()))
}

static COMMENTARY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(comment(?:s|ary|aries)?|audio\s+description|ad|extension)\b").unwrap()
});

/// Ranks an audio track for `select_audio_track`; higher is better.
fn audio_score(track: &AudioTrack, video_pos: u32) -> i32 {
    let is_ja = track.lang.starts_with("ja") || track.lang.starts_with("jpn");
    let is_und = track.lang == "und";
    let is_stereo = track.channels == 2;
    let is_dialogue = is_dialogue_track(&track.name);

    if is_ja && is_stereo && is_dialogue {
        10
    } else if is_ja && is_stereo {
        9
    } else if is_ja {
        8
    } else if is_und && is_stereo && is_dialogue {
        7
    } else if is_und && is_stereo {
        6
    } else if is_und {
        5
    } else if is_stereo {
        4
    } else if track.pos > video_pos {
        3
    } else {
        1
    }
}

/// Picks the audio track to keep, preferring Japanese, then undetermined, then stereo.
///
/// Commentary tracks are considered only when every track looks like commentary.
pub fn select_audio_track(audio_tracks: &[AudioTrack], video_pos: u32) -> Option<AudioTrack> {
    let is_primary = |t: &AudioTrack| !COMMENTARY_RE.is_match(&t.name);
    let is_fallback_pool = !audio_tracks.iter().any(is_primary);

    audio_tracks
        .iter()
        .filter(|&t| is_fallback_pool || is_primary(t))
        .max_by_key(|t| (audio_score(t, video_pos), Reverse(t.pos)))
        .cloned()
}

/// Builds the error returned when a promotion's rollback rename fails, naming both paths
/// so the orphaned backup can be located by hand.
fn rollback_error(
    final_path: &Path,
    backup_path: &Path,
    rb_err: &std::io::Error,
) -> std::io::Error {
    std::io::Error::other(format!(
        "Failed to replace destination '{}' and rollback failed ({rb_err}); backup preserved at '{}'",
        final_path.display(),
        backup_path.display()
    ))
}

/// Process-unique token for naming temporary and backup files.
pub fn unique_file_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{pid}.{nanos}.{seq}")
}

/// True when a failed `rename` means the two paths sit on different devices, which is the
/// main condition under which a copy can succeed where the rename did not.
///
/// Recognises `ErrorKind::CrossesDevices` plus the raw code for `EXDEV` (18, Linux/BSD) and
/// `ERROR_NOT_SAME_DEVICE` (17, Windows), which not every platform surfaces as a typed
/// kind. The codes are platform-scoped: 18 is `ERROR_NO_MORE_FILES` on Windows, and 17 is
/// not a device error off it.
///
/// This is a heuristic about the *error*, not a proof that a copy would help. A copy can
/// still rescue a failure that only affects the source side — see `promote_file`.
fn is_cross_device_rename(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::CrossesDevices
        || (cfg!(not(windows)) && err.raw_os_error() == Some(18))
        || (cfg!(windows) && err.raw_os_error() == Some(17))
}

/// Atomically moves `staged` onto `final_path`, creating the parent directory. An existing
/// destination is replaced via a backup-and-rollback rename where the platform's `rename`
/// refuses to overwrite.
///
/// Only a device-boundary failure (see `is_cross_device_rename`) falls back to copying into
/// a temporary sibling of `final_path` and renaming it into place. Other rename failures —
/// a missing source, a destination locked by an antivirus or indexer — return the rename
/// error rather than duplicating a multi-gigabyte payload to fail the same way.
///
/// Consequence: `rename(2)` needs write permission on *both* parents, so a staged file in a
/// read-only directory is refused even though a copy could have read it. Callers stage
/// inside directories this process creates: `extract` and `remux` use
/// `tempdir_in(<destination>)`, font promotion `<fonts_dir>/.font_staging`. See ADR-0013's
/// amendment.
pub fn promote_file(staged: &Path, final_path: &Path) -> Result<(), std::io::Error> {
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let rename_err = match std::fs::rename(staged, final_path) {
        Ok(()) => return Ok(()),
        Err(err) => err,
    };

    if !is_cross_device_rename(&rename_err) {
        // Destination already exists (typical on Windows, where rename refuses to replace):
        // move it aside, put `staged` in its place, and roll the backup back if that fails.
        if final_path.exists() {
            let backup_path = final_path.with_extension(format!("bak.{}", unique_file_token()));
            if std::fs::rename(final_path, &backup_path).is_ok() {
                if std::fs::rename(staged, final_path).is_ok() {
                    let _ = std::fs::remove_file(&backup_path);
                    return Ok(());
                } else if let Err(rb_err) = std::fs::rename(&backup_path, final_path) {
                    return Err(rollback_error(final_path, &backup_path, &rb_err));
                }
            }
        }
        // Not a device boundary: a copy would usually fail the same way, so report the
        // rename error.
        return Err(rename_err);
    }

    // Fallback across filesystem boundaries: copy to a temporary sibling file in final_path's
    // parent directory first, then rename over final_path to guarantee atomic replacement.
    let temp_sibling = parent.join(format!(".tmp_promote_{}", unique_file_token()));
    if let Err(e) = std::fs::copy(staged, &temp_sibling) {
        let _ = std::fs::remove_file(&temp_sibling);
        return Err(e);
    }

    let rename_result = if std::fs::rename(&temp_sibling, final_path).is_ok() {
        Ok(())
    } else if final_path.exists() {
        let backup_path = final_path.with_extension(format!("bak.{}", unique_file_token()));
        if std::fs::rename(final_path, &backup_path).is_ok() {
            if std::fs::rename(&temp_sibling, final_path).is_ok() {
                let _ = std::fs::remove_file(&backup_path);
                Ok(())
            } else {
                let _ = std::fs::remove_file(&temp_sibling);
                if let Err(rb_err) = std::fs::rename(&backup_path, final_path) {
                    Err(rollback_error(final_path, &backup_path, &rb_err))
                } else {
                    Err(std::io::Error::other(
                        "Failed to replace destination with temporary sibling file",
                    ))
                }
            }
        } else {
            let _ = std::fs::remove_file(&temp_sibling);
            Err(std::io::Error::other(
                "Failed to backup existing destination file",
            ))
        }
    } else {
        let _ = std::fs::remove_file(&temp_sibling);
        Err(std::io::Error::other(
            "Failed to rename temporary sibling file to final destination",
        ))
    };

    if rename_result.is_ok() {
        let _ = std::fs::remove_file(staged);
    }
    rename_result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins which rename failures are eligible for the copy fallback.
    ///
    /// The control flow that consults this is not observable from an integration test (see
    /// `test_promote_file_cross_device_copy`), so this is where a widening or narrowing of
    /// the rule would surface: an unrelated `ErrorKind` admitted here would send every
    /// such failure to a whole-payload copy.
    #[test]
    fn test_is_cross_device_rename_classifies_only_device_boundaries() {
        let raw = std::io::Error::from_raw_os_error;

        // Device boundaries, each raw code scoped to the platform that defines it: 18 is
        // EXDEV on Linux/BSD but ERROR_NO_MORE_FILES on Windows, and 17 is
        // ERROR_NOT_SAME_DEVICE on Windows but unrelated elsewhere.
        if cfg!(windows) {
            assert!(is_cross_device_rename(&raw(17)));
            assert!(
                !is_cross_device_rename(&raw(18)),
                "ERROR_NO_MORE_FILES must not trigger a whole-payload copy"
            );
        } else {
            assert!(is_cross_device_rename(&raw(18)));
            assert!(
                !is_cross_device_rename(&raw(17)),
                "ERROR_NOT_SAME_DEVICE must not count off Windows"
            );
        }
        assert!(is_cross_device_rename(&std::io::Error::from(
            std::io::ErrorKind::CrossesDevices,
        )));

        // Failures a copy generally cannot rescue, by raw code and by kind. ENOENT and
        // ENOTDIR are source-side, and `fs::copy` opens the source first. EACCES and EPERM
        // are the locked-destination case that motivated the gate.
        for code in [2, 20, 13, 1] {
            assert!(
                !is_cross_device_rename(&raw(code)),
                "raw OS error {code} must not reach the copy fallback"
            );
        }
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::AlreadyExists,
            std::io::ErrorKind::InvalidInput,
        ] {
            assert!(
                !is_cross_device_rename(&std::io::Error::from(kind)),
                "{kind:?} must not reach the copy fallback"
            );
        }
    }
}
