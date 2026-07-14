"""
mkv_common_fast.py
Simplified shared logic for mkv_extract_fast.py and mkv_remux_fast.py.
Optimized for performance and simplicity (minimal logging and checks).
"""

import json
import os
import re
import shutil
import subprocess
import sys
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

__all__ = [
    # constants
    "SUBTITLE_LANGS_PRIMARY", "SUBTITLE_LANGS_FALLBACK",
    "SUBTITLE_LANGS_PRIMARY_ORDER", "SUBTITLE_LANGS_FALLBACK_ORDER",
    "DIALOGUE_KEYWORDS", "COVER_EXTENSIONS", "COVER_MIME",
    "FONT_MIME", "FONT_EXTENSIONS",
    "LANG_LABEL", "LANG_MKV_TAG",
    "SUBTITLE_EXT", "CHAPTER_EXT",
    "MAX_DEFAULT_WORKERS", "DEFAULT_WORKER_MULTIPLIER",
    # type aliases
    "EpisodeInfo",
    # episode parsing
    "parse_episode", "ep_type_label", "is_dialogue_track",
    # naming helpers
    "safe_filename", "output_stem",
    # chapter sniffing
    "is_chapter_xml", "has_chapters",
    # cover sniffing
    "is_cover_filename", "is_cover_attachment",
    # font sniffing
    "is_font_attachment", "FontRegistry",
    # subtitle language detection
    "detect_sub_lang",
    # attachment finders
    "parse_attachments",
    # fonts directory discovery
    "find_fonts_dir",
    # subtitle track selection
    "select_best_subtitle_track", "shift_ass_timestamps",
    # mkvmerge / dependency helpers
    "check_dependencies", "run_mkvmerge_json", "run_subprocess",
    "inspect_videos",
    # natural sorting
    "natural_sort_key",
    # scandir helpers & caching
    "ParsedFile", "ScanResults", "scan_mkv_files", "scan_assets",
]

MAX_DEFAULT_WORKERS = 16
DEFAULT_WORKER_MULTIPLIER = 2
EpisodeInfo = tuple[str, str, str]

# ─── SORTING HELPER ──────────────────────────────────────────────────────────
def natural_sort_key(path: Path | str) -> list:
    return [int(text) if text.isdigit() else text.lower() for text in re.split(r'(\d+)', str(path))]


# ─── ATTACHMENT FINDERS ──────────────────────────────────────────────────────
def parse_attachments(attachments: list[dict]) -> tuple[dict | None, list[dict]]:
    """Separate front-cover image and font attachments from mkvmerge -J attachment metadata."""
    cover = None
    fonts = []
    for a in attachments:
        fname = a.get("file_name")
        if not fname:
            continue
        ctype = a.get("content_type") or ""
        if cover is None and is_cover_attachment(fname, ctype):
            cover = a
        if is_font_attachment(fname, ctype):
            fonts.append(a)
    return cover, fonts

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

SUBTITLE_EXT = ".ass"
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


def is_font_attachment(file_name: str, content_type: str) -> bool:
    if Path(file_name).suffix.lower() in FONT_EXTENSIONS:
        return True
    ct_lower = content_type.lower()
    return any(pat in ct_lower for pat in ("font", "truetype", "opentype"))


# ─── SUBPROCESS HELPER ───────────────────────────────────────────────────────
def run_subprocess(cmd: list[str], *, capture_stdout: bool = True, timeout: int | None = None) -> subprocess.CompletedProcess[str]:
    stdout_opt = subprocess.PIPE if capture_stdout else subprocess.DEVNULL
    return subprocess.run(
        cmd, stdout=stdout_opt, stderr=subprocess.PIPE, text=True,
        encoding="utf-8", errors="replace", timeout=timeout,
    )


# ─── NAMING HELPERS ──────────────────────────────────────────────────────────
_UNSAFE_FN_CHARS = re.compile(r'[/:*?"<>|\\]')


def safe_filename(name: str) -> str:
    return _UNSAFE_FN_CHARS.sub("_", Path(name).name)


def output_stem(base: str, ep_type: str, ep: str) -> str:
    if ep_type in ("OVA", "SP"):
        return f"{ep_type}{ep}_{base}"
    return base


