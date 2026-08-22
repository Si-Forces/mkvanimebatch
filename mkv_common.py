"""
mkv_common.py
Shared utilities, domain constants, data models, inspection caching, and event logging for mkvanimebatch.
"""
from __future__ import annotations

import hashlib
import json
import logging
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import nullcontext
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

__all__ = [
    # constants
    "CHAPTER_EXT",
    "COVER_EXTENSIONS",
    "COVER_MIME",
    "DEFAULT_WORKER_MULTIPLIER",
    "DIALOGUE_KEYWORDS",
    "FONT_EXTENSIONS",
    "FONT_MIME",
    "LANG_LABEL",
    "LANG_MKV_TAG",
    "MAX_DEFAULT_WORKERS",
    "SUBPROCESS_TIMEOUT",
    "SUBTITLE_EXTENSIONS",
    "SUBTITLE_LANGS_FALLBACK",
    "SUBTITLE_LANGS_FALLBACK_ORDER",
    "SUBTITLE_LANGS_PRIMARY",
    "SUBTITLE_LANGS_PRIMARY_ORDER",
    # types & data classes
    "EpisodeInfo",
    "FontClaim",
    "FontInfo",
    # classes
    "FontHashMemo",
    "FontManager",
    "ParsedFile",
    "PersistentInspectCache",
    "ScanResults",
    "StaleSubtitleTracker",
    # functions
    "acquire_semaphore",
    "ancestor_dirs",
    "check_dependencies",
    "cleanup_stale_subtitle_offsets_bulk",
    "detect_sub_lang",
    "emit_event",
    "ep_type_label",
    "find_fonts_dir",
    "format_ep",
    "get_default_cache_file",
    "get_default_font_hash_memo_file",
    "get_item_id",
    "has_chapters",
    "has_cover",
    "hash_file",
    "inspect_videos",
    "is_chapter_xml",
    "is_cover_attachment",
    "is_cover_filename",
    "is_dialogue_track",
    "is_events_enabled",
    "is_font_attachment",
    "natural_sort_key",
    "output_stem",
    "parse_attachments",
    "parse_episode",
    "resolve_output_dir",
    "run_mkvmerge_json",
    "run_subprocess",
    "safe_filename",
    "scan_assets",
    "scan_fonts",
    "scan_mkv_files",
    "seasons_match",
    "select_best_subtitle_track",
    "set_events_enabled",
    "setup_logging",
    "shift_ass_content",
    "shift_ass_timestamps",
]

# Initialize shared parent logger
logger = logging.getLogger("mkv_automation")

MAX_DEFAULT_WORKERS = 16
DEFAULT_WORKER_MULTIPLIER = 2
SUBPROCESS_TIMEOUT = 3600
EpisodeInfo = tuple[str, str, str]


_EVENTS_ENABLED: bool = False


def set_events_enabled(enabled: bool = True) -> None:
    """Enable or disable machine-readable structured @event: output."""
    global _EVENTS_ENABLED
    _EVENTS_ENABLED = enabled


def is_events_enabled() -> bool:
    """Return whether structured event output is currently enabled."""
    return _EVENTS_ENABLED


@lru_cache(maxsize=16384)
def get_item_id(video_path: Path, root_input_dir: Path) -> str:
    """Derive root-relative POSIX path ID for sidecar contract events and manifest items."""
    try:
        return video_path.resolve().relative_to(root_input_dir.resolve()).as_posix()
    except ValueError:
        try:
            return video_path.relative_to(root_input_dir).as_posix()
        except ValueError:
            return video_path.name


# ─── SORTING HELPER ──────────────────────────────────────────────────────────
_NATURAL_SORT_RE = re.compile(r"(\d+)")


def natural_sort_key(path: Path | str) -> list[int | str]:
    """Key function for sorting paths naturally (e.g. EP2 before EP10)."""
    return [int(text) if text.isdigit() else text.lower() for text in _NATURAL_SORT_RE.split(str(path))]


