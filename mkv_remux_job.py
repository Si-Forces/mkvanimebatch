"""Remux track selection, command construction, and transactional execution."""
from __future__ import annotations

import logging
import re
import shutil
import subprocess
import tempfile
import threading
import uuid
from concurrent.futures import ThreadPoolExecutor, as_completed, wait
from dataclasses import dataclass, field
from pathlib import Path

from mkv_domain import COVER_MIME, is_dialogue_track
from mkv_events import (
    OUTCOME_CANCELLED,
    OUTCOME_COMPLETED,
    OUTCOME_ERROR,
    emit_item_complete,
    emit_item_start,
)
from mkv_fonts import FontInfo
from mkv_proc import SUBPROCESS_TIMEOUT, CancelledError, CancelToken, acquire_semaphore, run_subprocess_cancellable
from mkv_scan import AudioTrack, InspectedVideo, InspectionResult

logger = logging.getLogger("mkv_automation.remux")


def mkv_success(result: subprocess.CompletedProcess[str]) -> bool:
    return result.returncode <= 1


@dataclass(frozen=True)
class RemuxExecutionContext:
    opus_bitrate: str
    ffmpeg_threads: int
    io_semaphore: threading.Semaphore | None
    complexity: int
    sub_offset: float
    cancel_token: CancelToken
    workers: int


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
    source_font_ids:     list[int] = field(default_factory=list[int])


def select_tracks(inspected: InspectedVideo) -> TrackSelection:
    has_src_chapters = inspected.has_chapters()
    cover = inspected.cover_attachment()
    cover_id = cover.get("id") if cover is not None else None
    source_cover_id = cover_id
    source_font_ids = [attachment_id for attachment in inspected.font_attachments()
                       if (attachment_id := attachment.get("id")) is not None]

    video_track = inspected.video_track
    video_id = video_track.get("id") if video_track is not None else None
    video_pos = inspected.video_track_pos

    audio_tracks = inspected.audio_tracks()

    audio, reason = _pick_audio(audio_tracks, video_pos)
    selected_audio: AudioTrack = audio or {
        "id": None,
        "pos": 0,
        "audio_idx": 0,
        "codec": "",
        "channels": 0,
        "lang": "",
        "name": "",
    }

    return TrackSelection(
        video_id=video_id,
        audio_id=selected_audio["id"],
        audio_index=selected_audio["audio_idx"],
        audio_codec=selected_audio["codec"],
        audio_channels=selected_audio["channels"],
        audio_lang=selected_audio["lang"],
        audio_name=selected_audio["name"],
        audio_reason=reason,
        has_source_chapters=has_src_chapters,
        source_cover_id=source_cover_id,
        source_font_ids=source_font_ids,
    )


_COMMENTARY_RE = re.compile(r"\b(commentary|comment|audio\s+description|ad|extension)\b", re.IGNORECASE)


def _pick_audio(audio_tracks: list[AudioTrack], video_pos: int) -> tuple[AudioTrack | None, str]:
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

    candidates: list[tuple[tuple[int, int], AudioTrack]] = []
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
    fonts:             FontInfo = FontInfo(args=(), count=0)
    release_dir:       Path | None = None


@dataclass
class ExecutionSummary:
    """All non-success outcomes: display lines for the batch log, plus per-job
    records so a multi-release batch can attribute results to each release."""

    skipped_files: list[str] = field(default_factory=list[str])
    failed_files: list[str] = field(default_factory=list[str])
    job_skips: list[tuple[PlannedJob, str]] = field(default_factory=list[tuple[PlannedJob, str]])
    failed_jobs: list[PlannedJob] = field(default_factory=list[PlannedJob])