# ─── FONT REGISTRY ───────────────────────────────────────────────────────────
class FontRegistry:
    def __init__(self, existing: set[tuple[str, int]] | None = None) -> None:
        self._claimed: set[tuple[str, int]] = set(existing or ())
        self._lock = threading.Lock()

    def try_claim(self, name: str, size: int) -> bool:
        with self._lock:
            if (name, size) in self._claimed:
                return False
            self._claimed.add((name, size))
            return True

    def release(self, name: str, size: int) -> None:
        with self._lock:
            self._claimed.discard((name, size))

    def is_claimed(self, name: str, size: int) -> bool:
        with self._lock:
            return (name, size) in self._claimed

    @property
    def claimed(self) -> set[tuple[str, int]]:
        with self._lock:
            return set(self._claimed)


# ─── FONTS DIRECTORY DISCOVERY ───────────────────────────────────────────────
_FONT_DIR_NAMES = frozenset({"fonts", "font", "attachments"})


def find_fonts_dir(input_dir: Path, override: Path | None = None) -> Path:
    if override is not None:
        return override

    exact = input_dir / "fonts"
    if exact.is_dir():
        return exact

    if input_dir.is_dir():
        for entry in sorted(input_dir.iterdir()):
            if entry.is_dir() and entry.name.lower() in _FONT_DIR_NAMES:
                return entry

    return exact


# ─── SUBTITLE LANGUAGE DETECTION ─────────────────────────────────────────────
_KNOWN_LANG_TOKENS = frozenset({
    "ind", "id", "eng", "en", "jpn", "ja", "und",
    "zho", "zh", "kor", "ko", "ara", "ar", "por", "pt",
    "spa", "es", "fra", "fr", "deu", "de", "ita", "it",
    "rus", "ru", "tha", "th", "vie", "vi", "msa", "ms",
})

_BRACKET_LANG_RE = re.compile(
    r'\[(?:(ID|Ind|Indonesian|Indo)|(EN|ENG|English)|(JA|JPN|Japanese))\]',
    re.IGNORECASE,
)

_LANG_BEFORE_TRACK_RE = re.compile(r"[_.]([a-zA-Z]{2,3})_track\d+")
_LANG_AFTER_TRACK_RE  = re.compile(r"track\d+[_.]([a-zA-Z]{2,3})(?:$|[^a-zA-Z])")
_LANG_PAREN_SUFFIX_RE = re.compile(r"\(([a-zA-Z]{2,3})\)$")
_LANG_SUFFIX_RE = re.compile(r"[._]([a-zA-Z]{2,3})$")


@lru_cache(maxsize=1024)
def detect_sub_lang(sub_stem: str) -> str:
    """Infer subtitle 3-letter language code ('ind', 'eng', 'jpn', 'und') from filename patterns."""
    # Strip any offset suffix like .[+5s] or .[-3s] at the end of the stem
    sub_stem = re.sub(r'\.\[[-+]?\d+(?:\.\d+)?s\]$', '', sub_stem, flags=re.IGNORECASE)
    m = _BRACKET_LANG_RE.search(sub_stem)
    if m:
        if m.group(1): return "ind"
        if m.group(2): return "eng"
        if m.group(3): return "jpn"

    m = _LANG_BEFORE_TRACK_RE.search(sub_stem)
    if m: return m.group(1).lower()

    m = _LANG_AFTER_TRACK_RE.search(sub_stem)
    if m: return m.group(1).lower()

    m = _LANG_PAREN_SUFFIX_RE.search(sub_stem)
    if m: return m.group(1).lower()

    m = _LANG_SUFFIX_RE.search(sub_stem)
    if m and m.group(1).lower() in _KNOWN_LANG_TOKENS:
        return m.group(1).lower()

    return "und"


# ─── EPISODE PARSER CONFIG ───────────────────────────────────────────────────
_OVA_PATTERN     = re.compile(r'(?<!\w)(OVA|OAD|ONA)(?=\d|\W|$)', re.IGNORECASE)
_SPECIAL_PATTERN = re.compile(r'(?<!\w)(Specials?|SP)(?=\d|\W|$)',  re.IGNORECASE)
_SXXEXX_PATTERN  = re.compile(r'[Ss](\d{1,2})[Ee](\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:[vV]\d)?')
_SXXPXX_PATTERN  = re.compile(r'[Ss](\d{1,2})[Pp](\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:[vV]\d)?')

