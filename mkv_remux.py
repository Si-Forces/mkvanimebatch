#!/usr/bin/env python3
"""
Batch MKV Re-Mux Utility
Optimized for anime & Indonesian fansub releases — including OVA, OAD, ONA, SP.

Recursively finds MKVs under --input-dir and remuxes each with its best-
matching subtitle (external .ass, embedded fallback, or interactive pick),
chapter XML, and cover image — searched from the video's own folder up to
--input-dir, so a shared season/show-level folder is found automatically.
Prefers stereo Japanese audio, converting FLAC to Opus via ffmpeg. By
default, a local "fonts" folder is deleted after a successful run (pass
--keep-fonts to keep it). Use --dry-run to preview without touching disk.

Requires: mkvmerge (MKVToolNix), ffmpeg
"""
from __future__ import annotations

import sys
sys.dont_write_bytecode = True

import os
import re
import shutil
import tempfile
import argparse
import uuid
import logging
import threading
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path

from mkv_common import (
    CHAPTER_EXT,
    COVER_EXTENSIONS,
    COVER_MIME,
    FONT_MIME,
    LANG_LABEL,
    LANG_MKV_TAG,
    SUBTITLE_EXT,
    SUBTITLE_LANGS_FALLBACK,
    SUBTITLE_LANGS_PRIMARY,
    acquire_semaphore,
    check_dependencies,
    detect_sub_lang,
    ep_type_label,
    parse_attachments,
    find_fonts_dir,
    has_chapters,
    run_subprocess,
    inspect_videos,
    PersistentInspectCache,
    is_chapter_xml,
    is_cover_filename,
    is_dialogue_track,
    output_stem,
    parse_episode,
    select_best_subtitle_track,
    setup_logging,
    natural_sort_key,
    ParsedFile,
    ScanResults,
    scan_mkv_files,
    scan_assets,
    resolve_output_dir,
    EpisodeInfo,
    MAX_DEFAULT_WORKERS,
    DEFAULT_WORKER_MULTIPLIER,
)

SCRIPT_DIR = Path(__file__).resolve().parent

# Initialize child logger for the remux module
logger = logging.getLogger("mkv_automation.remux")


# ─── TRACK SELECTOR ──────────────────────────────────────────────────────────
@dataclass
class TrackSelection:
    video_id:            int | None
    audio_id:            int | None
    audio_index:         int
    audio_codec:         str
    audio_channels:      int
    audio_lang:          str
    audio_name:          str
    audio_reason:        str
    has_source_chapters: bool
    source_cover_id:     int | None
    source_font_ids:     list[int] = field(default_factory=list)


def select_tracks(json_data: dict) -> TrackSelection:
    tracks           = json_data.get("tracks") or []
    has_src_chapters = has_chapters(json_data)
    attachments      = json_data.get("attachments") or []

    cover, font_attachments = parse_attachments(attachments)
    source_cover_id = int(cover["id"]) if cover and cover.get("id") is not None else None
    source_font_ids = [int(a["id"]) for a in font_attachments if a.get("id") is not None]

    video_id  = None
    video_pos = -1
    audio_tracks: list[dict] = []
    audio_idx_counter = 0

    for i, t in enumerate(tracks):
        if t.get("type") == "video" and video_id is None:
            video_id  = t.get("id")
            video_pos = i
        elif t.get("type") == "audio":
            p = t.get("properties") or {}
            audio_tracks.append({
                "id":       t.get("id"),
                "pos":      i,
                "audio_idx": audio_idx_counter,
                "lang":     p.get("language") or "",
                "name":     p.get("track_name") or "",
                "channels": p.get("audio_channels") or 0,
                "codec":    (p.get("codec_id") or "").upper(),
            })
            audio_idx_counter += 1

    audio, reason = _pick_audio(audio_tracks, video_pos)

    return TrackSelection(
        video_id=video_id,
        audio_id=audio["id"] if audio else None,
        audio_index=audio["audio_idx"] if audio else 0,
        audio_codec=audio["codec"] if audio else "",
        audio_channels=audio["channels"] if audio else 0,
        audio_lang=audio["lang"] if audio else "",
        audio_name=audio["name"] if audio else "",
        audio_reason=reason,
        has_source_chapters=has_src_chapters,
        source_cover_id=source_cover_id,
        source_font_ids=source_font_ids,
    )


_COMMENTARY_RE = re.compile(r"\b(commentary|comment|audio\s+description|ad|extension)\b", re.IGNORECASE)


def _pick_audio(audio_tracks: list[dict], video_pos: int) -> tuple[dict | None, str]:
    """Select best audio track using a priority scoring system, excluding commentary/description."""
    if not audio_tracks:
        return None, ""

    # Separate into primary (no commentary) and fallback (commentary allowed) pools
    primary_tracks = [a for a in audio_tracks if not _COMMENTARY_RE.search(a["name"])]
    if primary_tracks:
        pool_to_use = primary_tracks
        is_fallback_pool = False
    else:
        pool_to_use = audio_tracks
        is_fallback_pool = True

    candidates = []
    for a in pool_to_use:
        is_ja = a["lang"].startswith(("ja", "jpn"))
        is_und = (a["lang"] == "und")
        is_stereo = (a["channels"] == 2)
        is_diag = is_dialogue_track(a["name"])

        score = 0
        if is_ja and is_stereo and is_diag:
            score = 10
        elif is_ja and is_stereo:
            score = 9
        elif is_ja:
            score = 8
        elif is_und and is_stereo and is_diag:
            score = 7
        elif is_und and is_stereo:
            score = 6
        elif is_und:
            score = 5
        elif is_stereo:
            score = 4
        elif a["pos"] > video_pos:
            score = 3

        if score == 0:
            score = 2 if is_ja else 1

        priority_key = (score, -a["pos"])
        candidates.append((priority_key, a))

    best_candidate = max(candidates, key=lambda c: c[0])
    priority_key, best_track = best_candidate
    score = priority_key[0]

    if is_fallback_pool:
        if best_track["lang"].startswith(("ja", "jpn")):
            return best_track, "Japanese audio fallback (commentary-possible)"
        return best_track, "First audio (last resort)"

    reasons = {
        10: "Japanese stereo (dialogue-priority)",
        9: "Japanese stereo",
        8: "Japanese multi-channel (no stereo available)",
        7: "Undetermined stereo (dialogue-priority, likely Japanese)",
        6: "Undetermined stereo (likely Japanese)",
        5: "Undetermined multi-channel (likely Japanese)",
        4: "Stereo fallback (no Japanese audio)",
        3: "First audio after video",
        2: "Japanese audio fallback (commentary-possible)",
        1: "First audio (last resort)",
    }
    return best_track, reasons.get(score, "Selected audio")


