"""
mkv_common.py
Shared logic for mkv_extract.py and mkv_remux.py.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import threading
import logging
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
    "select_best_subtitle_track",
    # mkvmerge / dependency helpers
    "check_dependencies", "run_mkvmerge_json", "run_subprocess", "run_subprocess_quiet",
    "inspect_videos", "PersistentInspectCache", "get_default_cache_file", "setup_logging",
    # natural sorting
    "natural_sort_key",
    # scandir helpers
    "ParsedFile", "ScanResults", "scan_mkv_files", "scan_assets",
    # subtitle shift helper
    "shift_ass_timestamps",
]

# Initialize shared parent logger
logger = logging.getLogger("mkv_automation")

MAX_DEFAULT_WORKERS = 16
DEFAULT_WORKER_MULTIPLIER = 2
EpisodeInfo = tuple[str, str, str]


# ─── SORTING HELPER ──────────────────────────────────────────────────────────
def natural_sort_key(path: Path | str) -> list:
    """Key function for sorting paths naturally (e.g. EP2 before EP10)."""
    return [int(text) if text.isdigit() else text.lower() for text in re.split(r'(\d+)', str(path))]


class BelowLevelFilter(logging.Filter):
    """Filter log records whose level is strictly below a specified level."""
    def __init__(self, below_level: int):
        super().__init__()
        self.below_level = below_level

    def filter(self, record: logging.LogRecord) -> bool:
        return record.levelno < self.below_level


def setup_logging(level: int = logging.INFO) -> None:
    """Configure system-wide logging with standard output and error splits."""
    root_logger = logging.getLogger()
    root_logger.setLevel(level)

    # Remove pre-existing default handlers
    for handler in list(root_logger.handlers):
        root_logger.removeHandler(handler)
        handler.close()

    log_format = logging.Formatter('%(asctime)s [%(levelname)s] %(message)s')

    # Stream standard info/warning messages strictly to stdout
    stdout_handler = logging.StreamHandler(sys.stdout)
    stdout_handler.setLevel(level)
    stdout_handler.setFormatter(log_format)
    stdout_handler.addFilter(BelowLevelFilter(logging.ERROR))

    # Stream error/critical messages strictly to stderr
    stderr_handler = logging.StreamHandler(sys.stderr)
    stderr_handler.setLevel(logging.ERROR)
    stderr_handler.setFormatter(log_format)

    root_logger.addHandler(stdout_handler)
    root_logger.addHandler(stderr_handler)


def parse_attachments(attachments: list[dict]) -> tuple[dict | None, list[dict]]:
    """Parse attachments list once, returning (cover, list of fonts)."""
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
def run_subprocess(cmd: list[str], *, timeout: int | None = None) -> subprocess.CompletedProcess[str]:
    """Run a subprocess with consistent UTF-8 encoding/error handling."""
    return subprocess.run(
        cmd, capture_output=True, text=True,
        encoding="utf-8", errors="replace", timeout=timeout,
    )


# ─── NAMING HELPERS ──────────────────────────────────────────────────────────
_UNSAFE_FN_CHARS = re.compile(r'[/:*?"<>|\\]')


def safe_filename(name: str) -> str:
    """Sanitize a filename for OS-safe output."""
    return _UNSAFE_FN_CHARS.sub("_", Path(name).name)


def output_stem(base: str, ep_type: str, ep: str) -> str:
    """Build output filename stem with optional OVA/SP prefix."""
    if ep_type in ("OVA", "SP"):
        return f"{ep_type}{ep}_{base}"
    return base


# ─── FONT REGISTRY (thread-safe dedup by hash and name/size) ──────────────── Preserved
class FontRegistry:
    """Thread-safe registry of claimed font hashes and name/sizes for cross-file dedup."""

    def __init__(self, existing: set[str] | None = None) -> None:
        self._claimed_hashes: set[str] = set(existing or ())
        self._claimed_name_size: set[tuple[str, int]] = set()
        self._lock = threading.Lock()

    def try_claim_hash(self, file_hash: str) -> bool:
        """Atomically claim a file hash. Returns True if newly claimed."""
        with self._lock:
            if file_hash in self._claimed_hashes:
                return False
            self._claimed_hashes.add(file_hash)
            return True

    def try_claim_name_size(self, name: str, size: int) -> bool:
        """Atomically claim a font by name and size. Returns True if newly claimed."""
        with self._lock:
            key = (name, size)
            if key in self._claimed_name_size:
                return False
            self._claimed_name_size.add(key)
            return True

    def release_name_size(self, name: str, size: int) -> None:
        """Release a claimed name and size (e.g. if extraction fails)."""
        with self._lock:
            self._claimed_name_size.discard((name, size))

    @property
    def claimed_hashes(self) -> set[str]:
        with self._lock:
            return set(self._claimed_hashes)


# ─── FONTS DIRECTORY DISCOVERY ───────────────────────────────────────────────
# Fansub releases sometimes ship external fonts in a folder alongside external
# subtitles, but naming isn't standardised ("Fonts", "FONT", "attachments").
# This lets both scripts recognise the common variants without the user
# having to rename anything every time.
#
# Deliberately NOT recursive: every other discovery in this codebase (videos,
# subtitles, chapters) only looks at the top level of input_dir, since the
# whole design assumes one flat folder per release. Searching subdirectories
# for font-like files would be non-deterministic when more than one candidate
# exists, and could silently pick up an unrelated folder (another season, a
# leftover backup, an "Extras" folder) if input_dir isn't perfectly flat.
_FONT_DIR_NAMES = frozenset({"fonts", "font", "attachments"})


def find_fonts_dir(input_dir: Path, override: Path | None = None) -> Path:
    """
    Resolve which directory to use for fonts.

    Priority:
      1. `override` (e.g. from --fonts-dir) — used exactly as given.
      2. input_dir / "fonts" (the canonical, exact-match default) — checked
         first so existing setups are completely unaffected by this function.
      3. Any TOP-LEVEL subdirectory of input_dir whose name case-insensitively
         matches a recognised fonts-folder convention (fonts/font/attachments),
         checked in sorted order for determinism.
      4. Falls back to input_dir / "fonts" even if it doesn't exist yet —
         callers decide whether to create it or treat it as "no fonts".

    Always returns a Path (never None) — callers just check .is_dir().
    """
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

_YEAR_GUARD = r'(?!(?:19|20)\d{2}(?!\d))'

# High-priority explicit markers anywhere in the filename.
# NOTE: (?![0-9]) after \d{1,4} in the dash branch prevents 5-digit numbers
# from being silently truncated. _YEAR_GUARD rejects release years (1900-2099).
_EP_NUM_MARKER_RE = re.compile(
    r'(?:[-\u2013]\s*)' + _YEAR_GUARD + r'(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?![0-9pib])(?:\s*(?:end|fin|final))?(?:[vV]\d)?'
    r'|[\[(](\d{1,3}(?:\.\d{1,2}(?![0-9pib]))?)[\])](?:[vV]\d)?'
    r'|\b[Ee][Pp](?:isode)?\.?\s*(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)(?:[vV]\d)?\b',
    re.IGNORECASE
)

# Fallback standalone numbers (for titles without explicit dashes/EP tags)
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
    """Return (season, type, ep) or None."""
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
def select_best_subtitle_track(
    tracks: list[dict],
    *,
    codec_filter: frozenset[str] | None = None,
    log: list[str] | None = None,
) -> tuple[int, str, bool] | None:
    """
    Priority:
    1. Primary languages (ind → id → und), dialogue-keyword tiebreaker
    2. Fallback languages (eng → en), same tiebreaker
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
            if log is not None:
                log.append(
                    f"   -> INFO: Track {tid} ({codec_id or 'unknown codec'}) "
                    "— excluded by codec filter, skipped"
                )
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

    if log is not None:
        category_score = priority_key[0]
        category_desc = {
            12: "Indonesian ASS (ind)",
            11: "Indonesian ASS (id)",
            10: "Undetermined ASS (und)",
            9: "English ASS (eng)",
            8: "English ASS (en)",
            7: "Other ASS",
            6: "Indonesian non-ASS (ind)",
            5: "Indonesian non-ASS (id)",
            4: "Undetermined non-ASS (und)",
            3: "English non-ASS (eng)",
            2: "English non-ASS (en)",
            1: "Other non-ASS",
        }.get(category_score, "unknown")
        
        log.append(
            f"   -> Selected Track {tid} (lang '{language}', {category_desc}, "
            f"dialogue={is_diag})"
        )

    return tid, language, is_diag