_EP_NUM_MARKER_RE = re.compile(
    r'(?:[-\u2013]\s*)(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?![0-9pib])(?:\s*(?:end|fin|final))?(?:[vV]\d)?'
    r'|[\[(](\d{1,3}(?:\.\d{1,2}(?![0-9pib]))?)[\])](?:[vV]\d)?'
    r'|\b[Ee][Pp](?:isode)?\.?\s*(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:[vV]\d)?\b',
    re.IGNORECASE
)

_EP_NUM_FALLBACK_RE = re.compile(
    r'(?:^|\s)(\d{1,3}(?:\.\d{1,2}(?![0-9pib]))?)(?![0-9pib])(?:\s*(?:end|fin|final))?(?:[vV]\d)?\b',
    re.IGNORECASE
)

_SEASON_PATTERNS = (
    re.compile(r'[Ss]eason\s*(\d{1,2})'),
    re.compile(r'\bS(\d{1,2})\b(?![EePp])'),
    re.compile(r'(?<=[a-zA-Z])\s+(\d{1})\s+(?=[a-zA-Z])'),
)

def _first_group(m: re.Match) -> str | None:
    return next((g for g in m.groups() if g is not None), None)

def _parse_typed_ep(name: str, pattern: re.Pattern) -> str:
    m = pattern.search(name)
    if m is None: return '00'
    remainder = name[m.end():]
    glued = re.match(r'\s*(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)', remainder)
    if glued: 
        return format_ep(glued.group(1))
    
    m2 = _EP_NUM_MARKER_RE.search(remainder)
    if m2:
        val = _first_group(m2)
        if val is not None: return format_ep(val)
        
    matches = _EP_NUM_FALLBACK_RE.findall(remainder)
    if matches:
        return format_ep(matches[-1])
    return '00'


def format_ep(val: str) -> str:
    if '.' in val:
        parts = val.split('.', 1)
        return f"{int(parts[0]):02d}.{parts[1]}"
    return f"{int(val):02d}"


@lru_cache(maxsize=1024)
def parse_episode(name: str) -> EpisodeInfo | None:
    # Strip any offset suffix like .[+5s] or .[-3s] at the end of the name/stem
    name = re.sub(r'\.\[[-+]?\d+(?:\.\d+)?s\]$', '', name, flags=re.IGNORECASE)
    name = name.replace('_', ' ')
    name = re.sub(r'(?<!\d)\.|\.(?!\d)', ' ', name)

    season = ''
    for pat in _SEASON_PATTERNS:
        s = pat.search(name)
        if s:
            season = str(int(s.group(1)))
            name = name[:s.start()] + name[s.end():]
            break

    if _OVA_PATTERN.search(name):
        return season, 'OVA', _parse_typed_ep(name, _OVA_PATTERN)
    if _SPECIAL_PATTERN.search(name):
        return season, 'SP', _parse_typed_ep(name, _SPECIAL_PATTERN)

    m = _SXXEXX_PATTERN.search(name)
    if m: return str(int(m.group(1))), 'E', format_ep(m.group(2))

    m = _SXXPXX_PATTERN.search(name)
    if m: return str(int(m.group(1))), 'P', format_ep(m.group(2))

    ep_m = _EP_NUM_MARKER_RE.search(name)
    if ep_m:
        val = _first_group(ep_m)
        if val is not None:
            return season, 'E', format_ep(val)

    matches = _EP_NUM_FALLBACK_RE.findall(name)
    if matches:
        return season, 'E', format_ep(matches[-1])

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


# ─── CHAPTER CONTENT-SNIFFING ────────────────────────────────────────────────
_CHAPTER_SNIFF_BYTES = 4096
_CHAPTER_TAG_RE = re.compile(rb'<Chapters[ >]', re.IGNORECASE)


def is_chapter_xml(path: Path) -> bool:
    try:
        with path.open('rb') as fh:
            head = fh.read(_CHAPTER_SNIFF_BYTES)
        return bool(_CHAPTER_TAG_RE.search(head))
    except OSError:
        return False


def has_chapters(json_data: dict) -> bool:
    chapters = json_data.get("chapters") or []
    return bool(chapters) and any((c.get("num_entries") or 0) > 0 for c in chapters)


# ─── COVER IMAGE SNIFFING ────────────────────────────────────────────────────
_COVER_STEM_RE = re.compile(r'^cover$', re.IGNORECASE)
_COVER_LIKE_STEMS = frozenset({
    "cover", "cover_land", "small_cover", "small_cover_land",
    "poster", "thumbnail", "thumb", "fanart",
})