class BelowLevelFilter(logging.Filter):
    """Filter log records whose level is strictly below a specified level."""
    def __init__(self, below_level: int):
        super().__init__()
        self.below_level = below_level

    def filter(self, record: logging.LogRecord) -> bool:
        return record.levelno < self.below_level


def setup_logging(level: int = logging.INFO, *, stderr_only: bool = False) -> None:
    """Configure system-wide logging with standard output and error splits."""
    root_logger = logging.getLogger()
    root_logger.setLevel(level)

    for handler in list(root_logger.handlers):
        root_logger.removeHandler(handler)
        handler.close()

    log_format = logging.Formatter("%(asctime)s [%(levelname)s] %(message)s")

    def _stream_handler(stream, handler_level: int) -> logging.StreamHandler:
        handler = logging.StreamHandler(stream)
        handler.setLevel(handler_level)
        handler.setFormatter(log_format)
        return handler

    if stderr_only:
        root_logger.addHandler(_stream_handler(sys.stderr, level))
    else:
        stdout_handler = _stream_handler(sys.stdout, level)
        stdout_handler.addFilter(BelowLevelFilter(logging.ERROR))
        root_logger.addHandler(stdout_handler)
        root_logger.addHandler(_stream_handler(sys.stderr, logging.ERROR))


def emit_event(event_type: str, *, item_id: str | None = None, **payload) -> None:
    """Emit a machine-readable structured JSON-line to stdout for sidecar consumers when enabled."""
    if not _EVENTS_ENABLED:
        return
    ev = {"version": 1, "type": event_type}
    if item_id is not None:
        ev["id"] = item_id
    elif "id" in payload:
        ev["id"] = payload.pop("id")
    ev.update(payload)
    msg = json.dumps(ev, ensure_ascii=False)
    sys.stdout.write(f"@event: {msg}\n")
    sys.stdout.flush()


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


def is_font_attachment(file_name: str, content_type: str) -> bool:
    if Path(file_name).suffix.lower() in FONT_EXTENSIONS:
        return True
    ct_lower = content_type.lower()
    return any(pat in ct_lower for pat in ("font", "truetype", "opentype"))


# ─── SUBPROCESS & HASHING HELPERS ─────────────────────────────────────────────
def run_subprocess(cmd: list[str], *, capture_stdout: bool = True, timeout: int | None = None) -> subprocess.CompletedProcess[str]:
    """Run a subprocess with consistent UTF-8 encoding/error handling."""
    stdout_opt = subprocess.PIPE if capture_stdout else subprocess.DEVNULL
    return subprocess.run(
        cmd,
        stdout=stdout_opt,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
        check=False,
    )


def acquire_semaphore(sem: threading.Semaphore | None):
    """Return the semaphore as a context manager, or a no-op if None."""
    return sem if sem is not None else nullcontext()


def hash_file(filepath: Path) -> str:
    """Calculate BLAKE2b hash of a file for content-based deduplication."""
    with filepath.open("rb") as f:
        if hasattr(hashlib, "file_digest"):
            return hashlib.file_digest(f, hashlib.blake2b).hexdigest()  # type: ignore[attr-defined]
        hasher = hashlib.blake2b()
        while chunk := f.read(65536):
            hasher.update(chunk)
        return hasher.hexdigest()


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


# ─── FONT INFO & SCANNING ────────────────────────────────────────────────────
@dataclass(frozen=True)
class FontInfo:
    args:  tuple[str, ...]
    count: int