# ─── HELPERS ─────────────────────────────────────────────────────────────────
def log_skip(video_name: str, reason: str) -> None:
    logger.warning("SKIPPED: %s | Reason: %s", video_name, reason)


def seasons_match(mkv_season: str, sub_season: str) -> bool:
    if not mkv_season or not sub_season:
        return True
    return mkv_season == sub_season


def mkv_success(result) -> bool:
    return result.returncode <= 1


def is_completed_output(path: Path) -> bool:
    """Return whether a remux output exists and contains data."""
    return path.is_file() and path.stat().st_size > 0



def _ancestor_dirs(mkv_dir: Path, root_input_dir: Path) -> list[Path]:
    """
    Return every directory on the path from root_input_dir down to mkv_dir,
    inclusive of both endpoints.  If mkv_dir is not under root_input_dir the
    list contains only mkv_dir.

    Example: root=Root/, mkv_dir=Root/Season1/Arc/ → [Root/, Root/Season1/, Root/Season1/Arc/]
    """
    try:
        rel = mkv_dir.relative_to(root_input_dir)
    except ValueError:
        return [mkv_dir]
    dirs: list[Path] = []
    current = root_input_dir
    dirs.append(current)
    for part in rel.parts:
        current = current / part
        dirs.append(current)
    return dirs


# ─── FONT SCANNING ───────────────────────────────────────────────────────────
@dataclass(frozen=True)
class FontInfo:
    args:  tuple[str, ...]
    count: int


def scan_fonts(fonts_dir: Path) -> FontInfo:
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


# ─── INDEX CLASSES (Subtitles, Chapters, Covers) ─────────────────────────────
class SubIndex:
    def __init__(self) -> None:
        self._index: dict[str, list[ParsedFile]] = {}
        self._seen: set[tuple[str, str, str, str]] = set()

    def add(self, sub_file: ParsedFile) -> None:
        if not sub_file.parsed:
            return
        season, ep_type, ep = sub_file.parsed
        lang = detect_sub_lang(sub_file.stem)
        suffix = sub_file.path.suffix.lower()
        key = f"{ep_type}|{ep}"
        seen_key = (key, season, lang, suffix)

        if seen_key in self._seen:
            for f in self._index.get(key, []):
                if f.parsed[0] == season and detect_sub_lang(f.stem) == lang and f.path.suffix.lower() == suffix:
                    if f.path.parent == sub_file.path.parent:
                        logger.warning("Duplicate subtitle for key %s (lang %s, suffix %s) in '%s': keeping '%s', ignoring '%s'", key, lang, suffix, sub_file.path.parent.name, f.name, sub_file.name)
                    break
            return

        self._seen.add(seen_key)
        self._index.setdefault(key, []).append(sub_file)

    def lookup(self, ep_type: str, ep: str, mkv_season: str) -> tuple[Path, str] | None:
        key = f"{ep_type}|{ep}"
        candidates = []
        for f in self._index.get(key, ()):
            if seasons_match(mkv_season, f.parsed[0]):
                candidates.append(f)

        if not candidates:
            return None

        # Build mock tracks list for select_best_subtitle_track
        tracks = []
        for idx, f in enumerate(candidates):
            lang = detect_sub_lang(f.stem)
            codec_id = "s_text/ass" if f.path.suffix.lower() == ".ass" else "s_text/utf8"
            tracks.append({
                "id": idx,
                "type": "subtitles",
                "properties": {
                    "codec_id": codec_id,
                    "language": lang,
                    "track_name": f.stem,
                }
            })

        result = select_best_subtitle_track(tracks)
        if result is not None:
            idx, lang, _ = result
            return candidates[idx].path, lang

        return None

    @property
    def indexed_count(self) -> int: return sum(len(lst) for lst in self._index.values())

    def _count_by_lang(self, lang_set: frozenset[str]) -> int:
        return sum(
            1
            for lst in self._index.values()
            for f in lst
            if detect_sub_lang(f.stem) in lang_set
        )

    @property
    def primary_count(self) -> int:
        return self._count_by_lang(SUBTITLE_LANGS_PRIMARY)

    @property
    def fallback_count(self) -> int:
        return self._count_by_lang(SUBTITLE_LANGS_FALLBACK)


class ChapterIndex:
    def __init__(self) -> None:
        self._index:     dict[str, list[tuple[Path, str]]] = {}
        self._unmatched: list[Path] = []
        self._rejected = 0

    def add(self, xml_file: ParsedFile) -> None:
        if not is_chapter_xml(xml_file.path):
            self._rejected += 1
            return
        parsed = xml_file.parsed
        if parsed:
            season, ep_type, ep = parsed
            key = f"{ep_type}|{ep}"
            existing = self._index.setdefault(key, [])
            for path, s in existing:
                if s == season:
                    if path.parent == xml_file.path.parent:
                        logger.warning("Duplicate chapter file for key %s in '%s': keeping '%s', ignoring '%s'", key, xml_file.path.parent.name, path.name, xml_file.name)
                    return
            existing.append((xml_file.path, season))
        else:
            self._unmatched.append(xml_file.path)

    def lookup(self, ep_type: str, ep: str, mkv_season: str) -> Path | None:
        key = f"{ep_type}|{ep}"
        if key in self._index:
            for path, season in self._index[key]:
                if seasons_match(mkv_season, season):
                    return path
        return None

    def has_match(self, ep_type: str, ep: str, season: str) -> bool:
        key = f"{ep_type}|{ep}"
        if key not in self._index:
            return False
        return any(seasons_match(season, s) for _, s in self._index[key])

    @property
    def indexed_count(self) -> int:  return sum(len(lst) for lst in self._index.values())
    @property
    def rejected_count(self) -> int: return self._rejected
    @property
    def unmatched(self) -> list[Path]: return list(self._unmatched)


_COVER_FILE_RE = re.compile(r'^(?P<stem>.+)_cover$', re.IGNORECASE)