def is_cover_filename(file_name: str) -> bool:
    p = Path(file_name)
    return (
        p.suffix.lower() in COVER_EXTENSIONS
        and bool(_COVER_STEM_RE.match(p.stem))
    )


def is_cover_attachment(file_name: str, content_type: str) -> bool:
    if is_cover_filename(file_name):
        return True
    if not content_type.lower().startswith("image/"):
        return False
    return Path(file_name).stem.lower() in _COVER_LIKE_STEMS


# ─── UNIFIED SUBTITLE TRACK SELECTION ────────────────────────────────────────
_ASS_TIME_RE = re.compile(r'^(\d+):(\d{2}):(\d{2})\.(\d{2})$')


def _ass_time_to_cs(t: str) -> int:
    m = _ASS_TIME_RE.match(t.strip())
    if not m:
        raise ValueError(f"Bad ASS timestamp: {t!r}")
    h, mi, s, cs = map(int, m.groups())
    return ((h * 3600 + mi * 60 + s) * 100) + cs


def _ass_cs_to_time(total_cs: int) -> str:
    total_cs = max(0, total_cs)  # clamp at 0:00:00.00, timestamps can't go negative
    cs = total_cs % 100
    total_s = total_cs // 100
    s = total_s % 60
    total_m = total_s // 60
    m = total_m % 60
    h = total_m // 60
    return f"{h}:{m:02d}:{s:02d}.{cs:02d}"


def shift_ass_timestamps(path: Path, offset_seconds: float) -> int:
    """Shift every Dialogue/Comment line in an .ass file by offset_seconds (in place). Returns the number of lines shifted."""
    if offset_seconds == 0.0:
        return 0
    offset_cs = round(offset_seconds * 100)
    
    try:
        with path.open("rb") as f:
            has_bom = f.read(3) == b"\xef\xbb\xbf"
        raw = path.read_text(encoding="utf-8-sig")
    except (OSError, UnicodeDecodeError):
        return 0

    lines = raw.splitlines(keepends=True)

    out_lines = []
    in_events = False
    shifted_count = 0

    for line in lines:
        stripped = line.rstrip("\r\n")
        lower = stripped.lower()

        if lower.startswith("["):
            in_events = lower.startswith("[events]")
            out_lines.append(line)
            continue

        is_event_line = False
        prefix = ""
        rest = ""
        if in_events:
            parts_colon = stripped.split(":", 1)
            if len(parts_colon) == 2:
                key = parts_colon[0].strip().lower()
                if key in ("dialogue", "comment"):
                    is_event_line = True
                    prefix = parts_colon[0]
                    rest = parts_colon[1]

        if is_event_line:
            newline = line[len(stripped):]
            parts = rest.split(",", 9)
            if len(parts) >= 10:
                try:
                    parts[1] = _ass_cs_to_time(_ass_time_to_cs(parts[1]) + offset_cs)
                    parts[2] = _ass_cs_to_time(_ass_time_to_cs(parts[2]) + offset_cs)
                    out_lines.append(f"{prefix}:{','.join(parts)}{newline}")
                    shifted_count += 1
                except (ValueError, KeyError, IndexError):
                    out_lines.append(line)
            else:
                out_lines.append(line)
        else:
            out_lines.append(line)

    if shifted_count > 0:
        encoding = "utf-8-sig" if has_bom else "utf-8"
        path.write_text("".join(out_lines), encoding=encoding)
    return shifted_count


def select_best_subtitle_track(
    tracks: list[dict],
    *,
    codec_filter: frozenset[str] | None = None,
) -> tuple[int, str, bool] | None:
    """Select the best candidate subtitle track using a priority scoring matrix (language, codec, dialogue).
    
    Returns (track_id, detected_language, is_embedded) or None if no acceptable candidate exists.
    """
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
            continue

        is_diag = is_dialogue_track(name)
        is_ass  = (codec_id == "s_text/ass")

        # Category priority scoring (1 to 12):
        if is_ass:
            if language == "ind":
                category_score = 12
            elif language == "id":
                category_score = 11
            elif language == "und":
                category_score = 10
            elif language == "eng":
                category_score = 9
            elif language == "en":
                category_score = 8
            else:
                category_score = 7
        else:
            if language == "ind":
                category_score = 6
            elif language == "id":
                category_score = 5
            elif language == "und":
                category_score = 4
            elif language == "eng":
                category_score = 3
            elif language == "en":
                category_score = 2
            else:
                category_score = 1

        priority_key = (category_score, 1 if is_diag else 0, -tid)
        candidates.append((priority_key, tid, language, is_diag))

    if not candidates:
        return None

    # Pick candidate with highest priority_key
    best_candidate = max(candidates, key=lambda c: c[0])
    priority_key, tid, language, is_diag = best_candidate

    return tid, language, is_diag