# ─── MKVMERGE / DEPENDENCY HELPERS ───────────────────────────────────────────
def check_dependencies(*required: str) -> None:
    for cmd in required:
        if not shutil.which(cmd):
            logger.error("Required dependency '%s' not found in PATH.", cmd)
            sys.exit(1)


def run_mkvmerge_json(filepath: Path, log: list[str] | None = None) -> dict | None:
    def _warn(msg: str):
        if log is not None:
            log.append(f"  WARNING: {msg}")
        else:
            logger.warning(msg)

    try:
        result = run_subprocess(["mkvmerge", "-J", str(filepath)], timeout=60)
    except subprocess.TimeoutExpired:
        _warn(f"mkvmerge inspection timed out for '{filepath.name}', skipping.")
        return None
    if result.stderr.strip():
        _warn(f"(mkvmerge) {result.stderr.strip()}")
    if result.returncode > 1:
        _warn(f"mkvmerge failed (exit {result.returncode}), skipping '{filepath.name}'.")
        return None
    if not result.stdout.strip():
        _warn(f"No metadata returned for '{filepath.name}', skipping.")
        return None
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        _warn(f"Failed to parse metadata for '{filepath.name}': {exc}")
        return None


def get_default_cache_file() -> Path:
    local_app_data = os.environ.get("LOCALAPPDATA")
    if local_app_data:
        base_dir = Path(local_app_data) / "mkvanimebatch"
    else:
        base_dir = Path.home() / ".cache" / "mkvanimebatch"
    return base_dir / "inspect_cache.json"


