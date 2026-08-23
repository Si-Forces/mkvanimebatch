"""
mkv_scan.py
MKV inspection probing (`mkvmerge -J`) with a persistent probe cache,
attachment/chapter/cover classification, and release-directory scanning.
"""
from __future__ import annotations

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
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

from mkv_domain import (
    CHAPTER_EXT,
    COVER_EXTENSIONS,
    FONT_EXTENSIONS,
    SUBTITLE_EXTENSIONS,
    EpisodeInfo,
    natural_sort_key,
    parse_episode,
)
from mkv_fonts import _FONT_DIR_NAMES
from mkv_proc import CancelToken, run_subprocess_cancellable

_IGNORED_SCAN_DIR_NAMES = frozenset({"output", *_FONT_DIR_NAMES})

logger = logging.getLogger("mkv_automation")

# Inspection (`mkvmerge -J`) only parses container headers, so it gets a much
# shorter ceiling than SUBPROCESS_TIMEOUT to keep unresponsive files from
# stalling a whole batch.
INSPECT_TIMEOUT = 300

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

def is_font_attachment(file_name: str, content_type: str) -> bool:
    if Path(file_name).suffix.lower() in FONT_EXTENSIONS:
        return True
    ct_lower = content_type.lower()
    return any(pat in ct_lower for pat in ("font", "truetype", "opentype"))


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

def check_dependencies(*required: str) -> None:
    for cmd in required:
        if not shutil.which(cmd):
            logger.error("Required dependency '%s' not found in PATH.", cmd)
            sys.exit(1)


def run_mkvmerge_json(filepath: Path, log: list[str] | None = None, cancel_token: CancelToken | None = None) -> dict | None:
    def _warn(msg: str):
        if log is not None:
            log.append(f"  WARNING: {msg}")
        else:
            logger.warning(msg)

    try:
        result = run_subprocess_cancellable(
            ["mkvmerge", "-J", str(filepath)],
            cancel_token=cancel_token,
            timeout=INSPECT_TIMEOUT,
        )
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
        self._revision = 0
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

        Concurrent puts() that land while the serialized snapshot is being
        written are preserved: entries pruned by this save are removed from
        memory only when unchanged since the snapshot, and the dirty flag is
        only cleared when no mutation happened during the write.
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
            saved_revision = self._revision

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
                # Prune entries dropped during pruning of the snapshot, but
                # only when they were not re-put (with fresh data) meanwhile.
                for key in data_copy.keys() - live_data.keys():
                    if self._data.get(key) == data_copy.get(key):
                        self._data.pop(key, None)
                # A put() during the write bumped the revision; its entry must
                # not be lost, and the cache stays dirty for a later flush.
                if self._revision == saved_revision:
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
                self._revision += 1
        except OSError:
            pass

    def clear(self) -> None:
        with self._lock:
            self._data.clear()
            self._dirty = False
            self._revision = 0
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
    cancel_token: CancelToken | None = None,
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
            return v, run_mkvmerge_json(v, cancel_token=cancel_token)

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
