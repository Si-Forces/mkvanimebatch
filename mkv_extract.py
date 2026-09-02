#!/usr/bin/env python3
"""
MKV Track & Font Extractor
Optimized for anime & Indonesian fansub releases — including OVA, OAD, ONA, SP.

Extracts, per MKV: best-match ASS subtitle, chapters (if present), the
source's front-cover attachment (if any), and any embedded fonts (deduped
across files into a shared fonts/ directory).

Requires: mkvmerge, mkvextract (MKVToolNix)

NOTE: Concurrent invocations on the same output tree are unsupported.
"""
from __future__ import annotations

import argparse
import logging
import shutil
import subprocess
import sys
import tempfile
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed, wait
from dataclasses import dataclass
from pathlib import Path
from typing import NamedTuple

from mkv_domain import (
    COVER_EXTENSIONS,
    EpisodeInfo,
    ep_type_label,
    resolve_output_dir,
    select_best_subtitle_track,
)
from mkv_events import (
    OUTCOME_CANCELLED,
    OUTCOME_COMPLETED,
    OUTCOME_ERROR,
    OUTCOME_PLANNED,
    emit_batch_complete,
    emit_batch_start,
    emit_item_complete,
    emit_item_start,
    get_item_id,
    has_terminal_outcome,
    set_events_enabled,
    setup_logging,
)
from mkv_fonts import FontClaim, FontHashMemo, FontManager
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
    log_directory_banner,
)
from mkv_scan import (
    InspectedVideo,
    ParsedFile,
    check_dependencies,
    get_default_font_hash_memo_file,
    inspect_videos,
)
from mkv_storage import resolve_io_jobs

SUPPORTED_SUB_CODECS = frozenset({"s_text/ass"})

# Initialize local child logger
logger = logging.getLogger("mkv_automation.extract")


# ─── SUBTITLE SELECTION ──────────────────────────────────────────────────────
class ExtractTarget(NamedTuple):
    """One queued mkvextract output: how to address it and where it lands."""

    cmd_type: str  # "tracks" | "chapters" | "attachments"
    arg_id: str | None  # track/attachment id; None for whole-file outputs
    final_path: Path
    item_name: str


def _episode_suffix(parsed: EpisodeInfo | None, file_stem: str) -> str:
    """Filename segment identifying the episode; falls back to the raw stem
    when the file name carried no parseable Episode Info."""
    if parsed is None:
        return file_stem
    return f"{ep_type_label(parsed.ep_type)}{parsed.ep}"


def select_subtitle(
    inspected: InspectedVideo,
    file_stem: str,
    file_dir:  Path,
    parsed:    EpisodeInfo | None,
    log:       list[str],
) -> tuple[str, Path] | None:
    """Pick the best ASS subtitle track and build its output path."""
    result = select_best_subtitle_track(
        inspected.subtitle_tracks(),
        codec_filter=SUPPORTED_SUB_CODECS,
        log=log,
    )
    if result is None:
        return None
    track_id, language, _ = result
    sub_path = file_dir / f"{file_stem}_{_episode_suffix(parsed, file_stem)}_{language}_track{track_id}.ass"
    return str(track_id), sub_path


# ─── EXTRACTION HELPERS ──────────────────────────────────────────────────────
def _run_mkvextract(
    mkv_path: Path,
    args: list[str],
    log: list[str],
    io_semaphore: threading.Semaphore | None = None,
    cancel_token: CancelToken | None = None,
) -> None:
    """Run a single mkvextract command with mixed modes (tracks, chapters, attachments)."""
    if not args:
        log.append("   -> Nothing to extract.")
        return

    try:
        with acquire_semaphore(io_semaphore, cancel_token):
            result = run_subprocess_cancellable(
                ["mkvextract", str(mkv_path), *args],
                capture_stdout=False,
                timeout=SUBPROCESS_TIMEOUT,
                cancel_token=cancel_token,
            )
    except subprocess.TimeoutExpired:
        log.append(f"   -> ERROR: mkvextract timed out after {SUBPROCESS_TIMEOUT}s for '{mkv_path.name}'.")
        raise RuntimeError(
            f"mkvextract timed out after {SUBPROCESS_TIMEOUT}s for '{mkv_path.name}'"
        )
    if result.returncode > 1:
        raise RuntimeError(
            f"mkvextract failed (exit {result.returncode}): "
            f"{result.stderr.strip()}"
        )