class CoverIndex:
    def __init__(self) -> None:
        self._index: dict[str, list[tuple[Path, str]]] = {}
        self._global: Path | None = None

    def add(self, image_file: ParsedFile) -> None:
        stem = image_file.stem
        is_global = False
        parsed = None

        if is_cover_filename(image_file.name):
            is_global = True
        else:
            m = _COVER_FILE_RE.match(stem)
            if m:
                parsed = parse_episode(m.group("stem"))
                if not parsed:
                    is_global = True
            else:
                return

        if is_global:
            if self._global is None:
                self._global = image_file.path
            elif self._global.parent == image_file.path.parent:
                logger.warning("Duplicate global cover in '%s': keeping '%s', ignoring '%s'", image_file.path.parent.name, self._global.name, image_file.name)
            return

        season, ep_type, ep = parsed
        key = f"{ep_type}|{ep}"
        existing = self._index.setdefault(key, [])
        for path, s in existing:
            if s == season:
                if path.parent == image_file.path.parent:
                    logger.warning("Duplicate cover image for key %s in '%s': keeping '%s', ignoring '%s'", key, image_file.path.parent.name, path.name, image_file.name)
                return
        existing.append((image_file.path, season))

    def lookup(self, ep_type: str, ep: str, mkv_season: str) -> Path | None:
        key = f"{ep_type}|{ep}"
        if key in self._index:
            for path, season in self._index[key]:
                if seasons_match(mkv_season, season):
                    return path
        return self._global

    @property
    def indexed_count(self) -> int: return sum(len(lst) for lst in self._index.values())
    @property
    def has_global(self) -> bool:   return self._global is not None


# ─── EMBEDDED SUBTITLE SELECTION ─────────────────────────────────────────────
def select_embedded_subtitle(json_data: dict) -> tuple[int, str] | None:
    result = select_best_subtitle_track(json_data.get("tracks", []))
    if result is None:
        return None
    tid, lang, _ = result
    return tid, lang


# ─── JOB PLANNING ────────────────────────────────────────────────────────────
@dataclass
class PlannedJob:
    video:              Path
    video_name:         str
    base:               str
    output_file:        Path
    subtitle_file:      Path | None
    subtitle_label:     str
    subtitle_mkv_lang:  str
    chapter_file:       Path | None
    cover_file:         Path | None
    embedded_sub_id:    int | None  = None
    use_source_fonts:   bool        = False


@dataclass
class JobResult:
    log:            list[str] = field(default_factory=list)
    skipped_reason: str | None = None


# ─── AUDIO CONVERSION ────────────────────────────────────────────────────────
def _convert_flac_to_opus(
    video: Path, audio_id: int, audio_index: int, temp_dir: Path,
    base: str, opus_bitrate: str, log: list[str], ffmpeg_threads: int,
    complexity: int = 10,
    io_semaphore: threading.Semaphore | None = None,
) -> Path | None:
    """Convert FLAC audio to Opus. Returns temp opus path or None on failure."""
    unique_id = uuid.uuid4().hex[:8]
    temp_opus = temp_dir / f"{base}_{unique_id}_audio.opus"
    log.append(f"  FLAC detected — converting to Opus {opus_bitrate} via ffmpeg...")

    cmd = [
        "ffmpeg", "-y", "-nostdin",
        "-analyzeduration", "0", "-probesize", "1M",
        "-threads", str(ffmpeg_threads),
        "-i", str(video),
        "-map", f"0:a:{audio_index}",
        "-vn", "-c:a", "libopus",
        "-b:a", opus_bitrate,
        "-vbr", "on",
        "-application", "audio",
        "-compression_level", str(complexity),
        str(temp_opus),
    ]

    with acquire_semaphore(io_semaphore):
        result = run_subprocess(cmd, capture_stdout=False)
    if result.returncode != 0:
        log.append(f"  WARNING (ffmpeg): {result.stderr.strip()}")
        temp_opus.unlink(missing_ok=True)
        return None
    log.append(f"  Conversion done: {temp_opus.name}")
    return temp_opus


# ─── MKVMERGE ARGUMENT BUILDING ──────────────────────────────────────────────
def _build_mkvmerge_args(
    job: PlannedJob, tr: TrackSelection,
    temp_opus: Path | None, fonts: FontInfo,
    output_path: Path,
    sub_offset: float = 0.0,
) -> list[str]:
    """Construct the mkvmerge argument list for a planned job."""
    args: list[str] = ["-o", str(output_path),
                       "--video-tracks", str(tr.video_id)]

    keep_source_cover = job.cover_file is None and tr.source_cover_id is not None
    keep_attachment_ids: list[int] = []
    if job.use_source_fonts:
        keep_attachment_ids += tr.source_font_ids
    if keep_source_cover:
        keep_attachment_ids.append(tr.source_cover_id)

    if keep_attachment_ids:
        args += ["--attachments", ",".join(str(i) for i in keep_attachment_ids)]
    else:
        args.append("--no-attachments")

    if job.embedded_sub_id is not None:
        if sub_offset != 0.0:
            sync_ms = int(round(sub_offset * 1000))
            args += ["--sync", f"{job.embedded_sub_id}:{sync_ms}"]
        args += ["--subtitle-tracks", str(job.embedded_sub_id),
                 "--default-track-flag", f"{job.embedded_sub_id}:yes"]
    else:
        args.append("--no-subtitles")

    if job.chapter_file:
        args.append("--no-chapters")

    if tr.audio_id is not None and temp_opus is None:
        args += ["--audio-tracks", str(tr.audio_id)]
    else:
        args.append("--no-audio")

    args.append(str(job.video))

    if temp_opus is not None:
        track_lang = tr.audio_lang or "und"
        track_name = tr.audio_name or "Audio"
        args += ["--no-chapters",
                 "--language", f"0:{track_lang}",
                 "--track-name", f"0:{track_name}",
                 str(temp_opus)]

    if job.subtitle_file is not None:
        if sub_offset != 0.0:
            sync_ms = int(round(sub_offset * 1000))
            args += ["--sync", f"0:{sync_ms}"]
        args += ["--language", f"0:{job.subtitle_mkv_lang}",
                 "--track-name", f"0:{job.subtitle_label}",
                 "--default-track-flag", "0:yes",
                 str(job.subtitle_file)]

    if fonts.args:
        args.extend(fonts.args)

    if job.cover_file:
        cover_mime = COVER_MIME.get(job.cover_file.suffix.lower(), "image/jpeg")
        args += ["--attachment-name", f"cover{job.cover_file.suffix.lower()}",
                 "--attachment-mime-type", cover_mime,
                 "--attach-file", str(job.cover_file)]

    if job.chapter_file:
        args += ["--chapters", str(job.chapter_file)]

    return args