def scan_fonts(fonts_dir: Path) -> FontInfo:
    """Scan a fonts directory and build mkvmerge attachment arguments."""
    font_args: list[str] = []
    count = 0
    if not fonts_dir.is_dir():
        return FontInfo(args=(), count=0)

    try:
        entries = []
        with os.scandir(str(fonts_dir)) as it:
            for entry in it:
                if entry.is_file():
                    entries.append(entry)
        entries.sort(key=lambda e: natural_sort_key(e.name))
        for entry in entries:
            suffix = Path(entry.name).suffix.lower()
            mime = FONT_MIME.get(suffix)
            if not mime:
                continue
            font_args += ["--attachment-mime-type", mime, "--attach-file", entry.path]
            count += 1
    except OSError:
        pass
    return FontInfo(args=tuple(font_args), count=count)


_FONT_DIR_NAMES = frozenset({"fonts", "font", "attachments"})
_IGNORED_SCAN_DIR_NAMES = frozenset({"output", *_FONT_DIR_NAMES})


def find_fonts_dir(input_dir: Path, override: Path | None = None, fonts_dir_cache: dict[Path, Path] | None = None) -> Path:
    """
    Resolve which directory to use for fonts.
    Priority:
      1. override (e.g. from --fonts-dir)
      2. input_dir / "fonts"
      3. Any top-level directory matching fonts/font/attachments
      4. Falls back to input_dir / "fonts"
    """
    if override is not None:
        return override

    if fonts_dir_cache is not None and input_dir in fonts_dir_cache:
        return fonts_dir_cache[input_dir]

    def _store(path: Path) -> Path:
        if fonts_dir_cache is not None:
            fonts_dir_cache[input_dir] = path
        return path

    exact = input_dir / "fonts"
    if exact.is_dir():
        return _store(exact)

    if input_dir.is_dir():
        for entry in sorted(input_dir.iterdir()):
            if entry.is_dir() and entry.name.lower() in _FONT_DIR_NAMES:
                return _store(entry)

    return _store(exact)


# ─── FONT MANAGER (Thread-safe deduplication & atomic promotion) ─────────────
class FontHashMemo:
    """Persistent blake2b memo for font files keyed by (path, size, mtime_ns).

    Lets repeated runs skip re-reading and re-hashing unchanged fonts during
    seeding. Not thread-safe; intended for single-threaded seed passes.
    """

    def __init__(self, memo_file: Path) -> None:
        self.memo_file = memo_file
        self._data: dict[str, dict] = {}
        self._dirty = False
        self._load()

    def _load(self) -> None:
        if not self.memo_file.exists():
            return
        try:
            with open(self.memo_file, "r", encoding="utf-8") as f:
                loaded = json.load(f)
            if isinstance(loaded, dict):
                self._data = loaded
        except (OSError, json.JSONDecodeError, TypeError, ValueError) as e:
            logger.warning(f"Failed to load font hash memo from {self.memo_file}: {e}")
            self._data = {}

    def get(self, path: Path, st: os.stat_result) -> str | None:
        entry = self._data.get(str(path))
        if (
            isinstance(entry, dict)
            and entry.get("size") == st.st_size
            and entry.get("mtime_ns") == st.st_mtime_ns
        ):
            digest = entry.get("hash")
            if isinstance(digest, str):
                return digest
        return None

    def put(self, path: Path, st: os.stat_result, digest: str) -> None:
        entry = {"size": st.st_size, "mtime_ns": st.st_mtime_ns, "hash": digest}
        key = str(path)
        if self._data.get(key) != entry:
            self._data[key] = entry
            self._dirty = True

    def save(self) -> None:
        if not self._dirty:
            return
        temp_name = ""
        try:
            self.memo_file.parent.mkdir(parents=True, exist_ok=True)
            live_data = {key: val for key, val in self._data.items() if Path(key).exists()}
            try:
                with tempfile.NamedTemporaryFile(
                    "w", dir=self.memo_file.parent, encoding="utf-8", delete=False
                ) as temp_file:
                    temp_name = temp_file.name
                    json.dump(live_data, temp_file)
                Path(temp_name).replace(self.memo_file)
            except BaseException:
                if temp_name:
                    Path(temp_name).unlink(missing_ok=True)
                raise
            self._data = live_data
            self._dirty = False
        except (OSError, TypeError, ValueError) as e:
            logger.warning(f"Failed to save font hash memo to {self.memo_file}: {e}")