class PersistentInspectCache:
    """Persistent JSON cache for mkvmerge inspection data keyed on (path, size, mtime)."""

    def __init__(self, cache_file: Path | None = None, enabled: bool = True) -> None:
        self.cache_file = cache_file or get_default_cache_file()
        self.enabled = enabled
        self._data: dict[str, dict] = {}
        self._lock = threading.Lock()
        if self.enabled:
            self._load()

    def _load(self) -> None:
        if not self.cache_file.exists():
            return
        try:
            with open(self.cache_file, "r", encoding="utf-8") as f:
                self._data = json.load(f)
        except Exception:
            self._data = {}

    def save(self) -> None:
        if not self.enabled:
            return
        try:
            self.cache_file.parent.mkdir(parents=True, exist_ok=True)
            with self._lock:
                # Prune cache entries for files that no longer exist on disk
                pruned_data = {k: v for k, v in self._data.items() if Path(k).exists()}
                self._data = pruned_data
            temp_file = self.cache_file.with_suffix(".tmp")
            with open(temp_file, "w", encoding="utf-8") as f:
                json.dump(self._data, f)
            temp_file.replace(self.cache_file)
        except Exception:
            pass

    def get(self, path: Path) -> dict | None:
        if not self.enabled:
            return None
        key = str(path.resolve())
        with self._lock:
            entry = self._data.get(key)
        if not entry:
            return None
        try:
            st = path.stat()
            if entry.get("size") == st.st_size and abs(entry.get("mtime", 0) - st.st_mtime) < 0.001:
                return entry.get("json_data")
        except OSError:
            pass
        return None

    def put(self, path: Path, json_data: dict | None) -> None:
        if not self.enabled:
            return
        key = str(path.resolve())
        try:
            st = path.stat()
            with self._lock:
                self._data[key] = {
                    "size": st.st_size,
                    "mtime": st.st_mtime,
                    "json_data": json_data,
                }
        except OSError:
            pass

    def clear(self) -> None:
        with self._lock:
            self._data.clear()
        if self.cache_file.exists():
            try:
                self.cache_file.unlink(missing_ok=True)
            except OSError:
                pass


