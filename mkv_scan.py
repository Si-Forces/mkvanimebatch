"""
mkv_scan.py
MKV inspection probing (`mkvmerge -J`) with a persistent probe cache,
attachment/chapter/cover classification, and release-directory scanning.
"""
from __future__ import annotations

import concurrent.futures
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
from collections.abc import Mapping
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import TypedDict, cast

from mkv_domain import (
    CHAPTER_EXT,
    COVER_EXTENSIONS,
    FONT_EXTENSIONS,
    SUBTITLE_EXTENSIONS,
    EpisodeInfo,
    MkvTrack,
    TrackProperties,
    natural_sort_key,
    parse_episode,
)
from mkv_fonts import FONT_DIR_NAMES
from mkv_proc import (
    CancelledError,
    CancelToken,
    acquire_semaphore,
    run_subprocess_cancellable,
    submit_bounded,
)

_IGNORED_SCAN_DIR_NAMES = frozenset({"output", *FONT_DIR_NAMES})

logger = logging.getLogger("mkv_automation")




class MkvAttachment(TypedDict, total=False):
    id: int
    file_name: str
    content_type: str
    size: int


class MkvChapter(TypedDict, total=False):
    num_entries: int


class MkvmergeMetadata(TypedDict, total=False):
    tracks: list[MkvTrack]
    attachments: list[MkvAttachment]
    chapters: list[MkvChapter]


class AudioTrack(TypedDict):
    id: int | None
    pos: int
    audio_idx: int
    lang: str
    name: str
    channels: int
    codec: str


class InspectCacheEntry(TypedDict, total=False):
    size: int
    mtime: float
    json_data: MkvmergeMetadata | None

# Inspection (`mkvmerge -J`) only parses container headers, so it gets a much
# shorter ceiling than SUBPROCESS_TIMEOUT to keep unresponsive files from
# stalling a whole batch.
INSPECT_TIMEOUT = 300

def parse_attachments(
    attachments: list[MkvAttachment],
) -> tuple[MkvAttachment | None, list[MkvAttachment]]:
    """Parse attachments once, returning the cover and fonts."""
    cover: MkvAttachment | None = None
    fonts: list[MkvAttachment] = []
    for attachment in attachments:
        file_name = attachment.get("file_name")
        if not file_name:
            continue
        content_type = attachment.get("content_type") or ""
        if cover is None and is_cover_attachment(file_name, content_type):
            cover = attachment
        if is_font_attachment(file_name, content_type):
            fonts.append(attachment)
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


class InspectedVideo:
    """Typed view over one video's raw mkvmerge -J JSON.

    Wraps the probed dict verbatim — persistent-cache format is unchanged —
    and normalizes mkvmerge's schema access (.get chains) in one place, so
    consumers never re-learn the wire shape. Audio tracks are exposed in the
    normalized {id, pos, audio_idx, lang, name, channels, codec} form that
    track selection scores.
    """

    def __init__(self, data: Mapping[str, object]) -> None:
        self._data = cast(MkvmergeMetadata, dict(data))

    def __eq__(self, other: object) -> bool:
        return isinstance(other, InspectedVideo) and self._data == other._data

    def __repr__(self) -> str:
        return f"InspectedVideo({self._data!r})"

    @property
    def tracks(self) -> list[MkvTrack]:
        return self._data.get("tracks", [])

    @property
    def attachments(self) -> list[MkvAttachment]:
        return self._data.get("attachments", [])

    @property
    def has_attachments_key(self) -> bool:
        """True when the probe reported an attachments key at all (even empty)."""
        return "attachments" in self._data

    @property
    def video_track(self) -> MkvTrack | None:
        return next((t for t in self.tracks if t.get("type") == "video"), None)

    @property
    def video_track_pos(self) -> int:
        return next((i for i, t in enumerate(self.tracks) if t.get("type") == "video"), -1)

    def audio_tracks(self) -> list[AudioTrack]:
        normalized: list[AudioTrack] = []
        for position, track in enumerate(self.tracks):
            if track.get("type") != "audio":
                continue
            properties: TrackProperties = track.get("properties", {})
            normalized.append({
                "id": track.get("id"),
                "pos": position,
                "audio_idx": len(normalized),
                "lang": properties.get("language") or "",
                "name": properties.get("track_name") or "",
                "channels": properties.get("audio_channels") or 0,
                "codec": (properties.get("codec_id") or "").upper(),
            })
        return normalized

    def subtitle_tracks(self) -> list[MkvTrack]:
        return [t for t in self.tracks if t.get("type") == "subtitles"]

    def has_chapters(self) -> bool:
        """True when the probe reported chapters with at least one entry."""
        chapters = self._data.get("chapters") or []
        return bool(chapters) and any((c.get("num_entries") or 0) > 0 for c in chapters)

    def cover_attachment(self) -> MkvAttachment | None:
        cover, _ = parse_attachments(self.attachments)
        return cover

    def font_attachments(self) -> list[MkvAttachment]:
        _, fonts = parse_attachments(self.attachments)
        return fonts

    def fonts_count(self) -> int | None:
        """Number of font attachments; None when the probe reported no
        attachments key (indistinguishable from a failed probe upstream)."""
        return len(self.font_attachments()) if self.has_attachments_key else None


