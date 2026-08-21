#!/usr/bin/env python3
"""
MKV Track & Font Extractor
Optimized for anime & Indonesian fansub releases — including OVA, OAD, ONA, SP.

Extracts, per MKV: best-match ASS subtitle, chapters (if present), the
source's front-cover attachment (if any), and any embedded fonts (deduped
across files into a shared fonts/ folder).

Requires: mkvmerge, mkvextract (MKVToolNix)

NOTE: Concurrent invocations on the same output tree are unsupported.
"""
from __future__ import annotations

import argparse
import logging
import os
import shutil
import subprocess
import sys
import tempfile
import threading
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from mkv_common import (
    COVER_EXTENSIONS,
    DEFAULT_WORKER_MULTIPLIER,
    MAX_DEFAULT_WORKERS,
    SUBPROCESS_TIMEOUT,
    FontClaim,
    FontManager,
    ParsedFile,
    PersistentInspectCache,
    StaleSubtitleTracker,
    acquire_semaphore,
    check_dependencies,
    cleanup_stale_subtitle_offsets_bulk,
    emit_event,
    ep_type_label,
    get_item_id,
    has_chapters,
    inspect_videos,
    natural_sort_key,
    parse_attachments,
    resolve_output_dir,
    run_subprocess,
    scan_mkv_files,
    select_best_subtitle_track,
    set_events_enabled,
    setup_logging,
    shift_ass_timestamps,
)

SCRIPT_DIR = Path(__file__).resolve().parent
SUPPORTED_SUB_CODECS = frozenset({"s_text/ass"})

# Initialize local child logger
logger = logging.getLogger("mkv_automation.extract")


# ─── SUBTITLE SELECTION ──────────────────────────────────────────────────────
def select_subtitle(
    json_data: dict,
    file_stem: str,
    file_dir:  Path,
    ep_tag:    str,
    log:       list[str],
) -> tuple[str, Path] | None:
    """Pick the best ASS subtitle track and build its output path."""
    result = select_best_subtitle_track(
        json_data.get("tracks", []),
        codec_filter=SUPPORTED_SUB_CODECS,
        log=log,
    )
    if result is None:
        return None
    track_id, language, _ = result
    sub_path = file_dir / f"{file_stem}_{ep_tag}_{language}_track{track_id}.ass"
    return str(track_id), sub_path