def _clean_empty_file(path: Path, log: list[str], item_name: str) -> None:
    """OS-safe removal of empty files post-extraction."""
    if path.exists() and path.stat().st_size == 0:
        path.unlink(missing_ok=True)
        log.append(f"   -> WARNING: {item_name} extraction produced an empty file, removed.")


# ─── PER-FILE WORKER ─────────────────────────────────────────────────────────
def process_file(
    mkv_file:            ParsedFile,
    inspected:           InspectedVideo | None,
    font_manager:        FontManager,
    io_semaphore:        threading.Semaphore | None = None,
    output_dir:          Path | None = None,
    root_input_dir:      Path | None = None,
    cancel_token:        CancelToken | None = None,
) -> tuple[Path, list[str], bool]:
    mkv_path = mkv_file.path
    root_dir = root_input_dir or mkv_path.parent
    item_id = get_item_id(mkv_path, root_dir)
    log: list[str] = [f"Processing: {mkv_path.name}"]

    if cancel_token is not None and cancel_token.cancelled:
        log.append("  !! CANCELLED before start.")
        emit_item_complete(item_id, mkv_path.name, OUTCOME_CANCELLED)
        return mkv_path, log, False

    emit_item_start(item_id, mkv_path.name)

    if inspected is None:
        log.append(f"  ERROR: mkvmerge inspection failed for '{mkv_path.name}'.")
        emit_item_complete(item_id, mkv_path.name, OUTCOME_ERROR, error="mkvmerge inspection failed")
        return mkv_path, log, False

    file_stem = mkv_path.stem
    file_dir  = output_dir if output_dir is not None else mkv_path.parent

    parsed = mkv_file.parsed
    if parsed:
        chapter_stem = f"{file_stem}_{_episode_suffix(parsed, file_stem)}"
        log.append(f"   -> Type: {ep_type_label(parsed.ep_type)} {parsed.ep}")
    else:
        chapter_stem = file_stem
        log.append(
            "   -> WARNING: Could not parse episode number; "
            f"subtitle tag will use stem '{file_stem}'."
        )

    queued_outputs: list[ExtractTarget] = []

    sub_result = select_subtitle(inspected, file_stem, file_dir, parsed, log)
    if sub_result:
        track_id, sub_path = sub_result
        log.append(f"   -> Queued subtitle track {track_id} → {sub_path.name}")
        queued_outputs.append(ExtractTarget("tracks", str(track_id), sub_path, "Subtitle"))
    else:
        log.append("   -> No suitable ASS subtitle tracks found.")

    chapter_path: Path | None = None
    if inspected.has_chapters():
        chapter_path = file_dir / f"{chapter_stem}_chapters.xml"
        log.append(f"   -> Queued chapters → {chapter_path.name}")
        queued_outputs.append(ExtractTarget("chapters", None, chapter_path, "Chapter"))
    else:
        log.append("   -> No chapters found in source.")

    cover_path: Path | None = None
    cover = inspected.cover_attachment()
    font_attachments = inspected.font_attachments()
    cover_id = cover.get("id") if cover is not None else None
    if cover is not None and cover_id is not None:
        raw_ext = Path(cover.get("file_name", "")).suffix.lower()
        ext = raw_ext if raw_ext in COVER_EXTENSIONS else ".jpg"
        cover_path = file_dir / f"{file_stem}_cover{ext}"
        if cover_path.exists():
            log.append(f"   -> Cover already exists ({cover_path.name}), skipping.")
            cover_path = None
        else:
            log.append(f"   -> Queued cover image → {cover_path.name}")
            queued_outputs.append(ExtractTarget("attachments", str(cover_id), cover_path, "Cover"))
    else:
        log.append("   -> No cover image found in source.")

    temp_dir = None
    font_claims: list[FontClaim] = []
    try:
        temp_dir = Path(tempfile.mkdtemp(prefix=".tmp_extract_", dir=file_dir))
        extract_args: list[str] = []
        staged_outputs: list[tuple[Path, ExtractTarget]] = []
        for target in queued_outputs:
            staged_path = temp_dir / target.final_path.name
            if target.arg_id is not None:
                extract_args += [target.cmd_type, f"{target.arg_id}:{staged_path}"]
            else:
                extract_args += [target.cmd_type, str(staged_path)]
            staged_outputs.append((staged_path, target))

        font_args: list[str] = []
        attachments_open = cover_path is not None

        if font_attachments:
            for attachment in font_attachments:
                attachment_id = attachment.get("id")
                file_name = attachment.get("file_name")
                if attachment_id is None or not file_name:
                    continue
                claim = font_manager.claim(file_name, attachment.get("size", 0))
                if claim is not None:
                    font_claims.append(claim)
                    font_args.append(f"{attachment_id}:{claim.staging_path}")

        if font_args:
            log.append(f"   -> Queued {len(font_args)} font(s) for extraction...")
            if not attachments_open:
                extract_args += ["attachments"]
            extract_args += font_args

        log.append("   -> Running mkvextract...")
        _run_mkvextract(
            mkv_path,
            extract_args,
            log,
            io_semaphore=io_semaphore,
            cancel_token=cancel_token,
        )

        if cancel_token is not None:
            cancel_token.check()
        for staged_path, target in staged_outputs:
            _clean_empty_file(staged_path, log, target.item_name)
            if not staged_path.exists():
                continue
            staged_path.replace(target.final_path)

        saved_count = 0
        for claim in font_claims:
            promoted_path = font_manager.promote(claim)
            if promoted_path:
                saved_count += 1
                log.append(f"   -> Saved font: {promoted_path.name}")
        if font_claims:
            log.append(f"   -> Extracted {saved_count} new font(s).")

    except CancelledError:
        for claim in font_claims:
            font_manager.release(claim)
        emit_item_complete(item_id, mkv_path.name, OUTCOME_CANCELLED)
        raise
    except BaseException:
        for claim in font_claims:
            font_manager.release(claim)
        raise
    finally:
        if temp_dir is not None:
            shutil.rmtree(temp_dir, ignore_errors=True)

    log.append(f"Done: {mkv_path.name}")
    emit_item_complete(item_id, mkv_path.name, OUTCOME_COMPLETED)
    return mkv_path, log, True


