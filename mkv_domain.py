"""
mkv_domain.py
Anime-release domain rules: episode parsing, language detection, track
scoring/selection, output naming, and asset-type vocabulary.
"""
from __future__ import annotations

import os
import re
from functools import lru_cache
from pathlib import Path
from typing import NamedTuple

MAX_DEFAULT_WORKERS = 16
MAX_FFMPEG_AUDIO_THREADS = 8
DEFAULT_WORKER_MULTIPLIER = 2

class EpisodeInfo(NamedTuple):
    """Canonical parsed identity of an episode: season tag, type key, number."""

    season: str
    ep_type: str
    ep: str

def default_ffmpeg_threads(max_workers: int) -> int:
    """Per-job ffmpeg audio thread budget: divide CPUs across concurrent workers,
    capped at MAX_FFMPEG_AUDIO_THREADS."""
    return min(MAX_FFMPEG_AUDIO_THREADS, max(1, (os.cpu_count() or 4) // max_workers))

# ─── SORTING HELPER ──────────────────────────────────────────────────────────
_NATURAL_SORT_RE = re.compile(r"(\d+)")


def natural_sort_key(path: Path | str) -> list[int | str]:
    """Key function for sorting paths naturally (e.g. EP2 before EP10)."""
    return [int(text) if text.isdigit() else text.lower() for text in _NATURAL_SORT_RE.split(str(path))]

# ─── LANGUAGE CONFIG ─────────────────────────────────────────────────────────
SUBTITLE_LANGS_PRIMARY_ORDER  = ("ind", "id", "und")
SUBTITLE_LANGS_FALLBACK_ORDER = ("eng", "en")
SUBTITLE_LANGS_PRIMARY        = frozenset(SUBTITLE_LANGS_PRIMARY_ORDER)
SUBTITLE_LANGS_FALLBACK       = frozenset(SUBTITLE_LANGS_FALLBACK_ORDER)

DIALOGUE_KEYWORDS = frozenset({
    "full", "dialogue", "dialog", "main", "complete",
    "indo", "indonesia", "indonesian",
})

LANG_LABEL: dict[str, str] = {
    "ind": "Indonesian", "id": "Indonesian", "und": "Undetermined",
    "eng": "English", "en": "English",
    "jpn": "Japanese", "ja": "Japanese",
}

LANG_MKV_TAG: dict[str, str] = {
    "ind": "ind", "id": "ind", "und": "und",
    "eng": "eng", "en": "eng",
    "jpn": "jpn", "ja": "jpn",
}

SUBTITLE_EXTENSIONS = frozenset({".ass", ".srt"})
CHAPTER_EXT  = ".xml"

# ─── MIME TABLES ─────────────────────────────────────────────────────────────
COVER_MIME: dict[str, str] = {
    ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
    ".png": "image/png", ".webp": "image/webp",
    ".gif": "image/gif", ".avif": "image/avif",
}
COVER_EXTENSIONS = frozenset(COVER_MIME)

FONT_MIME: dict[str, str] = {
    ".ttf": "application/x-truetype-font",
    ".ttc": "application/x-truetype-font",
    ".otf": "application/vnd.ms-opentype",
    ".woff": "application/font-woff",
    ".woff2": "font/woff2",
    ".eot": "application/vnd.ms-fontobject",
}
FONT_EXTENSIONS = frozenset(FONT_MIME)

# ─── SUBTITLE SCORING TABLES ─────────────────────────────────────────────────
_ASS_SCORE_MAP: dict[str, int]     = {"ind": 12, "id": 11, "und": 10, "eng": 9, "en": 8}
_NON_ASS_SCORE_MAP: dict[str, int] = {"ind":  6, "id":  5, "und":  4, "eng": 3, "en": 2}


def score_subtitle_category(language: str, is_ass: bool) -> int:
    """Return priority score for a subtitle based on language and whether it is ASS format."""
    scores = _ASS_SCORE_MAP if is_ass else _NON_ASS_SCORE_MAP
    return scores.get(language, 7 if is_ass else 1)

# ─── NAMING & PATH HELPERS ───────────────────────────────────────────────────
_UNSAFE_FN_CHARS = re.compile(r'[/:*?"<>|\\]')


def safe_filename(name: str) -> str:
    """Sanitize a filename for OS-safe output."""
    return _UNSAFE_FN_CHARS.sub("_", Path(name).name)


def output_stem(base: str, ep_type: str, ep: str) -> str:
    """Build output filename stem with optional OVA/SP prefix."""
    if ep_type in ("OVA", "SP"):
        return f"{ep_type}{ep}_{base}"
    return base


def resolve_output_dir(
    mkv_dir: Path,
    root_input_dir: Path,
    out_override: Path | None,
    default_subdir: str | None = "Output",
) -> Path:
    """Preserve the source directory layout beneath a custom output root."""
    if out_override is None:
        return mkv_dir / default_subdir if default_subdir else mkv_dir
    try:
        relative_dir = mkv_dir.relative_to(root_input_dir)
    except ValueError:
        relative_dir = Path()
    return out_override / relative_dir


def seasons_match(mkv_season: str, sub_season: str) -> bool:
    if not mkv_season or not sub_season:
        return True
    return mkv_season == sub_season


def ancestor_dirs(mkv_dir: Path, root_input_dir: Path) -> list[Path]:
    """
    Return every directory on the path from root_input_dir down to mkv_dir,
    inclusive of both endpoints. If mkv_dir is not under root_input_dir the
    list contains only mkv_dir.
    """
    try:
        rel = mkv_dir.relative_to(root_input_dir)
    except ValueError:
        return [mkv_dir]
    return [root_input_dir, *(root_input_dir / Path(*rel.parts[:i]) for i in range(1, len(rel.parts) + 1))]

_KNOWN_LANG_TOKENS = frozenset({
    "ind", "id", "eng", "en", "jpn", "ja", "und",
    "zho", "zh", "kor", "ko", "ara", "ar", "por", "pt",
    "spa", "es", "fra", "fr", "deu", "de", "ita", "it",
    "rus", "ru", "tha", "th", "vie", "vi", "msa", "ms",
})

_BRACKET_LANG_RE = re.compile(
    r"\[(?:(ID|Ind|Indonesian|Indo)|(EN|ENG|English)|(JA|JPN|Japanese))\]",
    re.IGNORECASE,
)

_LANG_BEFORE_TRACK_RE = re.compile(r"[_.]([a-zA-Z]{2,3})_track\d+")
_LANG_AFTER_TRACK_RE  = re.compile(r"track\d+[_.]([a-zA-Z]{2,3})(?:$|[^a-zA-Z])")
_LANG_PAREN_SUFFIX_RE = re.compile(r"\(([a-zA-Z]{2,3})\)$")
_LANG_SUFFIX_RE = re.compile(r"[._]([a-zA-Z]{2,3})$")


_OFFSET_TAG_RE = re.compile(r"\.\[[-+]?\d+(?:\.\d+)?s\]$", re.IGNORECASE)


@lru_cache(maxsize=1024)
def detect_sub_lang(sub_stem: str) -> str:
    """Infer subtitle 3-letter language code ('ind', 'eng', 'jpn', 'und') from filename patterns."""
    sub_stem = _OFFSET_TAG_RE.sub("", sub_stem)
    m = _BRACKET_LANG_RE.search(sub_stem)
    if m:
        if m.group(1):
            return "ind"
        if m.group(2):
            return "eng"
        if m.group(3):
            return "jpn"

    for regex, require_known in (
        (_LANG_BEFORE_TRACK_RE, False),
        (_LANG_AFTER_TRACK_RE, False),
        (_LANG_PAREN_SUFFIX_RE, False),
        (_LANG_SUFFIX_RE, True),
    ):
        m = regex.search(sub_stem)
        if not m:
            continue
        token = m.group(1).lower()
        if not require_known or token in _KNOWN_LANG_TOKENS:
            return token

    return "und"

# ─── EPISODE PARSER ──────────────────────────────────────────────────────────
_OVA_PATTERN     = re.compile(r"(?<!\w)(OVA|OAD|ONA)(?=\d|\W|$)", re.IGNORECASE)
_SPECIAL_PATTERN = re.compile(r"(?<!\w)(Specials?|SP)(?=\d|\W|$)",  re.IGNORECASE)
_SXXEXX_PATTERN  = re.compile(r"[Ss](\d{1,2})[Ee](\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:-[Ee]?\d{1,4})?(?:[vV]\d)?")
_SXXPXX_PATTERN  = re.compile(r"[Ss](\d{1,2})[Pp](\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:-[Pp]?\d{1,4})?(?:[vV]\d)?")
_YEAR_GUARD = r"(?!(?:19|20)\d{2}(?!\d))"

_EP_NUM_MARKER_RE = re.compile(
    r"(?:[-\u2013]\s*)" + _YEAR_GUARD + r"(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:-\d{1,4})?(?![0-9pib])(?:\s*(?:end|fin|final))?(?:[vV]\d)?"
    r"|[\[(](\d{1,3}(?:\.\d{1,2}(?![0-9pib]))?)[\])](?:[vV]\d)?"
    r"|\b[Ee][Pp](?:isode)?\.?\s*(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:[vV]\d)?\b"
    r"|第\s*(\d{1,4}(?:\.\d{1,2})?)\s*話?",
    re.IGNORECASE
)

_EP_NUM_FALLBACK_RE = re.compile(
    r"(?:^|\s)(\d{1,3}(?:\.\d{1,2}(?![0-9pib]))?)(?![0-9pib])(?:\s*(?:end|fin|final))?(?:[vV]\d)?\b",
    re.IGNORECASE
)

_SEASON_PATTERNS = (
    re.compile(r"[Ss]eason\s*(\d{1,2})"),
    re.compile(r"\bS(\d{1,2})\b(?![EePp])"),
    re.compile(r"(?<=[a-zA-Z])\s+(\d{1})\s+(?=[a-zA-Z])"),
)


def _first_group(m: re.Match) -> str | None:
    return next((g for g in m.groups() if g is not None), None)


def _parse_typed_ep(name: str, pattern: re.Pattern) -> str:
    m = pattern.search(name)
    if m is None:
        return "00"
    remainder = name[m.end():]
    glued = re.match(r"\s*(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)", remainder)
    if glued:
        return format_ep(glued.group(1))

    m2 = _EP_NUM_MARKER_RE.search(remainder)
    if m2:
        val = _first_group(m2)
        if val is not None:
            return format_ep(val)

    matches = _EP_NUM_FALLBACK_RE.findall(remainder)
    if matches:
        return format_ep(matches[-1])
    return "00"


def format_ep(val: str) -> str:
    if "." in val:
        parts = val.split(".", 1)
        return f"{int(parts[0]):02d}.{parts[1]}"
    return f"{int(val):02d}"


@lru_cache(maxsize=1024)
def parse_episode(name: str) -> EpisodeInfo | None:
    """Return (season, type, ep) or None."""
    name = _OFFSET_TAG_RE.sub("", name)
    name = name.replace("_", " ")
    name = re.sub(r"(?<!\d)\.|\.(?!\d)", " ", name)

    season = ""
    for pat in _SEASON_PATTERNS:
        s = pat.search(name)
        if s:
            season = str(int(s.group(1)))
            name = name[:s.start()] + name[s.end():]
            break

    if _OVA_PATTERN.search(name):
        return EpisodeInfo(season, "OVA", _parse_typed_ep(name, _OVA_PATTERN))
    if _SPECIAL_PATTERN.search(name):
        return EpisodeInfo(season, "SP", _parse_typed_ep(name, _SPECIAL_PATTERN))

    m = _SXXEXX_PATTERN.search(name)
    if m:
        return EpisodeInfo(str(int(m.group(1))), "E", format_ep(m.group(2)))

    m = _SXXPXX_PATTERN.search(name)
    if m:
        return EpisodeInfo(str(int(m.group(1))), "P", format_ep(m.group(2)))

    ep_m = _EP_NUM_MARKER_RE.search(name)
    if ep_m:
        val = _first_group(ep_m)
        if val is not None:
            return EpisodeInfo(season, "E", format_ep(val))

    matches = _EP_NUM_FALLBACK_RE.findall(name)
    if matches:
        return EpisodeInfo(season, "E", format_ep(matches[-1]))

    return None


_EP_TYPE_SHORT = {"E": "EP", "P": "Part", "OVA": "OVA", "SP": "SP"}
_EP_TYPE_LONG  = {"E": "Episode", "P": "Part", "OVA": "OVA", "SP": "Special"}


def ep_type_label(ep_type: str, *, long: bool = False) -> str:
    table = _EP_TYPE_LONG if long else _EP_TYPE_SHORT
    return table.get(ep_type, ep_type)

def is_dialogue_track(name: str | None) -> bool:
    if not name:
        return False
    tokens = re.split(r"[\s\-_]+", name.lower())
    return bool(DIALOGUE_KEYWORDS.intersection(tokens))

def select_best_subtitle_track(
    tracks: list[dict],
    *,
    codec_filter: frozenset[str] | None = None,
    log: list[str] | None = None,
) -> tuple[int, str, bool] | None:
    candidates = []
    for t in tracks:
        if t.get("type") != "subtitles":
            continue
        tid = t.get("id")
        if tid is None:
            continue

        props    = t.get("properties") or {}
        codec_id = (props.get("codec_id") or "").lower()
        language = props.get("language") or "und"
        name     = props.get("track_name") or ""

        if codec_filter is not None and codec_id not in codec_filter:
            if log is not None:
                log.append(f"   -> INFO: Track {tid} ({codec_id or 'unknown codec'}) excluded by codec filter, skipped")
            continue

        is_diag = is_dialogue_track(name)
        is_ass  = (codec_id == "s_text/ass")
        category_score = score_subtitle_category(language, is_ass)

        priority_key = (category_score, 1 if is_diag else 0, -tid)
        candidates.append((priority_key, tid, language, is_diag))

    if not candidates:
        return None

    best_candidate = max(candidates, key=lambda c: c[0])
    priority_key, tid, language, is_diag = best_candidate

    if log is not None:
        log.append(f"   -> Selected Track {tid} (lang '{language}', dialogue={is_diag})")

    return tid, language, is_diag