# ─── EXTRACTION HELPERS ──────────────────────────────────────────────────────
def _run_mkvextract(mkv_path: Path, args: list[str], log: list[str], io_semaphore: threading.Semaphore | None = None) -> None:
    """Run a single mkvextract command with mixed modes (tracks, chapters, attachments)."""
    if not args:
        log.append("   -> Nothing to extract.")
        return
        
    try:
        with acquire_semaphore(io_semaphore):
            result = run_subprocess(["mkvextract", str(mkv_path), *args], capture_stdout=False, timeout=SUBPROCESS_TIMEOUT)
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
    json_data:           dict | None,
    font_manager:        FontManager,
    stale_subs_tracker:  StaleSubtitleTracker | None = None,
    sub_offset:          float = 0.0,
    io_semaphore:        threading.Semaphore | None = None,
    output_dir:          Path | None = None,
    root_input_dir:      Path | None = None,
) -> tuple[Path, list[str], bool]:
    mkv_path = mkv_file.path
    root_dir = root_input_dir or mkv_path.parent
    item_id = get_item_id(mkv_path, root_dir)
    log: list[str] = [f"Processing: {mkv_path.name}"]

    emit_event("item_start", item_id=item_id, file=mkv_path.name)

    if json_data is None:
        log.append(f"  WARNING: mkvmerge inspection failed for '{mkv_path.name}', skipping.")
        emit_event("item_complete", item_id=item_id, file=mkv_path.name, status="skipped", reason="mkvmerge failed")
        return mkv_path, log, False

    file_stem = mkv_path.stem
    file_dir  = output_dir if output_dir is not None else mkv_path.parent

    parsed = mkv_file.parsed
    if parsed:
        _, ep_type, ep = parsed
        ep_tag       = f"{ep_type_label(ep_type)}{ep}"
        chapter_stem = f"{file_stem}_{ep_tag}"
        log.append(f"   -> Type: {ep_type_label(ep_type)} {ep}")
    else:
        ep_tag       = file_stem
        chapter_stem = file_stem
        log.append(
            "   -> WARNING: Could not parse episode number; "
            f"subtitle tag will use stem '{file_stem}'."
        )

    queued_outputs: list[tuple[str, str | None, Path, str]] = []

    sub_result = select_subtitle(json_data, file_stem, file_dir, ep_tag, log)
    base_sub_path: Path | None = None
    final_sub_path = None
    if sub_result:
        track_id, sub_path = sub_result
        base_sub_path = sub_path
        if sub_offset != 0.0:
            sub_path = sub_path.with_name(f"{sub_path.stem}.[{sub_offset:+g}s]{sub_path.suffix}")
        final_sub_path = sub_path
        log.append(f"   -> Queued subtitle track {track_id} → {sub_path.name}")
        queued_outputs.append(("tracks", str(track_id), sub_path, "Subtitle"))
    else:
        log.append("   -> No suitable ASS subtitle tracks found.")

    chapter_path: Path | None = None
    if has_chapters(json_data):
        chapter_path = file_dir / f"{chapter_stem}_chapters.xml"
        log.append(f"   -> Queued chapters → {chapter_path.name}")
        queued_outputs.append(("chapters", None, chapter_path, "Chapter"))
    else:
        log.append("   -> No chapters found in source.")

    cover_path: Path | None = None
    cover, font_attachments = parse_attachments(json_data.get("attachments") or [])
    if cover:
        raw_ext = Path(cover.get("file_name", "")).suffix.lower()
        ext = raw_ext if raw_ext in COVER_EXTENSIONS else ".jpg"
        cover_path = file_dir / f"{file_stem}_cover{ext}"
        if cover_path.exists():
            log.append(f"   -> Cover already exists ({cover_path.name}), skipping.")
            cover_path = None
        else:
            log.append(f"   -> Queued cover image → {cover_path.name}")
            queued_outputs.append(("attachments", str(cover['id']), cover_path, "Cover"))
    else:
        log.append("   -> No cover image found in source.")

    temp_dir = None
    font_claims: list[FontClaim] = []
    try:
        temp_dir = Path(tempfile.mkdtemp(prefix=".tmp_extract_", dir=file_dir))
        extract_args: list[str] = []
        staged_outputs: list[tuple[Path, Path, str]] = []
        for cmd_type, arg_id, final_path, item_name in queued_outputs:
            staged_path = temp_dir / final_path.name
            if arg_id is not None:
                extract_args += [cmd_type, f"{arg_id}:{staged_path}"]
            else:
                extract_args += [cmd_type, str(staged_path)]
            staged_outputs.append((staged_path, final_path, item_name))

        font_args: list[str] = []
        attachments_open = cover_path is not None

        if font_attachments:
            for a in font_attachments:
                if a.get("id") is None or not a.get("file_name"):
                    continue
                claim = font_manager.claim(a["file_name"], a.get("size", 0))
                if claim is not None:
                    font_claims.append(claim)
                    font_args.append(f"{a['id']}:{claim.staging_path}")

        if font_args:
            log.append(f"   -> Queued {len(font_args)} font(s) for extraction...")
            if not attachments_open:
                extract_args += ["attachments"]
            extract_args += font_args

        log.append("   -> Running mkvextract...")
        _run_mkvextract(mkv_path, extract_args, log, io_semaphore=io_semaphore)

        for staged_path, final_path, item_name in staged_outputs:
            _clean_empty_file(staged_path, log, item_name)
            if not staged_path.exists():
                continue
            if final_path == final_sub_path and sub_offset != 0.0:
                shift_ass_timestamps(staged_path, sub_offset, log)
            staged_path.replace(final_path)

        if final_sub_path and final_sub_path.exists():
            if stale_subs_tracker is not None and base_sub_path is not None:
                stale_subs_tracker.record(base_sub_path.stem, final_sub_path.name)
            elif base_sub_path is not None:
                cleanup_stale_subtitle_offsets_bulk(
                    base_sub_path.parent,
                    {base_sub_path.stem: final_sub_path.name},
                )

        saved_count = 0
        for claim in font_claims:
            promoted_path = font_manager.promote(claim)
            if promoted_path:
                saved_count += 1
                log.append(f"   -> Saved font: {promoted_path.name}")
        if font_claims:
            log.append(f"   -> Extracted {saved_count} new font(s).")

    except Exception:
        for claim in font_claims:
            font_manager.release(claim)
        raise
    finally:
        if temp_dir is not None:
            shutil.rmtree(temp_dir, ignore_errors=True)

    log.append(f"Done: {mkv_path.name}")
    emit_event("item_complete", item_id=item_id, file=mkv_path.name, status="completed")
    return mkv_path, log, True