@dataclass
class FontClaim:
    name: str
    size: int
    staging_path: Path
    identity: tuple[str, int]
    _promoted: bool = False
    _released: bool = False


class FontManager:
    """
    Thread-safe manager configured with a fonts destination directory for font extraction,
    hash/name deduplication, collision safety, atomic promotion, and safe rollback.
    """

    def __init__(self, fonts_dir: Path, hash_memo: FontHashMemo | None = None) -> None:
        self.fonts_dir = fonts_dir
        self._hash_memo = hash_memo
        self._claimed_hashes: set[str] = set()
        self._claimed_name_size: set[tuple[str, int]] = set()
        self._staging_dir = self.fonts_dir / ".font_staging"
        self._lock = threading.Lock()
        self._seed_existing()

    def _seed_existing(self) -> None:
        if not self.fonts_dir.is_dir():
            return
        try:
            with os.scandir(str(self.fonts_dir)) as it:
                for entry in it:
                    try:
                        if not entry.is_file():
                            continue
                        p = Path(entry.path)
                        if p.suffix.lower() not in FONT_EXTENSIONS:
                            continue
                        st = entry.stat()
                        self._claimed_name_size.add((entry.name, st.st_size))
                        if st.st_size > 0:
                            file_hash = None
                            if self._hash_memo is not None:
                                file_hash = self._hash_memo.get(p, st)
                            if file_hash is None:
                                file_hash = hash_file(p)
                                if self._hash_memo is not None:
                                    self._hash_memo.put(p, st, file_hash)
                            self._claimed_hashes.add(file_hash)
                    except OSError:
                        pass
        except OSError:
            pass

    @property
    def claimed_hashes(self) -> set[str]:
        with self._lock:
            return set(self._claimed_hashes)

    def claim(self, name: str, size: int) -> FontClaim | None:
        """
        Atomically reserve a font by sanitized name and size.
        Returns a FontClaim with a manager-created same-filesystem staging path, or None if already claimed.
        """
        safe_name = safe_filename(name)
        key = (safe_name, size)
        with self._lock:
            if key in self._claimed_name_size:
                return None
            self._staging_dir.mkdir(parents=True, exist_ok=True)
            self._claimed_name_size.add(key)
            unique_id = uuid.uuid4().hex
            staging_path = self._staging_dir / f".tmp_{unique_id}_{safe_name}"

        return FontClaim(
            name=safe_name,
            size=size,
            staging_path=staging_path,
            identity=key,
        )

    def promote(self, claim: FontClaim) -> Path | None:
        """
        Validate, hash, deduplicate, and atomically promote a staged font file to fonts_dir.
        If hash is duplicate or validation fails, staging file is removed and released.
        Returns the final promoted destination Path, or None if duplicate or failed.
        """
        if claim._promoted:
            return None

        def _abandon_locked() -> None:
            try:
                claim.staging_path.unlink(missing_ok=True)
            except OSError:
                pass
            self._claimed_name_size.discard(claim.identity)
            claim._released = True

        try:
            if not claim.staging_path.is_file() or claim.staging_path.stat().st_size == 0:
                self.release(claim)
                return None
            file_hash = hash_file(claim.staging_path)
        except OSError:
            self.release(claim)
            return None

        with self._lock:
            if file_hash in self._claimed_hashes:
                _abandon_locked()
                return None

            try:
                self.fonts_dir.mkdir(parents=True, exist_ok=True)
                target_path = self.fonts_dir / claim.name
                if target_path.exists():
                    stem = target_path.stem
                    suffix = target_path.suffix
                    target_path = self.fonts_dir / f"{stem}_{file_hash[:8]}{suffix}"
                    counter = 1
                    while target_path.exists():
                        target_path = self.fonts_dir / f"{stem}_{file_hash[:8]}_{counter}{suffix}"
                        counter += 1
                claim.staging_path.replace(target_path)
                self._claimed_hashes.add(file_hash)
                claim._promoted = True
                return target_path
            except OSError as e:
                logger.error("Failed to promote font '%s': %s", claim.staging_path.name, e)
                _abandon_locked()
                return None

    def release(self, claim: FontClaim | None) -> None:
        """
        Idempotently release an uncommitted claim and clean up its staging path.
        Never removes committed/promoted files.
        """
        if claim is None or claim._promoted or claim._released:
            return

        with self._lock:
            self._claimed_name_size.discard(claim.identity)
            try:
                claim.staging_path.unlink(missing_ok=True)
            except OSError:
                pass
            claim._released = True

    def cleanup_staging(self) -> None:
        """Remove staging directory if empty."""
        with self._lock:
            if self._staging_dir.is_dir():
                try:
                    self._staging_dir.rmdir()
                except OSError:
                    pass


