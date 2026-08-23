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

NOTE: Concurrent invocations on the same output tree are unsupported.
"""
from __future__ import annotations

import argparse
import json
import logging
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import uuid
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path
from typing import Literal

from mkv_assets import (
    AssetResolver,
    InteractionMode,
    claim_leftover_chapters,
)
from mkv_domain import (
    COVER_MIME,
    ancestor_dirs,
    default_ffmpeg_threads,
    ep_type_label,
    is_dialogue_track,
    output_stem,
    resolve_output_dir,
    seasons_match,  # noqa: F401 - compatibility re-export
)
from mkv_events import (
    OUTCOME_CANCELLED,
    OUTCOME_COMPLETED,
    OUTCOME_ERROR,
    OUTCOME_PLANNED,
    OUTCOME_SKIPPED,
    emit_batch_complete,
    emit_batch_start,
    emit_item_complete,
    emit_item_start,
    get_item_id,
    set_events_enabled,
    setup_logging,
)
from mkv_fonts import FontInfo, find_fonts_dir
from mkv_proc import (
    SUBPROCESS_TIMEOUT,
    CancelledError,
    CancelToken,
    acquire_semaphore,
    run_subprocess_cancellable,
)
from mkv_runner import (
    SCRIPT_DIR,
    CliRuntime,
    add_shared_args,
    build_cli_runtime,
    collect_release_dirs,
    handle_clear_cache_flag,
    install_cancel_handlers,
    run_batch,
)
from mkv_scan import (
    InspectedVideo,
    ParsedFile,
    PersistentInspectCache,
    ScanResults,
    check_dependencies,
    inspect_videos,
    scan_mkv_files,
)

# Initialize child logger for the remux module
logger = logging.getLogger("mkv_automation.remux")

_ancestor_dirs = ancestor_dirs


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


def select_tracks(inspected: InspectedVideo) -> TrackSelection:
    has_src_chapters = inspected.has_chapters()
    cover = inspected.cover_attachment()
    source_cover_id = int(cover["id"]) if cover and cover.get("id") is not None else None
    source_font_ids = [int(a["id"]) for a in inspected.font_attachments() if a.get("id") is not None]

    video_track = inspected.video_track
    video_id = video_track.get("id") if video_track is not None else None
    video_pos = inspected.video_track_pos

    audio_tracks = inspected.audio_tracks()

    audio, reason = _pick_audio(audio_tracks, video_pos)
    audio = audio or {
        "id": None,
        "audio_idx": 0,
        "codec": "",
        "channels": 0,
        "lang": "",
        "name": "",
    }

    return TrackSelection(
        video_id=video_id,
        audio_id=audio["id"],
        audio_index=audio["audio_idx"],
        audio_codec=audio["codec"],
        audio_channels=audio["channels"],
        audio_lang=audio["lang"],
        audio_name=audio["name"],
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
    primary_tracks = [track for track in audio_tracks if not _COMMENTARY_RE.search(track["name"])]
    if primary_tracks:
        pool_to_use = primary_tracks
        is_fallback_pool = False
    else:
        pool_to_use = audio_tracks
        is_fallback_pool = True

    candidates = []
    for track in pool_to_use:
        is_ja = track["lang"].startswith(("ja", "jpn"))
        is_und = (track["lang"] == "und")
        is_stereo = (track["channels"] == 2)
        is_diag = is_dialogue_track(track["name"])

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
        elif track["pos"] > video_pos:
            score = 3

        if score == 0:
            score = 2 if is_ja else 1

        priority_key = (score, -track["pos"])
        candidates.append((priority_key, track))

    best_candidate = max(candidates, key=lambda candidate: candidate[0])
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


def mkv_success(result) -> bool:
    return result.returncode <= 1


def is_completed_output(path: Path) -> bool:
    """Return whether a remux output exists and contains data."""
    return path.is_file() and path.stat().st_size > 0


def _prompt_for_subtitle(video: ParsedFile, all_subs: list[Path]) -> Path | None:
    """Interactive fallback: offer all discovered subtitles for manual assignment.

    Returns the chosen subtitle, or None when the user skips.
    """
    print("  Available subtitles in release directory:")
    for i, sub in enumerate(all_subs):
        print(f"    [{i + 1}] {sub.name}")
    print("    [0] Skip this video")
    choice = input("\n  Type a number to assign a subtitle, or 0 to skip: ").strip()
    if choice.isdigit() and 0 < int(choice) <= len(all_subs):
        return all_subs[int(choice) - 1]
    return None


# ─── JOB MODEL ───────────────────────────────────────────────────────────────
@dataclass
class PlannedJob:
    video:             Path
    video_name:        str
    base:              str
    output_file:       Path
    subtitle_file:     Path | None
    subtitle_label:    str
    subtitle_mkv_lang: str
    chapter_file:      Path | None
    cover_file:        Path | None
    embedded_sub_id:   int | None = None
    use_source_fonts:  bool = False
    item_id:           str = ""


@dataclass
class JobResult:
    log:            list[str] = field(default_factory=list)
    skipped_reason: str | None = None
    error_reason:   str | None = None
    cancelled:      bool = False
    failed:         bool = False


# ─── AUDIO CONVERSION ────────────────────────────────────────────────────────
def _convert_flac_to_opus(
    video: Path,
    audio_index: int,
    temp_dir: Path,
    base: str,
    opus_bitrate: str,
    log: list[str] | None = None,
    ffmpeg_threads: int = 2,
    complexity: int = 10,
    cancel_token: CancelToken | None = None,
) -> Path | None:
    if log is not None:
        log.append(f"  Converting FLAC audio -> Opus ({opus_bitrate})...")
    unique_id = uuid.uuid4().hex[:8]
    temp_opus = temp_dir / f"{base}_{unique_id}.opus"

    cmd = [
        "ffmpeg", "-y",
        "-analyzeduration", "0",
        "-probesize", "1M",
        "-v", "warning",
        "-threads", str(ffmpeg_threads),
        "-i", str(video),
        "-map", f"0:a:{audio_index}",
        "-c:a", "libopus",
        "-b:a", opus_bitrate,
        "-vbr", "on",
        "-compression_level", str(complexity),
        "-application", "audio",
        str(temp_opus),
    ]

    try:
        run_res = run_subprocess_cancellable(cmd, cancel_token=cancel_token, timeout=SUBPROCESS_TIMEOUT)
    except subprocess.TimeoutExpired:
        if log is not None:
            log.append(f"  ERROR: ffmpeg FLAC→Opus conversion timed out after {SUBPROCESS_TIMEOUT}s.")
        temp_opus.unlink(missing_ok=True)
        return None

    if run_res.returncode != 0:
        if log is not None:
            log.append(f"  ERROR: ffmpeg audio conversion failed (exit {run_res.returncode}): {run_res.stderr.strip()}")
        temp_opus.unlink(missing_ok=True)
        return None

    if log is not None:
        log.append("  Audio conversion done.")
    return temp_opus


# ─── MKVMERGE ARGS BUILDER ───────────────────────────────────────────────────
def _build_mkvmerge_args(
    job: PlannedJob,
    selection: TrackSelection,
    temp_opus: Path | None,
    fonts: FontInfo,
    output_path: Path,
    sub_offset: float = 0.0,
) -> list[str]:
    args: list[str] = ["-o", str(output_path)]

    if job.chapter_file:
        args += ["--no-chapters"]

    if job.subtitle_file is None and job.embedded_sub_id is not None:
        args += ["--subtitle-tracks", str(job.embedded_sub_id)]
    else:
        args += ["--no-subtitles"]

    if temp_opus is None and selection.audio_id is not None:
        args += ["--audio-tracks", str(selection.audio_id)]
    else:
        args += ["--no-audio"]

    keep_ids: list[int] = []
    if not job.cover_file and selection.source_cover_id is not None:
        keep_ids.append(selection.source_cover_id)
    if job.use_source_fonts:
        keep_ids.extend(selection.source_font_ids)
    args += ["--attachments", ",".join(map(str, keep_ids))] if keep_ids else ["--no-attachments"]

    if selection.video_id is not None:
        args += ["--video-tracks", str(selection.video_id)]
    else:
        args += ["--no-video"]

    args.append(str(job.video))

    if temp_opus is not None:
        args += ["--no-chapters"]
        if selection.audio_lang:
            args += ["--language", f"0:{selection.audio_lang}"]
        if selection.audio_name:
            args += ["--track-name", f"0:{selection.audio_name}"]
        args.append(str(temp_opus))

    if job.subtitle_file:
        if sub_offset != 0.0:
            offset_ms = round(sub_offset * 1000)
            args += ["--sync", f"0:{offset_ms}"]
        args += [
            "--language", f"0:{job.subtitle_mkv_lang}",
            "--track-name", f"0:{job.subtitle_label}",
            "--default-track-flag", "0:yes",
        ]
        args.append(str(job.subtitle_file))

    if job.chapter_file:
        args += ["--chapters", str(job.chapter_file)]

    if job.cover_file:
        mime = COVER_MIME.get(job.cover_file.suffix.lower(), "image/jpeg")
        args += [
            "--attachment-name", "cover" + job.cover_file.suffix.lower(),
            "--attachment-mime-type", mime,
            "--attach-file", str(job.cover_file),
        ]

    args += list(fonts.args)
    return args


# ─── JOB EXECUTION ───────────────────────────────────────────────────────────
def process_job(
    job: PlannedJob, temp_dir: Path | None, fonts: FontInfo, opus_bitrate: str,
    json_cache: dict[Path, InspectedVideo | None], ffmpeg_threads: int = 2,
    io_semaphore: threading.Semaphore | None = None,
    complexity: int = 10,
    sub_offset: float = 0.0,
    cancel_token: CancelToken | None = None,
) -> JobResult:
    result = JobResult()
    video, video_name, base = job.video, job.video_name, job.base
    output_file = job.output_file
    temp_opus: Path | None = None

    if cancel_token is not None and cancel_token.cancelled:
        emit_item_start(job.item_id, video_name)
        result.cancelled = True
        result.log.append(f"Processing: {video_name}")
        result.log.append("  !! CANCELLED before start.")
        emit_item_complete(job.item_id, video_name, OUTCOME_CANCELLED)
        return result

    emit_item_start(job.item_id, video_name)

    result.log.append(f"Processing: {video_name}")
    result.log.append("  Inspecting tracks...")

    def _skip(reason: str, detail: str) -> JobResult:
        result.log.append(f"  !! SKIPPED: {video_name}")
        result.log.append(f"  !! Reason : {detail}")
        result.skipped_reason = reason
        emit_item_complete(job.item_id, video_name, OUTCOME_SKIPPED, reason=reason)
        return result

    def _error(reason: str, detail: str) -> JobResult:
        result.log.append(f"  !! ERROR: {video_name}")
        result.log.append(f"  !! Detail : {detail}")
        result.failed = True
        result.error_reason = reason
        emit_item_complete(job.item_id, video_name, OUTCOME_ERROR, error=reason)
        return result

    inspected = json_cache.get(video)
    if inspected is None:
        return _error("mkvmerge failed", "mkvmerge inspection failed.")

    selection = select_tracks(inspected)

    if selection.video_id is None:
        return _error("no video track", "No video track found.")

    if selection.audio_id is not None:
        result.log.append(
            f"  Audio          : {selection.audio_reason} "
            f"(Track ID: {selection.audio_id}, codec: {selection.audio_codec}, "
            f"channels: {selection.audio_channels})"
        )
    else:
        result.log.append("  WARNING: No audio tracks found.")
    result.log.append(f"  Video Track ID : {selection.video_id}")
    result.log.append(f"  External fonts : {fonts.count}")
    if job.embedded_sub_id is not None:
        result.log.append(f"  Subtitle       : embedded track {job.embedded_sub_id} (from source)")
    elif job.subtitle_file is not None:
        result.log.append(f"  Subtitle       : {job.subtitle_file.name} (external)")
    if sub_offset != 0.0:
        result.log.append(f"  Sub Offset     : {sub_offset:+.2f}s")
    if job.use_source_fonts:
        result.log.append("  Font source    : using source's embedded fonts (no external fonts)")

    try:
        needs_conversion = selection.audio_id is not None and selection.audio_codec == "A_FLAC"
        if needs_conversion:
            if temp_dir is None:
                return _skip("temp dir unavailable", "Temporary directory unavailable for FLAC→Opus conversion.")
            temp_opus = _convert_flac_to_opus(
                video=video,
                audio_index=selection.audio_index,
                temp_dir=temp_dir,
                base=base,
                opus_bitrate=opus_bitrate,
                log=result.log,
                ffmpeg_threads=ffmpeg_threads,
                complexity=complexity,
                cancel_token=cancel_token,
            )
            if temp_opus is None:
                return _error("Opus conversion failed", "ffmpeg FLAC→Opus conversion failed.")

        if job.chapter_file:
            result.log.append(f"  Chapters       : {job.chapter_file.name} (external)")
        elif selection.has_source_chapters:
            result.log.append("  Chapters       : using source's embedded chapters (no external match found)")
        else:
            result.log.append("  Chapters       : none found in folder or source")

        if job.cover_file:
            result.log.append(f"  Cover          : {job.cover_file.name} (external, attached)")
        elif selection.source_cover_id is not None:
            result.log.append("  Cover          : keeping source's embedded cover (no external match found)")
        else:
            result.log.append("  Cover          : none found in folder or source")

        if temp_dir:
            temp_output = temp_dir / job.output_file.name
        else:
            temp_output = job.output_file.with_suffix(".tmp")

        temp_output.unlink(missing_ok=True)
        args = _build_mkvmerge_args(job, selection, temp_opus, fonts, temp_output, sub_offset=sub_offset)
        result.log.append("  Running mkvmerge...")

        try:
            with acquire_semaphore(io_semaphore):
                run_result = run_subprocess_cancellable(["mkvmerge", *args], cancel_token=cancel_token, timeout=SUBPROCESS_TIMEOUT)
        except subprocess.TimeoutExpired:
            result.log.append(f"  ERROR: mkvmerge timed out after {SUBPROCESS_TIMEOUT}s.")
            temp_output.unlink(missing_ok=True)
            result.log.append("  Removed partial output.")
            result.failed = True
            result.error_reason = f"mkvmerge timed out after {SUBPROCESS_TIMEOUT}s"
            emit_item_complete(job.item_id, video_name, OUTCOME_ERROR, error=result.error_reason)
            return result

        if run_result.stderr.strip():
            result.log.append(f"  WARNING (mkvmerge): {run_result.stderr.strip()}")

        if not mkv_success(run_result):
            result.log.append(f"  ERROR: mkvmerge failed (exit {run_result.returncode}).")
            temp_output.unlink(missing_ok=True)
            result.log.append("  Removed partial output.")
            result.failed = True
            result.error_reason = f"mkvmerge failed, exit {run_result.returncode}"
            emit_item_complete(job.item_id, video_name, OUTCOME_ERROR, error=result.error_reason)
            return result

        temp_output.replace(output_file)
        result.log.append("  Completed successfully.")
        emit_item_complete(job.item_id, video_name, OUTCOME_COMPLETED)

    except CancelledError:
        result.cancelled = True
        emit_item_complete(job.item_id, video_name, OUTCOME_CANCELLED)
        raise

    finally:
        if temp_opus is not None:
            temp_opus.unlink(missing_ok=True)

    return result


# ─── JOB PLANNING ────────────────────────────────────────────────────────────
def _plan_jobs(
    videos:           list[ParsedFile],
    json_cache:       dict[Path, InspectedVideo | None],
    resolver:         AssetResolver,
    output_dir:       Path | None = None,
    all_subs:         list[Path] | None = None,
    use_source_fonts: bool = False,
    dry_run:          bool = False,
    root_input_dir:   Path | None = None,
    sub_assignments:  dict[str, Path] | None = None,
    mode:             InteractionMode = InteractionMode.INTERACTIVE,
) -> tuple[list[PlannedJob], list[str], list[str]]:
    """Plan jobs for one release directory.

    dry_run and mode are deliberately independent: dry_run defers skip-event
    emission to the end of planning, while mode decides prompting inside
    resolution. Production keeps them consistent via main(); direct callers
    may combine them freely.
    """

    out_dir = output_dir or Path()
    subs_list = all_subs or []
    root_dir = root_input_dir or out_dir
    assignments = sub_assignments or {}

    chapter_claims = claim_leftover_chapters(videos, json_cache, resolver)

    skipped_files: list[str] = []
    skipped_events: list[tuple[ParsedFile, str]] = []
    matched_files: list[str] = []
    jobs: list[PlannedJob] = []

    def record_skip(video_file: ParsedFile, reason: str) -> None:
        skipped_files.append(f"{video_file.name} ({reason})")
        if not dry_run:
            emit_item_complete(
                get_item_id(video_file.path, root_dir),
                video_file.name,
                OUTCOME_SKIPPED,
                reason=reason,
            )
        else:
            skipped_events.append((video_file, reason))

    for video_file in videos:
        video = video_file.path
        parsed = video_file.parsed
        video_name = video.name
        base       = video.stem
        item_id    = get_item_id(video, root_dir)

        logger.info("-" * 60)
        logger.info("Matching: %s", video_name)

        if not parsed:
            log_skip(video_name, "Could not extract episode number from filename.")
            record_skip(video_file, "episode number not found")
            continue

        mkv_season, mkv_ep_type, mkv_ep = parsed
        season_tag = f" (Season {mkv_season})" if mkv_season else ""
        logger.info("  Episode        : %s %s%s", ep_type_label(mkv_ep_type, long=True), mkv_ep, season_tag)

        out_stem    = output_stem(base, mkv_ep_type, mkv_ep)
        output_file = out_dir / f"{out_stem}.mkv"
        if is_completed_output(output_file):
            logger.info("  Output already exists. Skipping.")
            record_skip(video_file, "output already exists")
            continue

        inspected = json_cache.get(video)
        assigned_sub = assignments.get(item_id)
        if assigned_sub is not None and not assigned_sub.is_file():
            logger.warning("  Assigned subtitle '%s' no longer exists — falling back to automatic matching.", assigned_sub)
            assigned_sub = None
        res_assets = resolver.resolve_for(
            video_file,
            inspected,
            all_subs=subs_list,
            mode=mode,
            claimed_chapter=chapter_claims.get(video_file.path),
            assigned_subtitle=assigned_sub,
        )

        if not res_assets.matched_subtitle:
            record_skip(video_file, res_assets.skip_reason or "no subtitle matched")
            continue

        if res_assets.subtitle_file:
            matched_files.append(f"{video_name}  <-  {res_assets.subtitle_file.name}")
        elif res_assets.embedded_sub_id is not None:
            matched_files.append(f"{video_name}  <-  [embedded track {res_assets.embedded_sub_id}]")

        if res_assets.chapter_file:
            matched_files.append(f"{video_name}  <-  {res_assets.chapter_file.name} [chapters]")

        if res_assets.cover_file:
            matched_files.append(f"{video_name}  <-  {res_assets.cover_file.name} [cover]")

        jobs.append(PlannedJob(
            video=video,
            video_name=video_name,
            base=base,
            output_file=output_file,
            subtitle_file=res_assets.subtitle_file,
            subtitle_label=res_assets.subtitle_label,
            subtitle_mkv_lang=res_assets.subtitle_mkv_lang,
            chapter_file=res_assets.chapter_file,
            cover_file=res_assets.cover_file,
            embedded_sub_id=res_assets.embedded_sub_id,
            use_source_fonts=use_source_fonts,
            item_id=item_id,
        ))

    if dry_run:
        for job in jobs:
            emit_item_complete(job.item_id, job.video_name, OUTCOME_PLANNED)
        for video, reason in skipped_events:
            emit_item_complete(
                get_item_id(video.path, root_dir),
                video.name,
                OUTCOME_SKIPPED,
                reason=reason,
            )

    return jobs, matched_files, skipped_files


# ─── JOB EXECUTOR ORCHESTRATOR ───────────────────────────────────────────────
def _execute_jobs(
    jobs:           list[PlannedJob],
    temp_dir:       Path | None,
    fonts:          FontInfo,
    opus_bitrate:   str,
    pool:           ThreadPoolExecutor,
    max_workers:    int,
    ffmpeg_threads: int,
    json_cache:     dict[Path, InspectedVideo | None],
    skipped_files:  list[str],
    failed_files:   list[str],
    complexity:     int = 10,
    io_semaphore:   threading.Semaphore | None = None,
    sub_offset:     float = 0.0,
    cancel_token:   CancelToken | None = None,
) -> None:
    """Execute planned jobs; intentional skips land in skipped_files while
    genuine processing errors/crashes additionally land in failed_files so
    callers can exit non-zero without treating planning skips as failures."""
    logger.info("Processing %d matched file(s) (up to %d at a time)...", len(jobs), max_workers)

    total_jobs = len(jobs)
    futures = {
        pool.submit(
            process_job,
            job=job,
            temp_dir=temp_dir,
            fonts=fonts,
            opus_bitrate=opus_bitrate,
            json_cache=json_cache,
            ffmpeg_threads=ffmpeg_threads,
            io_semaphore=io_semaphore,
            complexity=complexity,
            sub_offset=sub_offset,
            cancel_token=cancel_token,
        ): job
        for job in jobs
    }
    for completed_jobs, future in enumerate(as_completed(futures), start=1):
        job = futures[future]
        progress = completed_jobs / total_jobs
        try:
            res = future.result()
            if res.cancelled:
                skipped_files.append(f"{job.video_name} (cancelled)")
                logger.info(
                    "[%d/%d (%.0f%%)] Cancelled: %s",
                    completed_jobs, total_jobs, progress * 100, job.video_name,
                )
            elif res.failed or res.error_reason:
                failed_files.append(job.video_name)
                skipped_files.append(f"{job.video_name} (failed: {res.error_reason or 'error'})")
                logger.info(
                    "[%d/%d (%.0f%%)] Failed: %s",
                    completed_jobs, total_jobs, progress * 100, job.video_name,
                )
            elif res.skipped_reason:
                skipped_files.append(f"{job.video_name} ({res.skipped_reason})")
                logger.info(
                    "[%d/%d (%.0f%%)] Skipped: %s",
                    completed_jobs, total_jobs, progress * 100, job.video_name,
                )
            else:
                logger.info(
                    "[%d/%d (%.0f%%)] Remuxed: %s",
                    completed_jobs, total_jobs, progress * 100, job.video_name,
                )
        except Exception as exc:
            logger.exception(
                "[%d/%d (%.0f%%)] CRASHED: %s",
                completed_jobs, total_jobs, progress * 100, job.video_name,
            )
            emit_item_complete(job.item_id, job.video_name, OUTCOME_ERROR, error=str(exc))
            failed_files.append(job.video_name)
            skipped_files.append(f"{job.video_name} (exception: {exc})")


def _print_summary(output_dir: Path, matched_files: list[str], skipped_files: list[str]) -> None:
    logger.info("=========================================")
    logger.info(" Processing Summary")
    logger.info("=========================================")
    logger.info("Output directory: %s", output_dir)
    logger.info("Matched & processed: %d", len(matched_files))
    logger.info("Skipped: %d", len(skipped_files))
    if matched_files:
        logger.info("--- MATCHED FILES (%d) ---", len(matched_files))
        for entry in matched_files:
            logger.info("  %s", entry)
    if skipped_files:
        logger.info("--- SKIPPED FILES (%d) ---", len(skipped_files))
        for entry in skipped_files:
            logger.info("  - %s", entry)


# ─── INSPECTION MANIFEST GENERATION ──────────────────────────────────────────
def _parse_resolution(pixel_dimensions: str) -> str | None:
    if not pixel_dimensions:
        return None
    parts = pixel_dimensions.split("x")
    if len(parts) == 2:
        try:
            w, h = int(parts[0].strip()), int(parts[1].strip())
            if w >= 3800 or h >= 2100:
                return "4K"
            if w >= 2500 or h >= 1400:
                return "1440p"
            if w >= 1900 or h >= 1000:
                return "1080p"
            if w >= 1200 or h >= 700:
                return "720p"
            if h >= 480:
                return "480p"
        except ValueError:
            pass

    if "3840" in pixel_dimensions or "2160" in pixel_dimensions:
        return "4K"
    if "2560" in pixel_dimensions or "1440" in pixel_dimensions:
        return "1440p"
    if "1920" in pixel_dimensions or "1080" in pixel_dimensions:
        return "1080p"
    if "1280" in pixel_dimensions or "720" in pixel_dimensions:
        return "720p"
    return None


def _parse_video_codec(codec: str, codec_id: str) -> str | None:
    c_low = f"{codec} {codec_id}".lower()
    if "hevc" in c_low or "h.265" in c_low:
        return "HEVC"
    if "avc" in c_low or "h.264" in c_low:
        return "AVC"
    if "av1" in c_low:
        return "AV1"
    if "vp9" in c_low:
        return "VP9"
    return codec or None


_CH_MAP = {8: "7.1", 6: "5.1", 1: "1.0"}


def _parse_audio_track_info(track: dict) -> tuple[str, bool]:
    codec = track.get("codec", "")
    props = track.get("properties") or {}
    codec_id = props.get("codec_id", "")
    c_low = f"{codec} {codec_id}".lower()
    is_flac = "flac" in c_low
    channels = props.get("audio_channels")
    if channels is not None:
        ch_str = _CH_MAP.get(channels, f"{channels}.0" if channels <= 2 else f"{channels}.1")
    else:
        ch_str = None

    if is_flac:
        aud_name = "FLAC"
    elif "aac" in c_low:
        aud_name = "AAC"
    elif "opus" in c_low:
        aud_name = "Opus"
    elif "ac3" in c_low or "ac-3" in c_low:
        aud_name = "AC3"
    elif codec:
        aud_name = codec
    else:
        aud_name = "Audio"

    if ch_str is not None:
        return f"{aud_name} {ch_str}", is_flac
    return aud_name, is_flac


def _extract_mkv_metadata(inspected: InspectedVideo | None) -> dict:
    if not inspected:
        return {
            "resolution": None,
            "video_codec": None,
            "video_tag": None,
            "audio_summary": None,
            "has_flac": None,
            "sub_summary": None,
            "fonts_count": None,
        }

    resolution = None
    video_codec = None
    audio_parts = []
    has_flac = False
    has_audio = False
    sub_langs = []
    sub_formats = []

    for track in inspected.tracks:
        t_type = track.get("type", "")
        props = track.get("properties") or {}

        if t_type == "video":
            video_codec = _parse_video_codec(track.get("codec", ""), props.get("codec_id", ""))
            resolution = _parse_resolution(props.get("pixel_dimensions", ""))
        elif t_type == "audio":
            has_audio = True
            aud_str, flac_found = _parse_audio_track_info(track)
            if flac_found:
                has_flac = True
            audio_parts.append(aud_str)
        elif t_type == "subtitles":
            c_low = f"{track.get('codec', '')} {props.get('codec_id', '')}".lower()
            if "ass" in c_low or "substation" in c_low:
                fmt_name = "ASS"
            elif "srt" in c_low or "subrip" in c_low or "utf8" in c_low:
                fmt_name = "SRT"
            elif "pgs" in c_low or "hdmv" in c_low:
                fmt_name = "PGS"
            elif "vob" in c_low:
                fmt_name = "VobSub"
            else:
                fmt_name = "Subtitles"

            if fmt_name not in sub_formats:
                sub_formats.append(fmt_name)

            lang = (props.get("language") or "und").upper()
            if lang not in sub_langs:
                sub_langs.append(lang)

    video_tag = f"{resolution} {video_codec}" if (resolution and video_codec) else None
    audio_summary = " / ".join(audio_parts) if audio_parts else (None if not has_audio else "No Audio")

    if sub_langs:
        fmt = sub_formats[0] if len(sub_formats) == 1 else "Subs"
        sub_summary = f"{fmt} ({', '.join(sub_langs)})"
    elif sub_formats:
        sub_summary = sub_formats[0] if len(sub_formats) == 1 else "Subtitles"
    else:
        sub_summary = None

    return {
        "resolution": resolution,
        "video_codec": video_codec,
        "video_tag": video_tag,
        "audio_summary": audio_summary,
        "has_flac": has_flac if has_audio else None,
        "sub_summary": sub_summary,
        "fonts_count": inspected.fonts_count(),
    }


# ─── RELEASE PREPARATION ─────────────────────────────────────────────────────
FontsOrigin = Literal["override", "local", "root_fallback", "parent_fallback", "default"]


@dataclass
class PreparedRelease:
    """One Release Directory resolved against its ancestor chain.

    Carries every filesystem-derived fact a planning or execution pass needs:
    the videos to process, the effective fonts directory (with provenance),
    and the Asset Resolver built for this directory. Inspection results are
    deliberately NOT included — each mode probes mkvmerge itself so run mode
    can skip completed outputs first.
    """

    mkv_dir: Path
    videos: list[ParsedFile]
    fonts_dir: Path
    fonts_origin: FontsOrigin
    resolver: AssetResolver


def prepare_release_directory(
    mkv_dir: Path,
    root_input_dir: Path,
    videos: list[ParsedFile],
    *,
    fonts_override: Path | None = None,
    assets_dir: Path | None = None,
    scan_cache: dict[Path, ScanResults] | None = None,
    fonts_dir_cache: dict[Path, Path] | None = None,
    subtitle_prompt: Callable[[ParsedFile, list[Path]], Path | None] | None = None,
) -> PreparedRelease:
    """Resolve one Release Directory's fonts policy and Asset Resolver.

    Single owner of the fonts-directory policy shared by --plan mode and
    remux runs: an explicit override wins, else the nearest ancestor fonts
    directory (shallowest last), else the conventional local 'fonts' folder.
    """
    search_dirs = ancestor_dirs(mkv_dir, root_input_dir)

    if fonts_override is not None:
        fonts_dir = fonts_override
        fonts_origin: FontsOrigin = "override"
    else:
        fonts_dir = mkv_dir / "fonts"
        fonts_origin = "default"
        for d in reversed(search_dirs):
            candidate = find_fonts_dir(d, fonts_dir_cache=fonts_dir_cache)
            if candidate.is_dir():
                fonts_dir = candidate
                if candidate.parent == mkv_dir:
                    fonts_origin = "local"
                elif candidate.parent == root_input_dir:
                    fonts_origin = "root_fallback"
                else:
                    fonts_origin = "parent_fallback"
                break

    resolver = AssetResolver.discover(
        search_dirs,
        fonts_dir,
        assets_dir=assets_dir,
        scan_cache=scan_cache,
        subtitle_prompt=subtitle_prompt,
    )
    return PreparedRelease(
        mkv_dir=mkv_dir,
        videos=videos,
        fonts_dir=fonts_dir,
        fonts_origin=fonts_origin,
        resolver=resolver,
    )


@dataclass
class RemuxRunConfig:
    """Inputs for one release-directory remux pass.

    Per-directory values plus remux-owned engine knobs; process-wide
    infrastructure (worker count, inspect caches, io semaphore, cancel
    token) travels on the embedded CliRuntime.
    """

    runtime: CliRuntime
    root_input_dir: Path
    output_dir: Path
    temp_dir: Path | None
    opus_bitrate: str
    complexity: int
    ffmpeg_threads: int | None
    interaction_mode: InteractionMode = InteractionMode.INTERACTIVE
    sub_assignments: dict[str, Path] | None = None


def _log_fonts_dir(release: PreparedRelease) -> None:
    suffix = {
        "override": " (override)",
        "root_fallback": " (root fallback)",
        "parent_fallback": " (parent fallback)",
    }.get(release.fonts_origin, "")
    logger.info("Fonts folder: %s%s", release.fonts_dir, suffix)


def generate_inspection_manifest(input_dir: Path, persistent_cache: PersistentInspectCache | None = None) -> dict:
    all_mkvs = scan_mkv_files(input_dir)
    if not all_mkvs:
        return {"version": 1, "items": []}

    with ThreadPoolExecutor(max_workers=min(len(all_mkvs), 16)) as pool:
        json_cache = inspect_videos(
            [m.path for m in all_mkvs],
            pool=pool,
            persistent_cache=persistent_cache,
        )

    # Discover external subtitle candidates once per distinct release directory
    # so the UI can offer manual subtitle assignment before a run.
    videos_by_dir: dict[Path, list[ParsedFile]] = {}
    for m in all_mkvs:
        videos_by_dir.setdefault(m.path.parent, []).append(m)

    scan_cache: dict[Path, ScanResults] = {}
    fonts_dir_cache: dict[Path, Path] = {}
    resolver_by_dir: dict[Path, AssetResolver] = {}
    for mkv_dir in sorted(videos_by_dir):
        resolver_by_dir[mkv_dir] = prepare_release_directory(
            mkv_dir,
            input_dir,
            videos_by_dir[mkv_dir],
            scan_cache=scan_cache,
            fonts_dir_cache=fonts_dir_cache,
        ).resolver

    sub_ids_by_dir: dict[Path, list[str]] = {
        mkv_dir: [
            get_item_id(sub_path, input_dir)
            for sub_path in resolver.sub_index.all_subtitles
        ]
        for mkv_dir, resolver in resolver_by_dir.items()
    }

    if persistent_cache is not None:
        persistent_cache.save()

    items = []
    for mkv in all_mkvs:
        meta = _extract_mkv_metadata(json_cache.get(mkv.path))
        item = {
            "id": get_item_id(mkv.path, input_dir),
            "name": mkv.name,
            "metadata": meta,
        }
        candidates = sub_ids_by_dir[mkv.path.parent]
        if candidates:
            item["sub_candidates"] = candidates
        items.append(item)

    return {"version": 1, "items": items}


# ─── ORCHESTRATOR ────────────────────────────────────────────────────────────
def _run(
    release: PreparedRelease,
    config:  RemuxRunConfig,
    pool:    ThreadPoolExecutor,
) -> bool:
    runtime = config.runtime
    dry_run = runtime.dry_run
    mkv_dir = release.mkv_dir
    videos = release.videos
    output_dir = config.output_dir
    temp_dir = config.temp_dir
    resolver = release.resolver
    use_source_fonts = resolver.use_source_fonts

    if not videos:
        logger.warning("No MKV files to process in '%s'.", mkv_dir.name)
        return False

    videos_to_inspect = []
    for v in videos:
        if not v.parsed:
            continue

        out_stem = output_stem(v.stem, v.parsed.ep_type, v.parsed.ep)
        if is_completed_output(output_dir / f"{out_stem}.mkv"):
            continue

        if dry_run and resolver.has_local_subtitle(v.parsed, local_dir=v.path.parent):
            continue

        videos_to_inspect.append(v.path)

    json_cache = inspect_videos(videos_to_inspect, pool, persistent_cache=runtime.persistent_cache, cancel_token=runtime.cancel_token) if videos_to_inspect else {}

    all_subs = resolver.all_subtitles

    jobs, matched_files, skipped_files = _plan_jobs(
        videos, json_cache, resolver,
        output_dir=output_dir, all_subs=all_subs, use_source_fonts=use_source_fonts, dry_run=dry_run,
        root_input_dir=config.root_input_dir,
        sub_assignments=config.sub_assignments,
        mode=config.interaction_mode,
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
        return True

    if config.ffmpeg_threads is None:
        ffmpeg_threads = default_ffmpeg_threads(runtime.workers)
    else:
        ffmpeg_threads = config.ffmpeg_threads
    failed_files: list[str] = []
    _execute_jobs(
        jobs=jobs,
        temp_dir=temp_dir,
        fonts=resolver.fonts,
        opus_bitrate=config.opus_bitrate,
        pool=pool,
        max_workers=runtime.workers,
        ffmpeg_threads=ffmpeg_threads,
        json_cache=json_cache,
        skipped_files=skipped_files,
        failed_files=failed_files,
        complexity=config.complexity,
        io_semaphore=runtime.io_semaphore,
        sub_offset=runtime.sub_offset,
        cancel_token=runtime.cancel_token,
    )
    _print_summary(output_dir, matched_files, skipped_files)
    # Intentional planning skips (no matching assets, output already exists,
    # user declined manual selection) are successful outcomes; only genuine
    # processing failures or worker crashes fail the batch.
    return not failed_files


# ─── MAIN ────────────────────────────────────────────────────────────────────
def main() -> None:
    setup_logging(logging.INFO)

    parser = argparse.ArgumentParser(
        description="Batch MKV Re-Mux Utility for Anime & Fansubs",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    add_shared_args(parser)
    parser.add_argument("--ffmpeg-threads", type=int, default=None, choices=range(1, 65),
                        metavar="[1-64]",
                        help="Maximum CPU threads per ffmpeg audio transcode job (defaults to automatic scaling capped at 8).")
    parser.add_argument("--bitrate", type=str, default="192k",
                        help="Target bitrate for Opus audio conversion (e.g., 128k, 192k).")
    parser.add_argument("--complexity", type=int, default=10, choices=range(11),
                        metavar="[0-10]",
                        help="Opus compression complexity level (0-10, default: 10).")
    parser.add_argument("--sub-offset", type=float, default=0.0,
                        help="Shift subtitle timestamps by this many seconds (negative = earlier, positive = later).")
    parser.add_argument("--out-dir", type=str, default=None,
                        help="Directory where finished MKVs will be saved (defaults to <input-dir>/Output).")
    parser.add_argument("--fonts-dir", type=str, default=None,
                        help="Directory to use for fonts (auto-detects 'fonts'/'Fonts'/"
                             "'attachments' inside --input-dir if not given).")
    parser.add_argument("--assets-dir", type=str, default=None,
                        help="Fallback directory (e.g. a prior extraction's --out-dir) to search for "
                             "subtitles/chapters/covers/fonts when a video has none of its own and none are "
                             "found alongside it. Also searches its immediate parent directory for shared assets.")
    parser.add_argument("--dry-run", action="store_true",
                        help="Show matched files without processing.")
    parser.add_argument("--keep-fonts", action="store_true",
                        help="Keep the extracted fonts folder after processing.")
    parser.add_argument("--io-jobs", type=int, default=2,
                        help="Maximum concurrent disk I/O jobs for mkvmerge. Set to 0 to disable throttling.")
    parser.add_argument("--plan", action="store_true",
                        help="Output inspection manifest as JSON document and exit without processing.")
    parser.add_argument("--sub-assignments", type=str, default=None,
                        help="Path to a JSON file mapping manifest item IDs to subtitle file paths "
                             "(from GUI manual selection). Assigned subtitles take precedence over automatic matching.")
    cfg = parser.parse_args()

    set_events_enabled(cfg.events)

    if cfg.plan:
        setup_logging(logging.WARNING, stderr_only=True)

    handle_clear_cache_flag(cfg.clear_cache)

    opus_bitrate = cfg.bitrate

    bitrate_match = re.match(r"^(\d+)([kM]?)$", opus_bitrate)
    if not bitrate_match or int(bitrate_match.group(1)) == 0:
        logger.error("Invalid bitrate format: '%s'. Must be a positive value like 128k, 192k, 1M.", opus_bitrate)
        sys.exit(1)

    root_input_dir = Path(cfg.input_dir).resolve() if cfg.input_dir else SCRIPT_DIR
    out_override = Path(cfg.out_dir).resolve() if cfg.out_dir else None
    fonts_override = Path(cfg.fonts_dir).resolve() if cfg.fonts_dir else None
    assets_override = Path(cfg.assets_dir).resolve() if cfg.assets_dir else None
    dry_run = cfg.dry_run

    runtime = build_cli_runtime(
        workers=cfg.workers,
        sub_offset=cfg.sub_offset,
        dry_run=dry_run,
        no_cache=cfg.no_cache,
        io_jobs=cfg.io_jobs,
    )
    install_cancel_handlers(runtime.cancel_token)

    sub_assignments: dict[str, Path] | None = None
    if cfg.sub_assignments:
        try:
            raw = json.loads(Path(cfg.sub_assignments).read_text(encoding="utf-8"))
            if not isinstance(raw, dict) or raw.get("version") != 1 or not isinstance(raw.get("assignments"), dict):
                raise ValueError("expected {\"version\": 1, \"assignments\": {<item_id>: <subtitle_path>}}")
            sub_assignments = {}
            for item_id, sub_path in raw["assignments"].items():
                p = Path(str(sub_path))
                sub_assignments[item_id] = p if p.is_absolute() else root_input_dir / p
        except (OSError, ValueError) as exc:
            logger.error("Invalid --sub-assignments file '%s': %s", cfg.sub_assignments, exc)
            sys.exit(1)

    if cfg.plan:
        plan_manifest = generate_inspection_manifest(root_input_dir, runtime.persistent_cache)
        print(json.dumps(plan_manifest, ensure_ascii=False))
        sys.exit(0)

    logger.info("=========================================")
    logger.info(" MKV Batch Re-Mux Utility")
    logger.info(" Anime / Indonesian Fansub Edition")
    logger.info("=========================================")
    logger.info("Workers: %d | Bitrate: %s | Input: %s", runtime.workers, opus_bitrate, root_input_dir)

    check_dependencies("mkvmerge", "ffmpeg")
    logger.info("Using mkvmerge : %s", shutil.which('mkvmerge'))
    logger.info("Using ffmpeg   : %s", shutil.which('ffmpeg'))

    mkvs_by_dir = collect_release_dirs(
        root_input_dir,
        ignore_paths={out_override} if out_override else None,
    )

    emit_batch_start(
        total=sum(len(files) for files in mkvs_by_dir.values()),
        input_dir=str(root_input_dir),
    )

    scan_cache: dict[Path, ScanResults] = {}
    fonts_dir_cache: dict[Path, Path] = {}
    interactive_prompt = _prompt_for_subtitle if sys.stdin.isatty() else None
    if dry_run:
        interaction_mode = InteractionMode.DRY_RUN
    elif interactive_prompt is not None:
        interaction_mode = InteractionMode.INTERACTIVE
    else:
        interaction_mode = InteractionMode.NON_INTERACTIVE

    def process_directory(pool: ThreadPoolExecutor, mkv_dir: Path, dir_mkvs: list[ParsedFile]) -> bool:
        current_output_dir = resolve_output_dir(mkv_dir, root_input_dir, out_override)
        if not dry_run:
            current_output_dir.mkdir(parents=True, exist_ok=True)

        release = prepare_release_directory(
            mkv_dir,
            root_input_dir,
            dir_mkvs,
            fonts_override=fonts_override,
            assets_dir=resolve_output_dir(mkv_dir, root_input_dir, assets_override, default_subdir=None) if assets_override else None,
            scan_cache=scan_cache,
            fonts_dir_cache=fonts_dir_cache,
            subtitle_prompt=interactive_prompt,
        )
        _log_fonts_dir(release)

        temp_dir = None
        if not dry_run:
            temp_dir = Path(tempfile.mkdtemp(prefix=".tmp_remux_", dir=current_output_dir))

        try:
            config = RemuxRunConfig(
                runtime=runtime,
                root_input_dir=root_input_dir,
                output_dir=current_output_dir,
                temp_dir=temp_dir,
                opus_bitrate=opus_bitrate,
                complexity=cfg.complexity,
                ffmpeg_threads=cfg.ffmpeg_threads,
                interaction_mode=interaction_mode,
                sub_assignments=sub_assignments,
            )
            dir_success = _run(release, config, pool)
        finally:
            if temp_dir is not None:
                shutil.rmtree(temp_dir, ignore_errors=True)

        if (
            dir_success
            and not dry_run
            and not cfg.keep_fonts
            and release.fonts_origin == "local"
            and release.fonts_dir.name == "fonts"
        ):
            try:
                shutil.rmtree(release.fonts_dir)
                logger.info("Cleanup: Removed temporary fonts folder '%s'", release.fonts_dir.name)
            except OSError as e:
                logger.error("Failed to remove fonts folder: %s", e)

        return dir_success

    try:
        overall_success = run_batch(runtime, mkvs_by_dir, process_directory)
    except CancelledError:
        logger.warning("Cancelled by user.")
        overall_success = False

    cancelled = runtime.cancel_token.cancelled

    emit_batch_complete(success=overall_success and not cancelled, cancelled=cancelled)

    if cancelled:
        sys.exit(130)

    if not overall_success:
        sys.exit(1)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        logger.warning("Cancelled by user.")
        sys.exit(130)