# ─── RUN ORCHESTRATOR ────────────────────────────────────────────────────────
def _run(
    output_dir:         Path,
    fonts_dir:          Path,
    mkv_files:          list[ParsedFile],
    pool:               ThreadPoolExecutor,
    max_workers:        int,
    dry_run:            bool,
    sub_offset:         float = 0.0,
    persistent_cache:   PersistentInspectCache | None = None,
    io_semaphore:       threading.Semaphore | None = None,
    font_manager:       FontManager | None = None,
    root_input_dir:     Path | None = None,
) -> bool:
    if not mkv_files:
        logger.warning("No MKV files found in '%s', skipping.", output_dir.name)
        return False
    logger.info("MKV files discovered: %d", len(mkv_files))

    root_dir = root_input_dir or output_dir

    if dry_run:
        logger.info("--- DRY RUN: Files that would be processed ---")
        for f in mkv_files:
            item_id = get_item_id(f.path, root_dir)
            emit_event("item_complete", item_id=item_id, file=f.name, status="planned")
            logger.info("  %s", f.name)
        logger.info("Total: %d file(s). No extraction performed.", len(mkv_files))
        return True

    fonts_dir.mkdir(parents=True, exist_ok=True)
    json_cache = inspect_videos([f.path for f in mkv_files], pool, persistent_cache=persistent_cache)

    if font_manager is None:
        font_manager = FontManager(fonts_dir)
    stale_subs_tracker = StaleSubtitleTracker()

    logger.info("Processing %d file(s) (up to %d at a time)...", len(mkv_files), max_workers)

    results: dict[Path, list[str]] = {}
    total_files = len(mkv_files)
    any_failures = False

    futures = {
        pool.submit(
            process_file,
            mkv_file,
            json_cache.get(mkv_file.path),
            font_manager,
            stale_subs_tracker=stale_subs_tracker,
            sub_offset=sub_offset,
            io_semaphore=io_semaphore,
            output_dir=output_dir,
            root_input_dir=root_dir,
        ): mkv_file
        for mkv_file in mkv_files
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

        except Exception as exc:
            any_failures = True
            logger.exception(
                "[%d/%d (%.0f%%)] CRASHED: %s",
                completed_files, total_files, progress * 100, mkv_path.name,
            )
            item_id = get_item_id(mkv_path, root_dir)
            emit_event("item_complete", item_id=item_id, file=mkv_path.name, status="error", error=str(exc))
            results[mkv_path] = [
                f"Processing: {mkv_path.name}",
                "  WARNING: Worker crashed. See traceback above.",
            ]

    for mkv_file in mkv_files:
        mkv_path = mkv_file.path
        logger.info("-" * 60)
        for line in results.get(mkv_path, []):
            stripped = line.lstrip()
            if stripped.startswith("WARNING"):
                logger.warning(line)
            elif stripped.startswith("ERROR"):
                logger.error(line)
            else:
                logger.info(line)

    stale_subs_tracker.cleanup(output_dir)
    font_manager.cleanup_staging()

    logger.info("=========================================")
    logger.info(" All files processed.")
    logger.info("=========================================")
    return not any_failures


def main() -> None:
    cpu_count = os.cpu_count() or 4
    default_workers = min(cpu_count * DEFAULT_WORKER_MULTIPLIER, MAX_DEFAULT_WORKERS)

    parser = argparse.ArgumentParser(
        description="MKV Track & Font Extractor for Anime & Fansubs",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--workers", type=int, default=default_workers, choices=range(1, 65),
                        metavar="[1-64]",
                        help="Number of concurrent files to process.")
    parser.add_argument("--out-dir", type=str, default=None,
                        help="Directory where extracted assets (subtitles, covers, chapters, fonts) will be saved.")
    parser.add_argument("--input-dir", type=str, default=None,
                        help="Directory containing MKV files (defaults to script directory).")
    parser.add_argument("--fonts-dir", type=str, default=None,
                        help="Directory to save extracted fonts (defaults to <out-dir>/fonts or <input-dir>/fonts).")
    parser.add_argument("--dry-run", action="store_true",
                        help="List discovered MKV files without extracting anything.")
    parser.add_argument("--sub-offset", type=float, default=0.0,
                        help="Shift extracted subtitle timestamps by this many seconds (negative = earlier, positive = later). Applied once, at extraction time.")
    parser.add_argument("--io-jobs", type=int, default=2,
                        help="Maximum concurrent disk I/O jobs for mkvextract. Set to 0 to disable throttling.")
    parser.add_argument("--no-cache", action="store_true",
                        help="Bypass persistent inspection cache.")
    parser.add_argument("--clear-cache", action="store_true",
                        help="Clear persistent inspection cache and exit.")
    parser.add_argument("--events", action="store_true",
                        help="Emit structured @event: JSON-line events on stdout.")
    cfg = parser.parse_args()

    set_events_enabled(cfg.events)

    max_workers = cfg.workers
    root_input_dir = Path(cfg.input_dir).resolve() if cfg.input_dir else SCRIPT_DIR
    out_override = Path(cfg.out_dir).resolve() if cfg.out_dir else None
    fonts_override = Path(cfg.fonts_dir).resolve() if cfg.fonts_dir else None
    dry_run = cfg.dry_run

    if cfg.clear_cache:
        PersistentInspectCache().clear()
        logger.info("Persistent inspection cache cleared.")
        sys.exit(0)

    persistent_cache = PersistentInspectCache(enabled=not cfg.no_cache)
    io_semaphore = threading.BoundedSemaphore(cfg.io_jobs) if cfg.io_jobs > 0 else None

    setup_logging(logging.INFO)

    logger.info("=========================================")
    logger.info(" MKV Track & Font Extractor")
    logger.info(" Anime / Indonesian Fansub Edition")
    logger.info("=========================================")
    logger.info("Workers: %d | Input: %s", max_workers, root_input_dir)

    check_dependencies("mkvmerge", "mkvextract")
    logger.info("Using mkvmerge  : %s", shutil.which('mkvmerge'))
    logger.info("Using mkvextract: %s", shutil.which('mkvextract'))

    ignore_paths = {out_override} if out_override else None
    all_mkvs = scan_mkv_files(root_input_dir, ignore_paths=ignore_paths)
    if not all_mkvs:
        logger.error("No MKV files found.")
        sys.exit(1)

    mkvs_by_dir: dict[Path, list[ParsedFile]] = defaultdict(list)
    for mkv in all_mkvs:
        mkvs_by_dir[mkv.path.parent].append(mkv)

    mkv_dirs = sorted(mkvs_by_dir.keys(), key=natural_sort_key)

    emit_event("batch_start", total=len(all_mkvs), input_dir=str(root_input_dir))

    def fonts_dir_for(mkv_dir: Path) -> Path:
        if fonts_override:
            return fonts_override
        if out_override:
            return out_override / "fonts"
        return mkv_dir / "fonts"

    shared_font_manager: FontManager | None = None
    if fonts_override or out_override:
        shared_font_manager = FontManager(fonts_dir_for(root_input_dir))

    overall_success = True
    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        for mkv_dir in mkv_dirs:
            logger.info("")
            logger.info("=" * 60)
            logger.info("Processing directory: %s", mkv_dir)
            logger.info("=" * 60)

            current_output_dir = resolve_output_dir(
                mkv_dir,
                root_input_dir,
                out_override,
                default_subdir=None,
            )

            if not dry_run:
                current_output_dir.mkdir(parents=True, exist_ok=True)

            current_fonts_dir = fonts_dir_for(mkv_dir)

            dir_mkvs = mkvs_by_dir[mkv_dir]
            success = _run(
                current_output_dir,
                current_fonts_dir,
                dir_mkvs,
                pool,
                max_workers,
                dry_run,
                sub_offset=cfg.sub_offset,
                persistent_cache=persistent_cache,
                io_semaphore=io_semaphore,
                font_manager=shared_font_manager,
                root_input_dir=root_input_dir,
            )
            if not success:
                overall_success = False

    emit_event("batch_complete", success=overall_success)

    if not overall_success:
        sys.exit(1)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        logger.warning("Cancelled by user.")
        sys.exit(130)