# ─── RUN ORCHESTRATOR ────────────────────────────────────────────────────────
@dataclass
class ExtractRunConfig:
    """Inputs for one whole extract batch run; process-wide infrastructure
    (worker count, dry-run, inspect cache, io semaphore, cancel token)
    travels on the embedded CliRuntime."""

    runtime:         CliRuntime
    root_input_dir:  Path
    out_override:    Path | None = None
    fonts_override:  Path | None = None
    font_hash_memo:  FontHashMemo | None = None


@dataclass(frozen=True)
class _ExtractRelease:
    """Immutable per-release task record produced by the sequential prep pass."""

    mkv_dir:      Path
    mkv_files:    tuple[ParsedFile, ...]
    output_dir:   Path
    fonts_dir:    Path
    font_manager: FontManager


def _fonts_dir_for(mkv_dir: Path, config: ExtractRunConfig) -> Path:
    if config.fonts_override:
        return config.fonts_override
    if config.out_override:
        return config.out_override / "fonts"
    return mkv_dir / "fonts"


def _run_pipeline(
    mkvs_by_dir: dict[Path, list[ParsedFile]],
    config:      ExtractRunConfig,
) -> bool:
    """Prepare every release sequentially, inspect every source once, then
    execute every file through one global bounded queue. Per-file logs are
    buffered and printed grouped per release in stable source order after the
    queue drains; staging cleanup for every FontManager is deferred until
    then. Raises CancelledError on batch cancellation.
    """
    runtime = config.runtime
    token = runtime.cancel_token
    root_dir = config.root_input_dir
    any_failures = False

    # Managers are keyed by fonts destination so releases whose outputs share
    # a destination share one manager (and dedup fonts across releases);
    # distinct local fonts directories keep distinct managers. Creation stays
    # on this sequential pass because seeding (and the shared hash memo) is
    # not thread-safe.
    font_managers: dict[Path, FontManager] = {}

    try:
        with ThreadPoolExecutor(max_workers=runtime.workers) as pool:
            # Phase 1: sequential prep — output dirs and FontManagers up front.
            prepared: list[_ExtractRelease] = []
            for mkv_dir, dir_mkvs in mkvs_by_dir.items():
                if token.cancelled:
                    logger.warning("Cancellation requested — skipping remaining directories.")
                    raise CancelledError("Cancellation requested between release directories")

                output_dir = resolve_output_dir(mkv_dir, root_dir, config.out_override, default_subdir=None)

                if not dir_mkvs:
                    logger.warning("No MKV files found in '%s', skipping.", output_dir.name)
                    any_failures = True
                    continue

                if runtime.dry_run:
                    log_directory_banner(mkv_dir)
                    logger.info("MKV files discovered: %d", len(dir_mkvs))
                    logger.info("--- DRY RUN: Files that would be processed ---")
                    for f in dir_mkvs:
                        emit_item_complete(get_item_id(f.path, root_dir), f.name, OUTCOME_PLANNED)
                        logger.info("  %s", f.name)
                    logger.info("Total: %d file(s). No extraction performed.", len(dir_mkvs))
                    continue

                output_dir.mkdir(parents=True, exist_ok=True)
                fonts_dir = _fonts_dir_for(mkv_dir, config)
                font_manager = font_managers.get(fonts_dir)
                if font_manager is None:
                    fonts_dir.mkdir(parents=True, exist_ok=True)
                    font_manager = FontManager(fonts_dir, hash_memo=config.font_hash_memo)
                    font_managers[fonts_dir] = font_manager

                prepared.append(_ExtractRelease(
                    mkv_dir=mkv_dir,
                    mkv_files=tuple(dir_mkvs),
                    output_dir=output_dir,
                    fonts_dir=fonts_dir,
                    font_manager=font_manager,
                ))

            if runtime.dry_run or not prepared:
                return not any_failures

            # Phase 2: one batch inspection across every release.
            inspection_results = inspect_videos(
                [f.path for release in prepared for f in release.mkv_files],
                pool,
                persistent_cache=runtime.persistent_cache,
                cancel_token=token,
                probe_semaphore=runtime.io_semaphore,
            )
            runtime.persistent_cache.save()

            # Phase 3: one global bounded execution queue for every file.
            total_files = sum(len(release.mkv_files) for release in prepared)
            logger.info("Processing %d file(s) (up to %d at a time)...", total_files, runtime.workers)

            results: dict[Path, list[str]] = {}
            futures = {
                pool.submit(
                    process_file,
                    mkv_file,
                    inspection_results[mkv_file.path].video,
                    release.font_manager,
                    io_semaphore=runtime.io_semaphore,
                    output_dir=release.output_dir,
                    root_input_dir=root_dir,
                    cancel_token=token,
                ): mkv_file
                for release in prepared
                for mkv_file in release.mkv_files
            }

            for completed_files, future in enumerate(as_completed(futures), start=1):
                mkv_file = futures[future]
                mkv_path = mkv_file.path
                progress = completed_files / total_files

                try:
                    _, log, ok = future.result()
                    results[mkv_path] = log
                    if not ok:
                        any_failures = True

                    logger.info(
                        "[%d/%d (%.0f%%)] %s: %s",
                        completed_files,
                        total_files,
                        progress * 100,
                        "Extracted" if ok else "Skipped",
                        mkv_path.name,
                    )

                except CancelledError:
                    for pending, pending_file in futures.items():
                        if pending.cancel():
                            emit_item_complete(
                                get_item_id(pending_file.path, root_dir),
                                pending_file.name,
                                OUTCOME_CANCELLED,
                            )
                    wait(futures)
                    raise
                except Exception as exc:
                    any_failures = True
                    logger.exception(
                        "[%d/%d (%.0f%%)] CRASHED: %s",
                        completed_files, total_files, progress * 100, mkv_path.name,
                    )
                    item_id = get_item_id(mkv_path, root_dir)
                    emit_item_complete(item_id, mkv_path.name, OUTCOME_ERROR, error=str(exc))
                    results[mkv_path] = [
                        f"Processing: {mkv_path.name}",
                        "  WARNING: Worker crashed. See traceback above.",
                    ]

            # Phase 4: grouped per-release logs, stable source order.
            for release in prepared:
                log_directory_banner(release.mkv_dir)
                logger.info("MKV files discovered: %d", len(release.mkv_files))
                for mkv_file in release.mkv_files:
                    logger.info("-" * 60)
                    for line in results.get(mkv_file.path, []):
                        stripped = line.lstrip()
                        if stripped.startswith("WARNING"):
                            logger.warning(line)
                        elif stripped.startswith("ERROR"):
                            logger.error(line)
                        else:
                            logger.info(line)
                logger.info("=========================================")
                logger.info(" All files processed.")
                logger.info("=========================================")

            return not any_failures
    finally:
        # Staging directories span the whole batch; remove them once here,
        # strictly after the queue drains.
        for font_manager in font_managers.values():
            font_manager.cleanup_staging()
        runtime.persistent_cache.save()