@dataclass(frozen=True)
class InspectionResult:
    """One completed probe: typed metadata or the reason it failed."""

    video: InspectedVideo | None
    error: str | None = None

    @classmethod
    def success(cls, data: Mapping[str, object]) -> InspectionResult:
        return cls(video=InspectedVideo(data))

    @classmethod
    def failure(cls, error: str) -> InspectionResult:
        return cls(video=None, error=error)


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


class InspectionError(Exception):
    """Expected mkvmerge probe failure with a user-facing reason."""


def run_mkvmerge_json(
    filepath: Path,
    log: list[str] | None = None,
    cancel_token: CancelToken | None = None,
) -> MkvmergeMetadata:
    def _warn(message: str) -> None:
        if log is not None:
            log.append(f"  WARNING: {message}")
        else:
            logger.warning(message)

    try:
        result = run_subprocess_cancellable(
            ["mkvmerge", "-J", str(filepath)],
            cancel_token=cancel_token,
            timeout=INSPECT_TIMEOUT,
        )
    except subprocess.TimeoutExpired as exc:
        message = f"mkvmerge inspection timed out for '{filepath.name}'."
        _warn(message)
        raise InspectionError(message) from exc
    if result.stderr.strip():
        _warn(f"(mkvmerge) {result.stderr.strip()}")
    if result.returncode > 1:
        message = f"mkvmerge failed with exit {result.returncode} for '{filepath.name}'."
        _warn(message)
        raise InspectionError(message)
    if not result.stdout.strip():
        message = f"No metadata returned for '{filepath.name}'."
        _warn(message)
        raise InspectionError(message)
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        message = f"Failed to parse metadata for '{filepath.name}': {exc}"
        _warn(message)
        raise InspectionError(message) from exc
    if not isinstance(data, dict):
        message = f"Invalid metadata document returned for '{filepath.name}'."
        _warn(message)
        raise InspectionError(message)
    return cast(MkvmergeMetadata, data)


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
        self._data: dict[str, InspectCacheEntry] = {}
        self._dirty = False
        self._revision = 0
        self._last_save_ts = 0.0
        self._lock = threading.Lock()
        # Memoized path.resolve() results: probing caches are consulted once
        # per video per pass, and realpath syscalls add up across passes.
        self._resolved_keys: dict[str, str] = {}
        if self.enabled:
            self._load()

    def _cache_key(self, path: Path) -> str:
        """Stable on-disk identity for a video path, realpath-resolved once."""
        raw = str(path)
        with self._lock:
            key = self._resolved_keys.get(raw)
        if key is not None:
            return key
        try:
            key = str(path.resolve())
        except OSError:
            key = raw
        with self._lock:
            return self._resolved_keys.setdefault(raw, key)

    def _load(self) -> None:
        if not self.cache_file.exists():
            return
        try:
            with open(self.cache_file, "r", encoding="utf-8") as file:
                loaded: object = json.load(file)
            if not isinstance(loaded, dict):
                raise ValueError("cache root must be an object")
            self._data = cast(dict[str, InspectCacheEntry], loaded)
        except (OSError, json.JSONDecodeError, TypeError, ValueError) as e:
            logger.warning(f"Failed to load inspect cache from {self.cache_file}: {e}")
            self._data = {}

    def save(self, *, min_interval: float = 0.0) -> None:
        """Atomically persist the cache to disk.

        Skips all disk work when nothing has been cached since the last
        successful save. With min_interval > 0, saves are debounced to at most
        one per interval; skipped saves stay dirty so a later call flushes
        them. Saves never stat historical source paths; stale entries are
        evicted lazily by get() when their source vanishes or changes.

        Concurrent puts() that land while the serialized snapshot is being
        written are preserved: the dirty flag is only cleared when no mutation
        happened during the write.
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
                if entry.get("json_data") is not None
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
                # Drop entries the snapshot filtered out (no usable data), but
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

    def get(self, path: Path) -> MkvmergeMetadata | None:
        if not self.enabled:
            return None
        key = self._cache_key(path)
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
        # Source vanished or changed: self-heal so dead entries never
        # accumulate (save() no longer prunes). Skip when a concurrent put()
        # already refreshed the entry.
        with self._lock:
            if self._data.get(key) == entry:
                del self._data[key]
                self._dirty = True
                self._revision += 1
        return None

    def put(self, path: Path, json_data: Mapping[str, object]) -> None:
        if not self.enabled:
            return
        key = self._cache_key(path)
        try:
            st = path.stat()
            with self._lock:
                self._data[key] = {
                    "size": st.st_size,
                    "mtime": st.st_mtime,
                    "json_data": cast(MkvmergeMetadata, dict(json_data)),
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
    probe_semaphore: threading.Semaphore | None = None,
) -> dict[Path, InspectionResult]:
    """Probe videos in parallel, preserving failures as explicit results.

    Each worker validates the persistent cache itself, so cache hits and
    probe misses start concurrently without a synchronous pre-pass. Only the
    mkvmerge probe runs under probe_semaphore; cache validation stays wide.
    Only successful raw metadata is cached. Callers own persistence and
    should invoke ``persistent_cache.save()`` after their batch completes.
    """
    results: dict[Path, InspectionResult] = {}

    if not videos:
        return results

    logger.info("Pre-inspecting %d video(s)...", len(videos))

    active_token = cancel_token or CancelToken()

    def _inspect(video: Path) -> tuple[Path, MkvmergeMetadata, bool]:
        if active_token.cancelled:
            raise CancelledError("Cancellation requested before probe")
        if persistent_cache and not force_refresh:
            cached_data = persistent_cache.get(video)
            if cached_data is not None:
                return video, cached_data, True
        with acquire_semaphore(probe_semaphore, active_token):
            return video, run_mkvmerge_json(video, cancel_token=active_token), False

    cached_count = 0
    cancelled = False

    try:
        with submit_bounded(
            pool,
            _inspect,
            videos,
            cancel_token=active_token,
        ) as completions:
            for video, future in completions:
                try:
                    _, data, from_cache = future.result()
                except (CancelledError, concurrent.futures.CancelledError):
                    cancelled = True
                    active_token.request()
                except InspectionError as exc:
                    results[video] = InspectionResult.failure(str(exc))
                except Exception as exc:
                    logger.warning("Inspection failed for %s: %s", video.name, exc)
                    results[video] = InspectionResult.failure(f"Inspection failed: {exc}")
                else:
                    results[video] = InspectionResult.success(data)
                    if from_cache:
                        cached_count += 1
                    elif persistent_cache:
                        persistent_cache.put(video, data)
    except CancelledError:
        cancelled = True

    if cancelled or active_token.cancelled:
        raise CancelledError("Cancellation requested during video inspection")

    succeeded = sum(1 for result in results.values() if result.video is not None)
    if cached_count:
        logger.info("Inspection complete: %d/%d succeeded (%d from cache).", succeeded, len(videos), cached_count)
    else:
        logger.info("Inspection complete: %d/%d succeeded.", succeeded, len(videos))
    return results


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
    results: list[ParsedFile] = []
    stack: list[str] = [str(dir_path)]
    resolved_ignores: set[Path] = {p.resolve() for p in ignore_paths} if ignore_paths else set()

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

    subs: list[ParsedFile] = []
    chapters: list[ParsedFile] = []
    covers: list[ParsedFile] = []
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
