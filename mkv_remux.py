#!/usr/bin/env python3
"""
Batch MKV Re-Mux Utility
Optimized for anime & Indonesian fansub releases — including OVA, OAD, ONA, SP.

Recursively finds MKVs under --input-dir and remuxes each with its best-
matching subtitle (external .ass, embedded fallback, or interactive pick),
chapter XML, and cover image — searched from the video's own directory up to
--input-dir, so a shared season/show-level directory is found automatically.
Prefers stereo Japanese audio, converting FLAC to Opus via ffmpeg. By
default, a local "fonts" directory is deleted after a successful run (pass
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
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path

from mkv_assets import (
    AssetResolver,
    InteractionMode,
    PreparedRelease,
    claim_leftover_chapters,
    prepare_release_directory,
)
from mkv_domain import (
    default_ffmpeg_threads,
    ep_type_label,
    output_stem,
    resolve_output_dir,
)
from mkv_events import (
    OUTCOME_CANCELLED,
    OUTCOME_PLANNED,
    OUTCOME_SKIPPED,
    emit_batch_complete,
    emit_batch_start,
    emit_item_complete,
    get_item_id,
    has_terminal_outcome,
    set_events_enabled,
    setup_logging,
)
from mkv_manifest import generate_inspection_manifest
from mkv_proc import CancelledError
from mkv_remux_job import PlannedJob, RemuxExecutionContext, execute_jobs
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
    InspectionResult,
    ParsedFile,
    ScanResults,
    check_dependencies,
    inspect_videos,
)
from mkv_storage import resolve_io_jobs

# Initialize child logger for the remux module
logger = logging.getLogger("mkv_automation.remux")




# ─── HELPERS ─────────────────────────────────────────────────────────────────
def log_skip(video_name: str, reason: str) -> None:
    logger.warning("SKIPPED: %s | Reason: %s", video_name, reason)




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
    try:
        choice = input("\n  Type a number to assign a subtitle, or 0 to skip: ").strip()
    except EOFError:
        # stdin closed mid-prompt (e.g. detached run): treat as a user skip.
        return None
    if choice.isdigit() and 0 < int(choice) <= len(all_subs):
        return all_subs[int(choice) - 1]
    return None




# ─── JOB PLANNING ────────────────────────────────────────────────────────────
def _plan_jobs(
    videos:           list[ParsedFile],
    inspection_results: dict[Path, InspectionResult],
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

    chapter_claims = claim_leftover_chapters(videos, inspection_results, resolver)

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

        inspection = inspection_results.get(video)
        inspected = inspection.video if inspection is not None else None
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
    sub_offset: float = 0.0
    interaction_mode: InteractionMode = InteractionMode.INTERACTIVE
    sub_assignments: dict[str, Path] | None = None


def _log_fonts_dir(release: PreparedRelease) -> None:
    suffix = {
        "override": " (override)",
        "root_fallback": " (root fallback)",
        "parent_fallback": " (parent fallback)",
    }.get(release.fonts_origin, "")
    logger.info("Fonts directory: %s%s", release.fonts_dir, suffix)




# ─── ORCHESTRATOR ────────────────────────────────────────────────────────────
def _effective_ffmpeg_threads(config_threads: int | None, runtime: CliRuntime) -> int:
    """Per-job ffmpeg thread budget.

    An explicit --ffmpeg-threads always wins; otherwise CPUs must be divided
    across jobs that can actually run concurrently (io-jobs), not the nominal
    worker count — otherwise the budget collapses to 1 thread per conversion
    while the io semaphore idles the remaining cores.
    """
    if config_threads is not None:
        return config_threads
    return default_ffmpeg_threads(runtime.effective_concurrency)


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

    inspection_results = inspect_videos(videos_to_inspect, pool, persistent_cache=runtime.persistent_cache, cancel_token=runtime.cancel_token) if videos_to_inspect else {}

    all_subs = resolver.all_subtitles

    jobs, matched_files, skipped_files = _plan_jobs(
        videos, inspection_results, resolver,
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

    execution = execute_jobs(
        jobs,
        pool,
        inspection_results,
        RemuxExecutionContext(
            temp_dir=config.temp_dir,
            fonts=resolver.fonts,
            opus_bitrate=config.opus_bitrate,
            ffmpeg_threads=_effective_ffmpeg_threads(config.ffmpeg_threads, runtime),
            io_semaphore=runtime.io_semaphore,
            complexity=config.complexity,
            sub_offset=config.sub_offset,
            cancel_token=runtime.cancel_token,
            workers=runtime.workers,
        ),
    )
    skipped_files.extend(execution.skipped_files)
    _print_summary(output_dir, matched_files, skipped_files)
    # Intentional planning skips (no matching assets, output already exists,
    # user declined manual selection) are successful outcomes; only genuine
    # processing failures or worker crashes fail the batch.
    return not execution.failed_files


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
    parser.add_argument("--complexity", type=int, default=5, choices=range(11),
                        metavar="[0-10]",
                        help="Opus compression complexity level (0-10, default: 5 — roughly 2x faster "
                             "than 10 with near-identical quality at typical bitrates).")
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
                        help="Keep the extracted fonts directory after processing.")
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

    io_jobs = resolve_io_jobs(cfg.io_jobs, root_input_dir)

    runtime = build_cli_runtime(
        workers=cfg.workers,
        dry_run=dry_run,
        no_cache=cfg.no_cache,
        io_jobs=io_jobs,
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
        try:
            plan_manifest = generate_inspection_manifest(
                root_input_dir,
                persistent_cache=runtime.persistent_cache,
                cancel_token=runtime.cancel_token,
            )
        except CancelledError:
            logger.warning("Cancelled by user.")
            sys.exit(130)
        print(json.dumps(plan_manifest, ensure_ascii=False))
        sys.exit(0)

    logger.info("=========================================")
    logger.info(" MKV Batch Re-Mux Utility")
    logger.info(" Anime / Indonesian Fansub Edition")
    logger.info("=========================================")
    logger.info("Workers: %d | IO jobs: %s | Bitrate: %s | Input: %s",
                runtime.workers, runtime.io_jobs or "unlimited", opus_bitrate, root_input_dir)

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
                sub_offset=cfg.sub_offset,
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
                logger.info("Cleanup: Removed temporary fonts directory '%s'", release.fonts_dir.name)
            except OSError as e:
                logger.error("Failed to remove fonts directory: %s", e)

        return dir_success

    try:
        overall_success = run_batch(runtime, mkvs_by_dir, process_directory)
    except CancelledError:
        logger.warning("Cancelled by user.")
        for files in mkvs_by_dir.values():
            for mkv_file in files:
                item_id = get_item_id(mkv_file.path, root_input_dir)
                if has_terminal_outcome(item_id):
                    continue
                emit_item_complete(
                    item_id,
                    mkv_file.name,
                    OUTCOME_CANCELLED,
                )
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
    except (KeyboardInterrupt, CancelledError):
        logger.warning("Cancelled by user.")
        sys.exit(130)