# ─── MKVMERGE / DEPENDENCY HELPERS ───────────────────────────────────────────
def check_dependencies(*required: str) -> None:
    for cmd in required:
        if not shutil.which(cmd):
            print(f"[ERROR] Dependency '{cmd}' not found in PATH.")
            sys.exit(1)


def run_mkvmerge_json(filepath: Path) -> dict | None:
    try:
        result = run_subprocess(["mkvmerge", "-J", str(filepath)], timeout=60)
    except subprocess.TimeoutExpired:
        return None
    if result.returncode > 1 or not result.stdout.strip():
        return None
    try:
        return json.loads(result.stdout)
    except Exception:
        return None


def inspect_videos(videos: list[Path], pool: ThreadPoolExecutor) -> dict[Path, dict | None]:
    print(f"Pre-inspecting {len(videos)} video(s)...")
    cache: dict[Path, dict | None] = {}

    def _inspect(v: Path) -> tuple[Path, dict | None]:
        return v, run_mkvmerge_json(v)

    futures = {pool.submit(_inspect, v): v for v in videos}
    for future in as_completed(futures):
        v = futures[future]
        try:
            _, data = future.result()
            cache[v] = data
        except Exception as exc:
            print(f"[WARNING] Inspection failed for {v.name}: {exc}")
            cache[v] = None
    return cache


@dataclass(frozen=True)
class ParsedFile:
    path: Path
    name: str
    stem: str
    parsed: EpisodeInfo | None


def _create_parsed_file(path: Path) -> ParsedFile:
    """Construct a ParsedFile instance with episode number metadata pre-evaluated."""
    return ParsedFile(
        path=path,
        name=path.name,
        stem=path.stem,
        parsed=parse_episode(path.stem),
    )


@dataclass(frozen=True)
class ScanResults:
    subs: list[ParsedFile]
    chapters: list[ParsedFile]
    covers: list[ParsedFile]


def scan_mkv_files(dir_path: Path, recursive: bool = True, ignore_paths: set[Path] | None = None) -> list[ParsedFile]:
    results = []
    stack = [str(dir_path)]
    resolved_ignores = {p.resolve() for p in ignore_paths} if ignore_paths else set()
    
    while stack:
        current_dir = stack.pop()
        try:
            with os.scandir(current_dir) as it:
                for entry in it:
                    try:
                        name = entry.name
                        if entry.is_file():
                            if name.lower().endswith(".mkv"):
                                path = Path(entry.path)
                                results.append(_create_parsed_file(path))
                        elif entry.is_dir(follow_symlinks=False) and recursive:
                            if name.lower() != "output":
                                entry_path = Path(entry.path)
                                if resolved_ignores and entry_path.resolve() in resolved_ignores:
                                    continue
                                stack.append(entry.path)
                    except OSError:
                        continue
        except OSError:
            continue
            
    results.sort(key=lambda pf: natural_sort_key(pf.path))
    return results


def scan_assets(dir_path: Path) -> ScanResults:
    subs = []
    chapters = []
    covers = []
    try:
        with os.scandir(str(dir_path)) as it:
            for entry in it:
                try:
                    if not entry.is_file():
                        continue
                    suffix = Path(entry.name).suffix.lower()
                    path = Path(entry.path)
                    
                    if suffix == SUBTITLE_EXT:
                        subs.append(_create_parsed_file(path))
                    elif suffix == CHAPTER_EXT:
                        chapters.append(_create_parsed_file(path))
                    elif suffix in COVER_EXTENSIONS:
                        covers.append(_create_parsed_file(path))
                except OSError:
                    continue
    except OSError:
        pass
    
    subs.sort(key=lambda pf: natural_sort_key(pf.name))
    chapters.sort(key=lambda pf: natural_sort_key(pf.name))
    covers.sort(key=lambda pf: natural_sort_key(pf.name))
    return ScanResults(subs=subs, chapters=chapters, covers=covers)