def inspect_videos(
    videos: list[Path],
    pool: ThreadPoolExecutor,
    persistent_cache: PersistentInspectCache | None = None,
    force_refresh: bool = False,
) -> dict[Path, dict | None]:
    """
    Run `mkvmerge -J` on every video in parallel and return a cache mapping
    each path to its parsed JSON (or None if inspection failed).
    """
    cache: dict[Path, dict | None] = {}
    to_inspect: list[Path] = []

    if persistent_cache and not force_refresh:
        for v in videos:
            cached_data = persistent_cache.get(v)
            if cached_data is not None:
                cache[v] = cached_data
            else:
                to_inspect.append(v)
    else:
        to_inspect = videos

    if to_inspect:
        cached_count = len(videos) - len(to_inspect)
        if cached_count > 0:
            logger.info("Pre-inspecting %d video(s) (%d loaded from cache)...", len(to_inspect), cached_count)
        else:
            logger.info("Pre-inspecting %d video(s)...", len(to_inspect))

        def _inspect(v: Path) -> tuple[Path, dict | None]:
            return v, run_mkvmerge_json(v)

        futures = {pool.submit(_inspect, v): v for v in to_inspect}
        for future in as_completed(futures):
            v = futures[future]
            try:
                _, data = future.result()
                cache[v] = data
                if persistent_cache:
                    persistent_cache.put(v, data)
            except Exception as exc:
                logger.warning("Inspection failed for %s: %s", v.name, exc)
                cache[v] = None

        if persistent_cache:
            persistent_cache.save()

    ok = sum(1 for d in cache.values() if d is not None)
    logger.info("Inspection complete: %d/%d succeeded.", ok, len(videos))
    return cache


def run_subprocess_quiet(cmd: list[str], *, timeout: int | None = None) -> subprocess.CompletedProcess[str]:
    """Run a subprocess and redirect stdout to DEVNULL, preserving stderr."""
    return subprocess.run(
        cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
        encoding="utf-8", errors="replace", timeout=timeout,
    )


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


def scan_mkv_files(dir_path: Path, ignore_paths: set[Path] | None = None) -> list[ParsedFile]:
    results = []
    resolved_ignores = {p.resolve() for p in ignore_paths} if ignore_paths else set()
    def _scan(current_dir: str):
        try:
            with os.scandir(current_dir) as it:
                for entry in it:
                    try:
                        name = entry.name
                        if entry.is_file():
                            if name.lower().endswith(".mkv"):
                                path = Path(entry.path)
                                results.append(_create_parsed_file(path))
                        elif entry.is_dir(follow_symlinks=False):
                            if name.lower() != "output":
                                entry_path = Path(entry.path)
                                if resolved_ignores and entry_path.resolve() in resolved_ignores:
                                    continue
                                _scan(entry.path)
                    except OSError:
                        continue
        except OSError:
            pass
    _scan(str(dir_path))
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

_ASS_TIME_RE = re.compile(r'^(\d+):(\d{2}):(\d{2})\.(\d{2})$')


def _ass_time_to_cs(t: str) -> int:
    """Parse ASS format time (H:MM:SS.cc) to centiseconds."""
    m = _ASS_TIME_RE.match(t.strip())
    if not m:
        raise ValueError(f"Bad ASS timestamp: {t!r}")
    h, mi, s, cs = map(int, m.groups())
    return ((h * 3600 + mi * 60 + s) * 100) + cs


def _ass_cs_to_time(total_cs: int) -> str:
    """Format centiseconds to ASS format time (H:MM:SS.cc) with clamping at 0."""
    total_cs = max(0, total_cs)  # clamp at 0:00:00.00, timestamps can't go negative
    cs = total_cs % 100
    total_s = total_cs // 100
    s = total_s % 60
    total_m = total_s // 60
    m = total_m % 60
    h = total_m // 60
    return f"{h}:{m:02d}:{s:02d}.{cs:02d}"


def shift_ass_timestamps(path: Path, offset_seconds: float, log: list[str] | None = None) -> int:
    """Shift every Dialogue/Comment line in an .ass file by offset_seconds (in place). Returns the number of lines shifted."""
    if offset_seconds == 0.0:
        return 0

    offset_cs = round(offset_seconds * 100)
    try:
        data = path.read_bytes()
        has_bom = data.startswith(b"\xef\xbb\xbf")
        raw = data.decode("utf-8-sig")
    except (OSError, UnicodeDecodeError) as e:
        if log is not None:
            log.append(f"   -> ERROR (sub shift): Failed to read subtitle file: {e}")
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
        try:
            encoding = "utf-8-sig" if has_bom else "utf-8"
            # newline="" prevents \n translation: raw already keeps original CRLF/LF endings
            path.write_text("".join(out_lines), encoding=encoding, newline="")
            if log is not None:
                log.append(f"   -> Shifted subtitle timing by {offset_seconds:+.2f}s ({shifted_count} lines)")
        except Exception as e:
            if log is not None:
                log.append(f"   -> ERROR (sub shift): Failed to write shifted subtitle: {e}")

    return shifted_count