@dataclass
class JobResult:
    log:            list[str] = field(default_factory=list[str])
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
    complexity: int = 5,
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
    job: PlannedJob, opus_bitrate: str,
    inspection_results: dict[Path, InspectionResult], ffmpeg_threads: int = 2,
    io_semaphore: threading.Semaphore | None = None,
    complexity: int = 5,
    sub_offset: float = 0.0,
    cancel_token: CancelToken | None = None,
) -> JobResult:
    result = JobResult()
    video, video_name, base = job.video, job.video_name, job.base
    output_file = job.output_file
    fonts = job.fonts
    scratch_dir: Path | None = None
    temp_opus: Path | None = None

    if cancel_token is not None and cancel_token.cancelled:
        result.cancelled = True
        result.log.append(f"Processing: {video_name}")
        result.log.append("  !! CANCELLED before start.")
        emit_item_complete(job.item_id, video_name, OUTCOME_CANCELLED)
        return result

    emit_item_start(job.item_id, video_name)

    result.log.append(f"Processing: {video_name}")
    result.log.append("  Inspecting tracks...")

    def _error(reason: str, detail: str) -> JobResult:
        result.log.append(f"  !! ERROR: {video_name}")
        result.log.append(f"  !! Detail : {detail}")
        result.failed = True
        result.error_reason = reason
        emit_item_complete(job.item_id, video_name, OUTCOME_ERROR, error=reason)
        return result

    inspection = inspection_results.get(video)
    if inspection is None or inspection.video is None:
        reason = inspection.error if inspection is not None and inspection.error else "mkvmerge inspection failed"
        return _error(reason, reason)

    inspected = inspection.video
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
        with acquire_semaphore(io_semaphore, cancel_token):
            # Scratch space beside the final output so the promote below is an
            # atomic same-filesystem rename.
            scratch_dir = Path(tempfile.mkdtemp(prefix=".tmp_remux_", dir=output_file.parent))
            needs_conversion = selection.audio_id is not None and selection.audio_codec == "A_FLAC"
            if needs_conversion:
                temp_opus = _convert_flac_to_opus(
                    video=video,
                    audio_index=selection.audio_index,
                    temp_dir=scratch_dir,
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
                result.log.append("  Chapters       : none found in directory or source")

            if job.cover_file:
                result.log.append(f"  Cover          : {job.cover_file.name} (external, attached)")
            elif selection.source_cover_id is not None:
                result.log.append("  Cover          : keeping source's embedded cover (no external match found)")
            else:
                result.log.append("  Cover          : none found in directory or source")

            temp_output = scratch_dir / job.output_file.name
            args = _build_mkvmerge_args(job, selection, temp_opus, fonts, temp_output, sub_offset=sub_offset)
            result.log.append("  Running mkvmerge...")

            try:
                run_result = run_subprocess_cancellable(["mkvmerge", *args], cancel_token=cancel_token, timeout=SUBPROCESS_TIMEOUT, capture_stdout=False)
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

        if cancel_token is not None:
            cancel_token.check()
        temp_output.replace(output_file)
        result.log.append("  Completed successfully.")
        emit_item_complete(job.item_id, video_name, OUTCOME_COMPLETED)

    except CancelledError:
        result.cancelled = True
        emit_item_complete(job.item_id, video_name, OUTCOME_CANCELLED)
        raise

    finally:
        if scratch_dir is not None:
            shutil.rmtree(scratch_dir, ignore_errors=True)
    return result
# ─── JOB EXECUTOR ORCHESTRATOR ────────────────────────────────────────────────
def execute_jobs(
    jobs: list[PlannedJob],
    pool: ThreadPoolExecutor,
    inspection_results: dict[Path, InspectionResult],
    context: RemuxExecutionContext,
) -> ExecutionSummary:
    """Execute jobs and return all non-success outcomes."""
    summary = ExecutionSummary()
    logger.info("Processing %d matched file(s) (up to %d at a time)...", len(jobs), context.workers)

    def record_skip(skipped_job: PlannedJob, line: str) -> None:
        summary.skipped_files.append(line)
        summary.job_skips.append((skipped_job, line))

    def record_failure(failed_job: PlannedJob, line: str) -> None:
        summary.failed_files.append(failed_job.video_name)
        summary.failed_jobs.append(failed_job)
        record_skip(failed_job, line)

    total_jobs = len(jobs)
    futures = {
        pool.submit(
            process_job,
            job=job,
            opus_bitrate=context.opus_bitrate,
            inspection_results=inspection_results,
            ffmpeg_threads=context.ffmpeg_threads,
            io_semaphore=context.io_semaphore,
            complexity=context.complexity,
            sub_offset=context.sub_offset,
            cancel_token=context.cancel_token,
        ): job
        for job in jobs
    }
    for completed_jobs, future in enumerate(as_completed(futures), start=1):
        job = futures[future]
        progress = completed_jobs / total_jobs
        try:
            result = future.result()
            if result.cancelled:
                record_skip(job, f"{job.video_name} (cancelled)")
                status = "Cancelled"
            elif result.failed or result.error_reason:
                record_failure(job, f"{job.video_name} (failed: {result.error_reason or 'error'})")
                status = "Failed"
            elif result.skipped_reason:
                record_skip(job, f"{job.video_name} ({result.skipped_reason})")
                status = "Skipped"
            else:
                status = "Remuxed"
            logger.info(
                "[%d/%d (%.0f%%)] %s: %s",
                completed_jobs,
                total_jobs,
                progress * 100,
                status,
                job.video_name,
            )
        except CancelledError:
            record_skip(job, f"{job.video_name} (cancelled)")
            logger.info(
                "[%d/%d (%.0f%%)] Cancelled: %s",
                completed_jobs,
                total_jobs,
                progress * 100,
                job.video_name,
            )
            for future_to_cancel, pending_job in futures.items():
                if future_to_cancel.cancel():
                    emit_item_complete(
                        pending_job.item_id,
                        pending_job.video_name,
                        OUTCOME_CANCELLED,
                    )
            wait(futures)
            raise
        except Exception as exc:
            logger.exception(
                "[%d/%d (%.0f%%)] CRASHED: %s",
                completed_jobs,
                total_jobs,
                progress * 100,
                job.video_name,
            )
            emit_item_complete(job.item_id, job.video_name, OUTCOME_ERROR, error=str(exc))
            record_failure(job, f"{job.video_name} (exception: {exc})")

    return summary