def main() -> None:
    setup_logging(logging.INFO)

    parser = argparse.ArgumentParser(
        description="MKV Track & Font Extractor for Anime & Fansubs",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    add_shared_args(parser)
    parser.add_argument("--out-dir", type=str, default=None,
                        help="Directory where extracted assets (subtitles, covers, chapters, fonts) will be saved.")
    parser.add_argument("--fonts-dir", type=str, default=None,
                        help="Directory to save extracted fonts (defaults to <out-dir>/fonts or <input-dir>/fonts).")
    parser.add_argument("--dry-run", action="store_true",
                        help="List discovered MKV files without extracting anything.")
    cfg = parser.parse_args()

    set_events_enabled(cfg.events)
    handle_clear_cache_flag(cfg.clear_cache)

    root_input_dir = Path(cfg.input_dir).resolve() if cfg.input_dir else SCRIPT_DIR
    out_override = Path(cfg.out_dir).resolve() if cfg.out_dir else None
    fonts_override = Path(cfg.fonts_dir).resolve() if cfg.fonts_dir else None

    io_jobs = resolve_io_jobs(cfg.io_jobs, root_input_dir)

    runtime = build_cli_runtime(
        workers=cfg.workers,
        dry_run=cfg.dry_run,
        no_cache=cfg.no_cache,
        io_jobs=io_jobs,
    )
    install_cancel_handlers(runtime.cancel_token)

    logger.info("=========================================")
    logger.info(" MKV Track & Font Extractor")
    logger.info(" Anime / Indonesian Fansub Edition")
    logger.info("=========================================")
    logger.info("Workers: %d | IO jobs: %s | Input: %s",
                runtime.workers, runtime.io_jobs or "unlimited", root_input_dir)

    check_dependencies("mkvmerge", "mkvextract")
    logger.info("Using mkvmerge  : %s", shutil.which('mkvmerge'))
    logger.info("Using mkvextract: %s", shutil.which('mkvextract'))

    mkvs_by_dir = collect_release_dirs(
        root_input_dir,
        ignore_paths={out_override} if out_override else None,
    )

    emit_batch_start(
        total=sum(len(files) for files in mkvs_by_dir.values()),
        input_dir=str(root_input_dir),
    )

    font_hash_memo = FontHashMemo(get_default_font_hash_memo_file())

    config = ExtractRunConfig(
        runtime=runtime,
        root_input_dir=root_input_dir,
        out_override=out_override,
        fonts_override=fonts_override,
        font_hash_memo=font_hash_memo,
    )

    try:
        overall_success = _run_pipeline(mkvs_by_dir, config)
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

    font_hash_memo.save()

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