# ─── JOB EXECUTION ───────────────────────────────────────────────────────────
def process_job(
    job: PlannedJob, temp_dir: Path | None, fonts: FontInfo, opus_bitrate: str,
    json_cache: dict[Path, dict | None], ffmpeg_threads: int = 2,
    io_semaphore: threading.Semaphore | None = None,
    complexity: int = 10,
    sub_offset: float = 0.0,
) -> JobResult:
    r = JobResult()
    video, video_name, base = job.video, job.video_name, job.base
    output_file = job.output_file
    temp_opus: Path | None = None

    r.log.append(f"Processing: {video_name}")
    r.log.append("  Inspecting tracks...")

    json_data = json_cache.get(video)
    if json_data is None:
        r.log.append(f"  !! SKIPPED: {video_name}")
        r.log.append("  !! Reason : mkvmerge inspection failed.")
        r.skipped_reason = "mkvmerge failed"
        return r

    tr = select_tracks(json_data)

    if tr.video_id is None:
        r.log.append(f"  !! SKIPPED: {video_name}")
        r.log.append("  !! Reason : No video track found.")
        r.skipped_reason = "no video track"
        return r

    if tr.audio_id is not None:
        r.log.append(
            f"  Audio          : {tr.audio_reason} "
            f"(Track ID: {tr.audio_id}, codec: {tr.audio_codec}, "
            f"channels: {tr.audio_channels})"
        )
    else:
        r.log.append("  WARNING: No audio tracks found.")
    r.log.append(f"  Video Track ID : {tr.video_id}")
    r.log.append(f"  External fonts : {fonts.count}")
    if job.embedded_sub_id is not None:
        r.log.append(f"  Subtitle       : embedded track {job.embedded_sub_id} (from source)")
    elif job.subtitle_file is not None:
        r.log.append(f"  Subtitle       : {job.subtitle_file.name} (external)")
    if sub_offset != 0.0:
        r.log.append(f"  Sub Offset     : {sub_offset:+.2f}s")
    if job.use_source_fonts:
        r.log.append("  Font source    : using source's embedded fonts (no external fonts)")

    try:
        needs_conversion = tr.audio_id is not None and tr.audio_codec == "A_FLAC"
        if needs_conversion:
            temp_opus = _convert_flac_to_opus(
                video, tr.audio_id, tr.audio_index, temp_dir, base, opus_bitrate, r.log, ffmpeg_threads,
                complexity=complexity, io_semaphore=io_semaphore,
            )
            if temp_opus is None:
                r.log.append(f"  !! SKIPPED: {video_name}")
                r.log.append("  !! Reason : ffmpeg FLAC→Opus conversion failed.")
                r.skipped_reason = "Opus conversion failed"
                return r

        if job.chapter_file:
            r.log.append(f"  Chapters       : {job.chapter_file.name} (external)")
        elif tr.has_source_chapters:
            r.log.append("  Chapters       : using source's embedded chapters (no external match found)")
        else:
            r.log.append("  Chapters       : none found in folder or source")

        if job.cover_file:
            r.log.append(f"  Cover          : {job.cover_file.name} (external, attached)")
        elif job.cover_file is None and tr.source_cover_id is not None:
            r.log.append("  Cover          : keeping source's embedded cover (no external match found)")
        else:
            r.log.append("  Cover          : none found in folder or source")

        # Determine temporary output destination to prevent corrupted/partial files in case of interruption
        if temp_dir:
            temp_output = temp_dir / job.output_file.name
        else:
            temp_output = job.output_file.with_suffix(".tmp")

        temp_output.unlink(missing_ok=True)
        args = _build_mkvmerge_args(job, tr, temp_opus, fonts, temp_output, sub_offset=sub_offset)
        r.log.append("  Running mkvmerge...")

        with acquire_semaphore(io_semaphore):
            run_result = run_subprocess(["mkvmerge", *args])
        if run_result.stderr.strip():
            r.log.append(f"  WARNING (mkvmerge): {run_result.stderr.strip()}")

        if not mkv_success(run_result):
            r.log.append(f"  ERROR: mkvmerge failed (exit {run_result.returncode}).")
            temp_output.unlink(missing_ok=True)
            r.log.append("  Removed partial output.")
            r.skipped_reason = f"mkvmerge failed, exit {run_result.returncode}"
            return r

        # Atomically promote the temp file to the final destination on success
        temp_output.replace(output_file)
        r.log.append("  Completed successfully.")

    finally:
        if temp_opus is not None:
            temp_opus.unlink(missing_ok=True)

    return r