# ─── RUNTIME STATE TRACKERS ──────────────────────────────────────────────────
class StaleSubtitleTracker:
    """Thread-safe collector for active subtitle output variants with single-pass bulk purge."""
    def __init__(self) -> None:
        self._map: dict[str, str] = {}
        self._lock = threading.Lock()

    def record(self, base_stem: str, final_name: str) -> None:
        with self._lock:
            self._map[base_stem] = final_name

    def cleanup(self, mkv_dir: Path) -> None:
        with self._lock:
            items = dict(self._map)
        cleanup_stale_subtitle_offsets_bulk(mkv_dir, items)


# ─── SUBTITLE LANGUAGE DETECTION ─────────────────────────────────────────────
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
        if m.group(1): return "ind"
        if m.group(2): return "eng"
        if m.group(3): return "jpn"

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
    if m is None: return "00"
    remainder = name[m.end():]
    glued = re.match(r"\s*(\d{1,4}(?:\.\d{1,2}(?![0-9pib]))?)", remainder)
    if glued: 
        return format_ep(glued.group(1))
    
    m2 = _EP_NUM_MARKER_RE.search(remainder)
    if m2:
        val = _first_group(m2)
        if val is not None: return format_ep(val)
        
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
        return season, "OVA", _parse_typed_ep(name, _OVA_PATTERN)
    if _SPECIAL_PATTERN.search(name):
        return season, "SP", _parse_typed_ep(name, _SPECIAL_PATTERN)

    m = _SXXEXX_PATTERN.search(name)
    if m: return str(int(m.group(1))), "E", format_ep(m.group(2))

    m = _SXXPXX_PATTERN.search(name)
    if m: return str(int(m.group(1))), "P", format_ep(m.group(2))

    ep_m = _EP_NUM_MARKER_RE.search(name)
    if ep_m:
        val = _first_group(ep_m)
        if val is not None:
            return season, "E", format_ep(val)

    matches = _EP_NUM_FALLBACK_RE.findall(name)
    if matches:
        return season, "E", format_ep(matches[-1])

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


# ─── CHAPTER & COVER SNIFFING ────────────────────────────────────────────────
_CHAPTER_SNIFF_BYTES = 4096
_CHAPTER_TAG_RE = re.compile(rb"<Chapters[ >]", re.IGNORECASE)


@lru_cache(maxsize=4096)
def _chapter_xml_sniff(path_str: str, size: int, mtime_ns: int) -> bool:
    try:
        with open(path_str, "rb") as fh:
            head = fh.read(_CHAPTER_SNIFF_BYTES)
        return bool(_CHAPTER_TAG_RE.search(head))
    except OSError:
        return False


def is_chapter_xml(path: Path) -> bool:
    """Sniff whether a file is MKV chapter XML; memoized per (path, size,
    mtime) so repeated asset-resolution passes skip re-reading unchanged files."""
    try:
        st = path.stat()
    except OSError:
        return False
    return _chapter_xml_sniff(str(path), st.st_size, st.st_mtime_ns)