# ─── SUBTITLE/CHAPTER/COVER RESOLVERS ────────────────────────────────────────
def _resolve_subtitle(
    video: Path, parsed: EpisodeInfo,
    sub_index: SubIndex, json_data: dict | None,
    all_subs: list[Path],
    dry_run: bool,
    fallback_sub_index: SubIndex | None = None,
) -> tuple[Path | None, int | None, str, str, str, bool]:
    """
    Returns (subtitle_file, embedded_sub_id, lang, label, mkv_lang, matched).
    `matched=False` means user chose to skip (or, in dry-run mode, that a
    real run would prompt for manual selection here).

    The interactive input() prompt is skipped entirely when dry_run=True —
    dry-run must never block on keyboard input.
    """
    mkv_season, mkv_ep_type, mkv_ep = parsed
    sub_result = sub_index.lookup(mkv_ep_type, mkv_ep, mkv_season)

    subtitle_file:     Path | None = None
    embedded_sub_id:   int | None  = None
    subtitle_lang:     str         = "und"

    if sub_result is not None:
        subtitle_file, subtitle_lang = sub_result
        if subtitle_lang in SUBTITLE_LANGS_FALLBACK:
            logger.info("  NOTE: No Indonesian sub found; using English fallback (possible mislabelled fansub).")
        logger.info("  Subtitle       : %s [%s] (matched)", subtitle_file.name, LANG_LABEL.get(subtitle_lang, subtitle_lang))
        return (subtitle_file, None, subtitle_lang,
                LANG_LABEL.get(subtitle_lang, subtitle_lang),
                LANG_MKV_TAG.get(subtitle_lang, subtitle_lang), True)

    logger.info("  No external subtitle match — checking source for embedded subtitles...")
    _embedded = select_embedded_subtitle(json_data) if json_data else None
    if _embedded is not None:
        embedded_sub_id, subtitle_lang = _embedded
        if subtitle_lang in SUBTITLE_LANGS_FALLBACK:
            logger.info("  NOTE: No Indonesian sub in source; using English fallback (possible mislabelled fansub).")
        logger.info("  Subtitle       : embedded track %d [%s] (from source)", embedded_sub_id, LANG_LABEL.get(subtitle_lang, subtitle_lang))
        return (None, embedded_sub_id, subtitle_lang,
                LANG_LABEL.get(subtitle_lang, subtitle_lang),
                LANG_MKV_TAG.get(subtitle_lang, subtitle_lang), True)

    if fallback_sub_index is not None:
        fallback_result = fallback_sub_index.lookup(mkv_ep_type, mkv_ep, mkv_season)
        if fallback_result is not None:
            subtitle_file, subtitle_lang = fallback_result
            if subtitle_lang in SUBTITLE_LANGS_FALLBACK:
                logger.info("  NOTE: No Indonesian sub in fallback; using English fallback.")
            logger.info("  Subtitle       : %s [%s] (fallback asset)", subtitle_file.name, LANG_LABEL.get(subtitle_lang, subtitle_lang))
            return (subtitle_file, None, subtitle_lang,
                    LANG_LABEL.get(subtitle_lang, subtitle_lang),
                    LANG_MKV_TAG.get(subtitle_lang, subtitle_lang), True)

    logger.warning("No subtitle match found for %s%s (external or embedded).", mkv_ep_type, mkv_ep)
    if not all_subs or dry_run or not sys.stdin.isatty():
        if not all_subs:
            log_skip(video.name, "No .ass files exist in the folder at all.")
        elif dry_run:
            logger.info("  DRY RUN: would prompt for manual subtitle selection here (skipped).")
        elif not sys.stdin.isatty():
            log_skip(video.name, "Non-interactive environment — skipped manual subtitle selection.")
        return (None, None, "und", LANG_LABEL["und"], LANG_MKV_TAG["und"], False)

    # Standard interactive shell prints remain for direct command prompts
    print("  Available subtitles in folder:")
    for i, sub in enumerate(all_subs):
        print(f"    [{i + 1}] {sub.name}")
    print("    [0] Skip this video")
    choice = input("\n  Type a number to assign a subtitle, or 0 to skip: ").strip()

    if choice.isdigit() and 0 < int(choice) <= len(all_subs):
        chosen_sub    = all_subs[int(choice) - 1]
        subtitle_lang = detect_sub_lang(chosen_sub.stem)
        logger.info("  -> Manually linked: %s", chosen_sub.name)
        return (chosen_sub, None, subtitle_lang,
                LANG_LABEL.get(subtitle_lang, subtitle_lang),
                LANG_MKV_TAG.get(subtitle_lang, subtitle_lang), True)

    log_skip(video.name, "User skipped manual subtitle selection.")
    return (None, None, "und", LANG_LABEL["und"], LANG_MKV_TAG["und"], False)


def _resolve_chapter(
    parsed: EpisodeInfo,
    chap_index: ChapterIndex,
    chap_fallback_count: int,
    chapter_unmatched_claimed: set[Path],
    has_source_chapters: bool = False,
    fallback_chap_index: ChapterIndex | None = None,
) -> Path | None:
    mkv_season, mkv_ep_type, mkv_ep = parsed
    chapter_file = chap_index.lookup(mkv_ep_type, mkv_ep, mkv_season)

    if chapter_file is None and not has_source_chapters and chap_index.unmatched:
        unclaimed = [f for f in chap_index.unmatched if f not in chapter_unmatched_claimed]
        if len(unclaimed) == 1 and chap_fallback_count == 1:
            chapter_file = unclaimed[0]
            chapter_unmatched_claimed.add(chapter_file)
            logger.info("  NOTE: Chapter file name had no episode number — claimed as sole leftover match.")

    if chapter_file is None and not has_source_chapters and fallback_chap_index is not None:
        chapter_file = fallback_chap_index.lookup(mkv_ep_type, mkv_ep, mkv_season)
        if chapter_file:
            logger.info("  Chapters       : %s (fallback asset)", chapter_file.name)
            return chapter_file

    if chapter_file:
        logger.info("  Chapters       : %s (matched)", chapter_file.name)
    else:
        logger.info("  Chapters       : no matching file — will check source's own chapters")
    return chapter_file


def _resolve_cover(
    parsed: EpisodeInfo, cover_index: CoverIndex,
    fallback_cover_index: CoverIndex | None = None,
) -> Path | None:
    mkv_season, mkv_ep_type, mkv_ep = parsed
    cover_file = cover_index.lookup(mkv_ep_type, mkv_ep, mkv_season)
    if cover_file is None and fallback_cover_index is not None:
        cover_file = fallback_cover_index.lookup(mkv_ep_type, mkv_ep, mkv_season)
        if cover_file:
            logger.info("  Cover          : %s (fallback asset)", cover_file.name)
            return cover_file
    if cover_file:
        logger.info("  Cover          : %s (matched)", cover_file.name)
    else:
        logger.info("  Cover          : no external file — will check source's own cover")
    return cover_file


# ─── JOB PLANNING ────────────────────────────────────────────────────────────
def _plan_jobs(
    videos:       list[ParsedFile],
    json_cache:   dict[Path, dict | None],
    sub_index:    SubIndex,
    chap_index:   ChapterIndex,
    cover_index:  CoverIndex,
    output_dir:   Path,
    all_subs:     list[Path],
    use_source_fonts: bool,
    dry_run:      bool,
    fallback_sub_index:  SubIndex | None = None,
    fallback_chap_index: ChapterIndex | None = None,
    fallback_cover_index: CoverIndex | None = None,
) -> tuple[list[PlannedJob], list[str], list[str]]:

    parsed_videos = [(v.path, v.parsed) for v in videos]

    chapter_fallback_count = sum(
        1 for _, ep in parsed_videos
        if ep and not chap_index.has_match(ep[1], ep[2], ep[0])
    )
    chapter_unmatched_claimed: set[Path] = set()

    skipped_files: list[str] = []
    matched_files: list[str] = []
    jobs: list[PlannedJob] = []

    for video, parsed in parsed_videos:
        video_name = video.name
        base       = video.stem

        logger.info("-" * 60)
        logger.info("Matching: %s", video_name)

        if not parsed:
            log_skip(video_name, "Could not extract episode number from filename.")
            skipped_files.append(f"{video_name} (episode number not found)")
            continue

        mkv_season, mkv_ep_type, mkv_ep = parsed
        season_tag = f" (Season {mkv_season})" if mkv_season else ""
        logger.info("  Episode        : %s %s%s", ep_type_label(mkv_ep_type, long=True), mkv_ep, season_tag)

        out_stem    = output_stem(base, mkv_ep_type, mkv_ep)
        output_file = output_dir / f"{out_stem}.mkv"
        if is_completed_output(output_file):
            logger.info("  Output already exists. Skipping.")
            skipped_files.append(f"{video_name} (output already exists)")
            continue

        sub_file, embedded_id, sub_lang, sub_label, sub_mkv_lang, matched = _resolve_subtitle(
            video, parsed, sub_index, json_cache.get(video), all_subs, dry_run,
            fallback_sub_index=fallback_sub_index,
        )
        if not matched:
            skipped_files.append(f"{video_name} (no subtitle matched)")
            continue
        if sub_file:
            matched_files.append(f"{video_name}  <-  {sub_file.name}")
        elif embedded_id is not None:
            matched_files.append(f"{video_name}  <-  [embedded track {embedded_id}]")

        json_data = json_cache.get(video)
        has_src_chapters = has_chapters(json_data) if json_data else False

        chapter_file = _resolve_chapter(
            parsed, chap_index, chapter_fallback_count, chapter_unmatched_claimed,
            has_source_chapters=has_src_chapters,
            fallback_chap_index=fallback_chap_index,
        )
        if chapter_file:
            matched_files.append(f"{video_name}  <-  {chapter_file.name} [chapters]")

        cover_file = _resolve_cover(parsed, cover_index, fallback_cover_index=fallback_cover_index)
        if cover_file:
            matched_files.append(f"{video_name}  <-  {cover_file.name} [cover]")

        jobs.append(PlannedJob(
            video=video,
            video_name=video_name,
            base=base,
            output_file=output_file,
            subtitle_file=sub_file,
            subtitle_label=sub_label,
            subtitle_mkv_lang=sub_mkv_lang,
            chapter_file=chapter_file,
            cover_file=cover_file,
            embedded_sub_id=embedded_id,
            use_source_fonts=use_source_fonts,
        ))

    return jobs, matched_files, skipped_files


# ─── JOB EXECUTOR ORCHESTRATOR ───────────────────────────────────────────────
def _execute_jobs(
    jobs:          list[PlannedJob],
    temp_dir:      Path | None,
    fonts:         FontInfo,
    opus_bitrate:  str,
    pool:          ThreadPoolExecutor,
    max_workers:   int,
    ffmpeg_threads: int,
    json_cache:    dict[Path, dict | None],
    skipped_files: list[str],
    matched_files: list[str],
    complexity:    int = 10,
    io_semaphore:  threading.Semaphore | None = None,
    sub_offset:    float = 0.0,
) -> None:
    logger.info("Processing %d matched file(s) (up to %d at a time)...", len(jobs), max_workers)

    results: dict[Path, JobResult] = {}
    total_jobs = len(jobs)
    completed_jobs = 0

    futures = {
        pool.submit(process_job, job, temp_dir, fonts, opus_bitrate, json_cache, ffmpeg_threads, io_semaphore, complexity, sub_offset): job
        for job in jobs
    }
    for future in as_completed(futures):
        job = futures[future]
        completed_jobs += 1
        progress = completed_jobs / total_jobs
        try:
            res = future.result()
            results[job.video] = res
            if res.skipped_reason:
                logger.info(" -> [%d/%d (%.0f%%)] Failed/Skipped: %s", completed_jobs, total_jobs, progress * 100, job.video_name)
            else:
                logger.info(" -> [%d/%d (%.0f%%)] Remuxed: %s", completed_jobs, total_jobs, progress * 100, job.video_name)
        except Exception as exc:
            # Immediate ERROR with full traceback attached — shows up right
            # away on stderr, in context, rather than buried in the final
            # per-file summary at the end of a long batch run.
            logger.error(
                " -> [%d/%d (%.0f%%)] CRASHED: %s",
                completed_jobs, total_jobs, progress * 100, job.video_name,
                exc_info=True,
            )
            # Deferred summary uses WARNING (not ERROR) so the final
            # per-file log loop doesn't emit a second ERROR-level record
            # for the same crash — the traceback was already logged above.
            r = JobResult()
            r.log.append(f"Processing: {job.video_name}")
            r.log.append("  WARNING: Worker crashed. See traceback above.")
            r.skipped_reason = f"worker crash: {exc}"
            results[job.video] = r

    for job in jobs:
        result = results[job.video]
        logger.info("=" * 60)
        for line in result.log:
            # Prefix check (not substring) — a filename containing "ERROR" or
            # "WARNING" anywhere in it must not get misclassified as an error.
            stripped = line.lstrip()
            if stripped.startswith("WARNING"):
                logger.warning(line)
            elif stripped.startswith("ERROR"):
                logger.error(line)
            else:
                logger.info(line)
        if result.skipped_reason:
            skipped_files.append(f"{job.video_name} ({result.skipped_reason})")
            matched_files[:] = [
                m for m in matched_files
                if not m.startswith(f"{job.video_name}  <-")
            ]


# ─── SUMMARY ─────────────────────────────────────────────────────────────────
def _print_summary(output_dir: Path, matched_files: list[str], skipped_files: list[str]) -> None:
    logger.info("=========================================")
    logger.info(" Batch Processing Complete")
    logger.info(" Output: %s", output_dir)
    logger.info("=========================================")
    if matched_files:
        logger.info("--- MATCHED SUBTITLES & CHAPTERS (%d) ---", len(matched_files))
        for entry in matched_files:
            logger.info("  %s", entry)
    if skipped_files:
        logger.info("--- SKIPPED FILES (%d) ---", len(skipped_files))
        for entry in skipped_files:
            logger.info("  - %s", entry)