def has_chapters(json_data: dict | None) -> bool:
    if not json_data:
        return False
    chapters = json_data.get("chapters") or []
    return bool(chapters) and any((c.get("num_entries") or 0) > 0 for c in chapters)


def has_cover(json_data: dict | None) -> bool:
    if not json_data:
        return False
    attachments = json_data.get("attachments") or []
    cover, _ = parse_attachments(attachments)
    return cover is not None


_COVER_STEM_RE = re.compile(r"^cover$", re.IGNORECASE)
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


# ─── SUBTITLE TRACK SELECTION ────────────────────────────────────────────────
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


# ─── MKVMERGE & DEPENDENCY HELPERS ───────────────────────────────────────────
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
        result = run_subprocess(["mkvmerge", "-J", str(filepath)], timeout=300)
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


def _cache_base_dir() -> Path:
    local_app_data = os.environ.get("LOCALAPPDATA")
    if local_app_data:
        return Path(local_app_data) / "mkvanimebatch"
    return Path.home() / ".cache" / "mkvanimebatch"


def get_default_cache_file() -> Path:
    return _cache_base_dir() / "inspect_cache.json"


def get_default_font_hash_memo_file() -> Path:
    return _cache_base_dir() / "font_hash_memo.json"


class PersistentInspectCache:
    """Persistent JSON cache for mkvmerge inspection data keyed on (path, size, mtime)."""

    def __init__(self, cache_file: Path | None = None, enabled: bool = True) -> None:
        self.cache_file = cache_file or get_default_cache_file()
        self.enabled = enabled
        self._data: dict[str, dict] = {}
        self._dirty = False
        self._last_save_ts = 0.0
        self._lock = threading.Lock()
        if self.enabled:
            self._load()

    def _load(self) -> None:
        if not self.cache_file.exists():
            return
        try:
            with open(self.cache_file, "r", encoding="utf-8") as f:
                self._data = json.load(f)
        except (OSError, json.JSONDecodeError, TypeError, ValueError) as e:
            logger.warning(f"Failed to load inspect cache from {self.cache_file}: {e}")
            self._data = {}

    def save(self, *, min_interval: float = 0.0) -> None:
        """Atomically persist the cache to disk.

        Skips all disk work when nothing has been cached since the last
        successful save. With min_interval > 0, saves are debounced to at most
        one per interval; skipped saves stay dirty so a later call flushes
        them. Each actual write drops entries whose source file no longer
        exists (or holds no usable data) so the cache file cannot grow
        without bound.
        """
        if not self.enabled:
            return

        now = time.monotonic()
        with self._lock:
            if not self._dirty:
                return
            if min_interval > 0 and (now - self._last_save_ts) < min_interval:
                return
            data_copy = dict(self._data)

        temp_name = ""
        try:
            self.cache_file.parent.mkdir(parents=True, exist_ok=True)
            live_data = {
                key: entry
                for key, entry in data_copy.items()
                if Path(key).exists() and entry.get("json_data") is not None
            }
            try:
                with tempfile.NamedTemporaryFile(
                    "w", dir=self.cache_file.parent, encoding="utf-8", delete=False
                ) as temp_file:
                    temp_name = temp_file.name
                    json.dump(live_data, temp_file)
                # NamedTemporaryFile creates mode 0600; keep the previous
                # cache file's permissions (or a standard readable default)
                # so the cache stays usable across users of a shared directory.
                if self.cache_file.exists():
                    os.chmod(temp_name, stat.S_IMODE(self.cache_file.stat().st_mode))
                else:
                    os.chmod(temp_name, 0o644)
                Path(temp_name).replace(self.cache_file)
            except BaseException:
                if temp_name:
                    Path(temp_name).unlink(missing_ok=True)
                raise
            with self._lock:
                self._data = live_data
                self._dirty = False
                self._last_save_ts = time.monotonic()
        except (OSError, TypeError, ValueError) as e:
            logger.warning(f"Failed to save inspect cache to {self.cache_file}: {e}")

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
            cached_mtime = entry.get("mtime")
            cached_size = entry.get("size")
            if cached_mtime is not None and cached_size == st.st_size and abs(float(cached_mtime) - st.st_mtime) < 0.001:
                return entry.get("json_data")
        except (OSError, TypeError, ValueError):
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
                self._dirty = True
        except OSError:
            pass

    def clear(self) -> None:
        with self._lock:
            self._data.clear()
            self._dirty = False
            self._last_save_ts = 0.0
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
    """Run `mkvmerge -J` on every video in parallel and return metadata mapping.

    Populates `persistent_cache` with fresh results but does not persist it;
    callers batch multiple inspect passes per run and must call
    `persistent_cache.save()` themselves once the batch completes.
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
            exc = future.exception()
            if exc is not None:
                logger.warning("Inspection failed for %s: %s", v.name, exc)
                cache[v] = None
                continue
            _, data = future.result()
            cache[v] = data
            if persistent_cache:
                persistent_cache.put(v, data)

    ok = sum(1 for d in cache.values() if d is not None)
    logger.info("Inspection complete: %d/%d succeeded.", ok, len(videos))
    return cache


# ─── PARSED FILE & SCANNING ──────────────────────────────────────────────────
@dataclass(frozen=True)
class ParsedFile:
    path: Path
    name: str
    stem: str
    parsed: EpisodeInfo | None


def _create_parsed_file(path: Path) -> ParsedFile:
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
                        elif entry.is_dir(follow_symlinks=False) and recursive and name.lower() not in _IGNORED_SCAN_DIR_NAMES:
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


def _clone_scan_results(sr: ScanResults) -> ScanResults:
    return ScanResults(subs=list(sr.subs), chapters=list(sr.chapters), covers=list(sr.covers))


def scan_assets(dir_path: Path, scan_cache: dict[Path, ScanResults] | None = None) -> ScanResults:
    if scan_cache is not None:
        dir_path = dir_path.resolve()
        if dir_path in scan_cache:
            return _clone_scan_results(scan_cache[dir_path])

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

                    if suffix in SUBTITLE_EXTENSIONS:
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
    result = ScanResults(subs=subs, chapters=chapters, covers=covers)
    if scan_cache is not None:
        scan_cache[dir_path] = result
        return _clone_scan_results(result)
    return result


# ─── ASS SUBTITLE TIMING MANIPULATION ─────────────────────────────────────────
_ASS_TIME_RE = re.compile(r"^(\d+):(\d{2}):(\d{2})\.(\d{2})$")


def _ass_time_to_cs(t: str) -> int:
    """Parse ASS format time (H:MM:SS.cc) to centiseconds."""
    s_clean = t.strip()
    m = _ASS_TIME_RE.match(s_clean)
    if not m:
        raise ValueError(f"Bad ASS timestamp: {t!r}")
    h, mi, s, cs = map(int, m.groups())
    return ((h * 3600 + mi * 60 + s) * 100) + cs


def _ass_cs_to_time(total_cs: int) -> str:
    """Format centiseconds to ASS format time (H:MM:SS.cc) with clamping at 0."""
    total_cs = max(0, total_cs)
    total_s, cs = divmod(total_cs, 100)
    total_m, s = divmod(total_s, 60)
    h, m = divmod(total_m, 60)
    return f"{h}:{m:02d}:{s:02d}.{cs:02d}"


def shift_ass_content(content: str, offset_seconds: float) -> tuple[str, int]:
    """
    Shift every Dialogue/Comment line in ASS format text by offset_seconds in memory.
    Returns tuple of (shifted_content_str, shifted_line_count).
    """
    try:
        if offset_seconds == 0.0 or not (-360000.0 <= float(offset_seconds) <= 360000.0):
            return content, 0
    except (ValueError, TypeError):
        return content, 0

    offset_cs = round(offset_seconds * 100)
    lines = content.splitlines(keepends=True)

    out_lines: list[str] = []
    in_events = False
    shifted_count = 0

    for line in lines:
        stripped = line.rstrip("\r\n")
        lower = stripped.lower()

        if lower.startswith("["):
            in_events = lower.startswith("[events]")
            out_lines.append(line)
            continue

        if not in_events:
            out_lines.append(line)
            continue

        parts_colon = stripped.split(":", 1)
        if len(parts_colon) == 2 and parts_colon[0].strip().lower() in ("dialogue", "comment"):
            prefix, rest = parts_colon
            parts = rest.split(",", 9)
            if len(parts) >= 10:
                try:
                    start_cs = _ass_time_to_cs(parts[1]) + offset_cs
                    end_cs = _ass_time_to_cs(parts[2]) + offset_cs
                    parts[1] = _ass_cs_to_time(start_cs)
                    parts[2] = _ass_cs_to_time(end_cs)
                    newline = line[len(stripped):]
                    out_lines.append(f"{prefix}:{','.join(parts)}{newline}")
                    shifted_count += 1
                    continue
                except (ValueError, KeyError, IndexError):
                    pass

        out_lines.append(line)

    return "".join(out_lines), shifted_count


def shift_ass_timestamps(path: Path, offset_seconds: float, log: list[str] | None = None) -> int:
    """Shift every Dialogue/Comment line in an .ass file by offset_seconds (in place)."""
    if offset_seconds == 0.0:
        return 0

    try:
        data = path.read_bytes()
        has_bom = data.startswith(b"\xef\xbb\xbf")
        raw = data.decode("utf-8-sig")
    except (OSError, UnicodeDecodeError) as e:
        if log is not None:
            log.append(f"   -> ERROR (sub shift): Failed to read subtitle file: {e}")
        return 0

    shifted_text, shifted_count = shift_ass_content(raw, offset_seconds)

    if shifted_count > 0:
        try:
            encoding = "utf-8-sig" if has_bom else "utf-8"
            path.write_text(shifted_text, encoding=encoding, newline="")
            if log is not None:
                log.append(f"   -> Shifted subtitle timing by {offset_seconds:+.2f}s ({shifted_count} lines)")
        except OSError as e:
            if log is not None:
                log.append(f"   -> ERROR (sub shift): Failed to write shifted subtitle: {e}")

    return shifted_count


_OFFSET_SUBTITLE_RE = re.compile(
    r"^(?P<stem>.+?)(?:\.\[[-+]?\d+(?:\.\d+)?s\])?(?P<suffix>\.ass)$",
    re.IGNORECASE,
)


def cleanup_stale_subtitle_offsets_bulk(mkv_dir: Path, active_subs: dict[str, str]) -> None:
    """Single directory pass that removes stale subtitle variants for known stems."""
    if not active_subs:
        return
    normalized_active_subs: dict[str, set[str]] = {}
    for stem, expected_name in active_subs.items():
        normalized_active_subs.setdefault(stem.casefold(), set()).add(expected_name)
    try:
        with os.scandir(str(mkv_dir)) as it:
            for entry in it:
                if not entry.is_file():
                    continue
                match = _OFFSET_SUBTITLE_RE.match(entry.name)
                if not match:
                    continue
                stem = match.group("stem").casefold()
                expected_names = normalized_active_subs.get(stem)
                if not expected_names:
                    continue
                entry_path = Path(entry.path)
                is_expected_file = False
                for expected_name in expected_names:
                    expected_path = mkv_dir / expected_name
                    try:
                        if entry_path.samefile(expected_path):
                            is_expected_file = True
                            break
                    except OSError:
                        if entry.name == expected_name:
                            is_expected_file = True
                            break
                if not is_expected_file:
                    entry_path.unlink(missing_ok=True)
    except OSError:
        pass