# ─── DISCOVERY ───────────────────────────────────────────────────────────────
def _discover_assets(
    fonts_dir: Path, search_dirs: list[Path],
    assets_dir: Path | None = None,
) -> tuple[FontInfo, SubIndex, ChapterIndex, CoverIndex, SubIndex | None, ChapterIndex | None, CoverIndex | None]:
    if fonts_dir.is_dir():
        logger.info("Scanning detected font directory: '%s'...", fonts_dir.name)
        fonts = scan_fonts(fonts_dir)
        logger.info("Fonts found: %d", fonts.count)
        if fonts.count == 0:
            logger.info("  (fonts folder is empty — will use source fonts if available)")
    else:
        logger.info("No font directory found — will use source fonts if available.")
        fonts = FontInfo(args=(), count=0)

    sub_index = SubIndex()
    chap_index = ChapterIndex()
    cover_index = CoverIndex()

    for d in reversed(search_dirs):
        assets = scan_assets(d)
        for sub_file in assets.subs:
            sub_index.add(sub_file)
        for xml_file in assets.chapters:
            chap_index.add(xml_file)
        for image_file in assets.covers:
            cover_index.add(image_file)

    fallback_sub_index: SubIndex | None = None
    fallback_chap_index: ChapterIndex | None = None
    fallback_cover_index: CoverIndex | None = None

    if assets_dir is not None:
        if not assets_dir.is_dir():
            logger.warning("Specified --assets-dir '%s' does not exist — fallback asset search disabled.", assets_dir)
        else:
            fallback_sub_index = SubIndex()
            fallback_chap_index = ChapterIndex()
            fallback_cover_index = CoverIndex()
            fallback_assets = scan_assets(assets_dir)
            for sub_file in fallback_assets.subs:
                fallback_sub_index.add(sub_file)
            for xml_file in fallback_assets.chapters:
                fallback_chap_index.add(xml_file)
            for image_file in fallback_assets.covers:
                fallback_cover_index.add(image_file)

            if fonts.count == 0:
                fallback_fonts_dir = assets_dir / "fonts" if (assets_dir / "fonts").is_dir() else assets_dir
                fallback_fonts = scan_fonts(fallback_fonts_dir)
                if fallback_fonts.count > 0:
                    fonts = fallback_fonts
                    logger.info("Detected %d external font(s) in fallback assets directory: %s", fonts.count, fallback_fonts_dir)

    logger.info("Subtitles indexed: %d primary, %d English fallback", sub_index.primary_count, sub_index.fallback_count)
    logger.info("Chapters indexed: %d by episode number, %d unmatched", chap_index.indexed_count, len(chap_index.unmatched))
    if chap_index.rejected_count:
        logger.info("  (%d .xml file(s) skipped — not chapter data)", chap_index.rejected_count)

    cover_note = " + folder-wide cover.*" if cover_index.has_global else ""
    logger.info("Covers indexed: %d by episode number%s", cover_index.indexed_count, cover_note)

    return fonts, sub_index, chap_index, cover_index, fallback_sub_index, fallback_chap_index, fallback_cover_index


# ─── ORCHESTRATOR ────────────────────────────────────────────────────────────
def _run(
    mkv_dir:      Path,
    videos:       list[ParsedFile],
    temp_dir:     Path | None,
    output_dir:   Path,
    fonts_dir:    Path,
    search_dirs:  list[Path],
    pool:         ThreadPoolExecutor,
    max_workers:  int,
    opus_bitrate: str,
    dry_run:      bool,
    sub_offset:   float = 0.0,
    complexity:   int = 10,
    persistent_cache: PersistentInspectCache | None = None,
    io_semaphore: threading.Semaphore | None = None,
    assets_dir:   Path | None = None,
) -> bool:
    if not videos:
        logger.warning("No MKV files to process in '%s'.", mkv_dir.name)
        return False
        
    discovered = _discover_assets(fonts_dir, search_dirs, assets_dir=assets_dir)
    fonts, sub_index, chap_index, cover_index, f_sub, f_chap, f_cov = discovered
    use_source_fonts = fonts.count == 0

    videos_to_inspect = []
    for v in videos:
        if not v.parsed:
            continue  # _plan_jobs will skip these anyway
            
        mkv_season, mkv_ep_type, mkv_ep = v.parsed
        out_stem = output_stem(v.stem, mkv_ep_type, mkv_ep)
        if is_completed_output(output_dir / f"{out_stem}.mkv"):
            continue
            
        if dry_run and sub_index.lookup(mkv_ep_type, mkv_ep, mkv_season) is not None:
            continue
            
        videos_to_inspect.append(v.path)

    json_cache = inspect_videos(videos_to_inspect, pool, persistent_cache=persistent_cache) if videos_to_inspect else {}

    # Collect all subtitle paths across the search path for interactive fallback selection
    seen_subs: set[Path] = set()
    all_subs: list[Path] = []
    for d in search_dirs:
        for sf in scan_assets(d).subs:
            if sf.path not in seen_subs:
                seen_subs.add(sf.path)
                all_subs.append(sf.path)
    all_subs.sort(key=natural_sort_key)

    jobs, matched_files, skipped_files = _plan_jobs(
        videos, json_cache, sub_index, chap_index, cover_index,
        output_dir, all_subs, use_source_fonts, dry_run,
        fallback_sub_index=f_sub,
        fallback_chap_index=f_chap,
        fallback_cover_index=f_cov,
    )

    if dry_run:
        logger.info("--- DRY RUN: Planned jobs ---")
        if jobs:
            for job in jobs:
                parts = [f"  {job.video_name}"]
                if job.subtitle_file:
                    parts.append(f"sub={job.subtitle_file.name}")
                elif job.embedded_sub_id is not None:
                    parts.append(f"sub=embedded:{job.embedded_sub_id}")
                if job.chapter_file:
                    parts.append(f"chap={job.chapter_file.name}")
                if job.cover_file:
                    parts.append(f"cover={job.cover_file.name}")
                logger.info("  |  ".join(parts))
        else:
            logger.info("  (no jobs planned)")
        logger.info("Total: %d job(s). No processing performed.", len(jobs))
        _print_summary(output_dir, matched_files, skipped_files)
        return all("(output already exists)" in s for s in skipped_files)

    # Distribute available CPU cores evenly among workers to prevent core thrashing
    ffmpeg_threads = max(1, (os.cpu_count() or 4) // max_workers)
    _execute_jobs(jobs, temp_dir, fonts, opus_bitrate, pool, max_workers, ffmpeg_threads, json_cache, skipped_files, matched_files, complexity, io_semaphore=io_semaphore, sub_offset=sub_offset)
    _print_summary(output_dir, matched_files, skipped_files)
    return all("(output already exists)" in s for s in skipped_files)


# ─── MAIN ────────────────────────────────────────────────────────────────────
def main() -> None:
    setup_logging(logging.INFO)

    cpu_count = os.cpu_count() or 4
    default_workers = min(cpu_count * DEFAULT_WORKER_MULTIPLIER, MAX_DEFAULT_WORKERS)

    parser = argparse.ArgumentParser(
        description="Batch MKV Re-Mux Utility for Anime & Fansubs",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--workers", type=int, default=default_workers, choices=range(1, 33),
                        metavar="[1-32]",
                        help="Number of concurrent files to process.")
    parser.add_argument("--bitrate", type=str, default="192k",
                        help="Target bitrate for Opus audio conversion (e.g., 128k, 192k).")
    parser.add_argument("--complexity", type=int, default=10, choices=range(0, 11),
                        metavar="[0-10]",
                        help="Opus compression complexity level (0-10, default: 10).")
    parser.add_argument("--sub-offset", type=float, default=0.0,
                        help="Shift subtitle timestamps by this many seconds (negative = earlier, positive = later).")
    parser.add_argument("--out-dir", type=str, default=None,
                        help="Directory where finished MKVs will be saved (defaults to <input-dir>/Output).")
    parser.add_argument("--input-dir", type=str, default=None,
                        help="Directory containing MKV files (defaults to script directory).")
    parser.add_argument("--fonts-dir", type=str, default=None,
                        help="Directory to use for fonts (auto-detects 'fonts'/'Fonts'/"
                             "'attachments' inside --input-dir if not given).")
    parser.add_argument("--assets-dir", type=str, default=None,
                        help="Fallback directory (e.g. a prior extraction's --out-dir) to search for subtitles/chapters/covers/fonts when a video has none of its own and none are found alongside it.")
    parser.add_argument("--dry-run", action="store_true",
                        help="Show matched files without processing.")
    parser.add_argument("--keep-fonts", action="store_true",
                        help="Keep the extracted fonts folder after processing.")
    parser.add_argument("--io-jobs", type=int, default=2,
                        help="Maximum concurrent disk I/O jobs for mkvmerge. Set to 0 to disable throttling.")
    parser.add_argument("--no-cache", action="store_true",
                        help="Bypass persistent inspection cache.")
    parser.add_argument("--clear-cache", action="store_true",
                        help="Clear persistent inspection cache.")
    cfg          = parser.parse_args()

    max_workers  = cfg.workers
    opus_bitrate = cfg.bitrate

    if cfg.clear_cache:
        PersistentInspectCache().clear()
        logger.info("Persistent inspection cache cleared.")
        sys.exit(0)

    persistent_cache = PersistentInspectCache(enabled=not cfg.no_cache)
    io_semaphore = threading.BoundedSemaphore(cfg.io_jobs) if cfg.io_jobs > 0 else None

    if not re.match(r"^\d+[kM]?$", opus_bitrate):
        logger.error("Invalid bitrate format: '%s'. Must match pattern like 128k, 192k, 1M.", opus_bitrate)
        sys.exit(1)
    root_input_dir = Path(cfg.input_dir).resolve() if cfg.input_dir else SCRIPT_DIR
    out_override = Path(cfg.out_dir).resolve() if cfg.out_dir else None
    fonts_override = Path(cfg.fonts_dir).resolve() if cfg.fonts_dir else None
    assets_override = Path(cfg.assets_dir).resolve() if cfg.assets_dir else None
    dry_run      = cfg.dry_run

    logger.info("=========================================")
    logger.info(" MKV Batch Re-Mux Utility")
    logger.info(" Anime / Indonesian Fansub Edition")
    logger.info("=========================================")
    logger.info("Workers: %d | Bitrate: %s | Input: %s", max_workers, opus_bitrate, root_input_dir)

    check_dependencies("mkvmerge", "ffmpeg")
    logger.info("Using mkvmerge : %s", shutil.which('mkvmerge'))
    logger.info("Using ffmpeg   : %s", shutil.which('ffmpeg'))

    # Discover all MKV files recursively to find which subdirectories contain them
    ignore_paths = {out_override} if out_override else None
    all_mkvs = scan_mkv_files(root_input_dir, ignore_paths=ignore_paths)
    if not all_mkvs:
        logger.error("No MKV files found.")
        sys.exit(1)

    mkvs_by_dir: dict[Path, list[ParsedFile]] = defaultdict(list)
    for mkv in all_mkvs:
        mkvs_by_dir[mkv.path.parent].append(mkv)

    mkv_dirs = sorted(list(mkvs_by_dir.keys()), key=natural_sort_key)

    overall_success = True

    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        for mkv_dir in mkv_dirs:
            logger.info("\n" + "=" * 60)
            logger.info("Processing directory: %s", mkv_dir)
            logger.info("=" * 60)

            current_output_dir = resolve_output_dir(
                mkv_dir,
                root_input_dir,
                out_override,
            )

            if not dry_run:
                current_output_dir.mkdir(parents=True, exist_ok=True)

            search_dirs = _ancestor_dirs(mkv_dir, root_input_dir)

            current_assets_dir = None
            if assets_override:
                current_assets_dir = resolve_output_dir(
                    mkv_dir, root_input_dir, assets_override, default_subdir=None,
                )

            # Resolve fonts directory for this subdirectory by scanning the full ancestor chain (local-first)
            current_fonts_dir = None
            if fonts_override:
                current_fonts_dir = fonts_override
            else:
                for d in reversed(search_dirs):
                    candidate = find_fonts_dir(d)
                    if candidate.is_dir():
                        current_fonts_dir = candidate
                        break
                if current_fonts_dir is None:
                    current_fonts_dir = mkv_dir / "fonts"

            if fonts_override:
                logger.info("Fonts folder: %s (override)", current_fonts_dir)
            elif current_fonts_dir.is_dir():
                if current_fonts_dir.parent == mkv_dir:
                    logger.info("Fonts folder: %s", current_fonts_dir)
                elif current_fonts_dir.parent == root_input_dir:
                    logger.info("Fonts folder: %s (root fallback)", current_fonts_dir)
                else:
                    logger.info("Fonts folder: %s (parent fallback)", current_fonts_dir)
            else:
                logger.info("Fonts folder: %s", current_fonts_dir)

            temp_dir = None
            if not dry_run:
                temp_dir = Path(tempfile.mkdtemp(prefix=".tmp_remux_", dir=current_output_dir))
            
            dir_mkvs = mkvs_by_dir[mkv_dir]
            
            success = False
            try:
                success = _run(
                    mkv_dir, dir_mkvs, temp_dir, current_output_dir, current_fonts_dir,
                    search_dirs, pool, max_workers, opus_bitrate, dry_run,
                    sub_offset=cfg.sub_offset,
                    complexity=cfg.complexity, persistent_cache=persistent_cache,
                    io_semaphore=io_semaphore, assets_dir=current_assets_dir,
                )
                if not success:
                    overall_success = False
            finally:
                if temp_dir is not None:
                    shutil.rmtree(temp_dir, ignore_errors=True)

            if success and not dry_run and not cfg.keep_fonts and fonts_override is None:
                if current_fonts_dir.is_dir() and current_fonts_dir.name == "fonts" and current_fonts_dir.parent == mkv_dir:
                    try:
                        shutil.rmtree(current_fonts_dir)
                        logger.info("Cleanup: Removed temporary fonts folder '%s'", current_fonts_dir.name)
                    except Exception as e:
                        logger.error("Failed to remove fonts folder: %s", e)

    if not overall_success:
        sys.exit(1)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        logger.warning("Cancelled by user.")
        sys.exit(130)
