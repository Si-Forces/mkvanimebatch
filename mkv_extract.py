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
import shutil
import subprocess
import sys
import tempfile
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from mkv_domain import (
    COVER_EXTENSIONS,
    EpisodeInfo,
    ep_type_label,
    resolve_output_dir,
    select_best_subtitle_track,
)
from mkv_events import emit_event, get_item_id, set_events_enabled, setup_logging
from mkv_fonts import FontClaim, FontHashMemo, FontManager
from mkv_proc import (
    SUBPROCESS_TIMEOUT,
    CancelledError,
    CancelToken,
    acquire_semaphore,
    run_subprocess_cancellable,
)
from mkv_scan import (
    ParsedFile,
    PersistentInspectCache,
    check_dependencies,
    get_default_font_hash_memo_file,
    has_chapters,
    inspect_videos,
    parse_attachments,
)
from mkv_runner import (
    SCRIPT_DIR,
    add_shared_args,
    build_cli_runtime,
    collect_release_dirs,
    handle_clear_cache_flag,
    install_cancel_handlers,
    run_batch,
)

SUPPORTED_SUB_CODECS = frozenset({"s_text/ass"})

# Initialize local child logger
logger = logging.getLogger("mkv_automation.extract")


# ─── SUBTITLE SELECTION ──────────────────────────────────────────────────────
def _episode_suffix(parsed: EpisodeInfo | None, file_stem: str) -> str:
    """Filename segment identifying the episode; falls back to the raw stem
    when the file name carried no parseable Episode Info."""
    if parsed is None:
        return file_stem
    return f"{ep_type_label(parsed.ep_type)}{parsed.ep}"


def select_subtitle(
    json_data: dict,
    file_stem: str,
    file_dir:  Path,
    parsed:    EpisodeInfo | None,
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
        with acquire_semaphore(io_semaphore):
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
    json_data:           dict | None,
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
        emit_event("item_start", item_id=item_id, file=mkv_path.name)
        log.append("  !! CANCELLED before start.")
        emit_event("item_complete", item_id=item_id, file=mkv_path.name, status="cancelled")
        return mkv_path, log, False

    emit_event("item_start", item_id=item_id, file=mkv_path.name)

    if json_data is None:
        log.append(f"  ERROR: mkvmerge inspection failed for '{mkv_path.name}'.")
        emit_event("item_complete", item_id=item_id, file=mkv_path.name, status="error", error="mkvmerge inspection failed")
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

    queued_outputs: list[tuple[str, str | None, Path, str]] = []

    sub_result = select_subtitle(json_data, file_stem, file_dir, parsed, log)
    final_sub_path = None
    if sub_result:
        track_id, sub_path = sub_result
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
        _run_mkvextract(
            mkv_path,
            extract_args,
            log,
            io_semaphore=io_semaphore,
            cancel_token=cancel_token,
        )

        for staged_path, final_path, item_name in staged_outputs:
            _clean_empty_file(staged_path, log, item_name)
            if not staged_path.exists():
                continue
            staged_path.replace(final_path)

        saved_count = 0
        for claim in font_claims:
            promoted_path = font_manager.promote(claim)
            if promoted_path:
                saved_count += 1
                log.append(f"   -> Saved font: {promoted_path.name}")
        if font_claims:
            log.append(f"   -> Extracted {saved_count} new font(s).")

    except BaseException:
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
    persistent_cache:   PersistentInspectCache | None = None,
    io_semaphore:       threading.Semaphore | None = None,
    font_manager:       FontManager | None = None,
    root_input_dir:     Path | None = None,
    font_hash_memo:     FontHashMemo | None = None,
    cancel_token:       CancelToken | None = None,
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
    json_cache = inspect_videos([f.path for f in mkv_files], pool, persistent_cache=persistent_cache, cancel_token=cancel_token)

    # A manager passed in from main() is shared across directory passes; its
    # staging directory must outlive this pass and is cleaned up by the caller.
    owns_font_manager = font_manager is None
    active_font_manager = FontManager(fonts_dir, hash_memo=font_hash_memo) if font_manager is None else font_manager

    logger.info("Processing %d file(s) (up to %d at a time)...", len(mkv_files), max_workers)

    results: dict[Path, list[str]] = {}
    total_files = len(mkv_files)
    any_failures = False

    futures = {
        pool.submit(
            process_file,
            mkv_file,
            json_cache.get(mkv_file.path),
            active_font_manager,
            io_semaphore=io_semaphore,
            output_dir=output_dir,
            root_input_dir=root_dir,
            cancel_token=cancel_token,
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

    if owns_font_manager:
        active_font_manager.cleanup_staging()

    logger.info("=========================================")
    logger.info(" All files processed.")
    logger.info("=========================================")
    return not any_failures


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
    parser.add_argument("--io-jobs", type=int, default=2,
                        help="Maximum concurrent disk I/O jobs for mkvextract. Set to 0 to disable throttling.")
    cfg = parser.parse_args()

    set_events_enabled(cfg.events)
    handle_clear_cache_flag(cfg.clear_cache)

    root_input_dir = Path(cfg.input_dir).resolve() if cfg.input_dir else SCRIPT_DIR
    out_override = Path(cfg.out_dir).resolve() if cfg.out_dir else None
    fonts_override = Path(cfg.fonts_dir).resolve() if cfg.fonts_dir else None

    runtime = build_cli_runtime(
        root_input_dir=root_input_dir,
        out_override=out_override,
        workers=cfg.workers,
        sub_offset=0.0,
        dry_run=cfg.dry_run,
        no_cache=cfg.no_cache,
        io_jobs=cfg.io_jobs,
    )
    install_cancel_handlers(runtime.cancel_token)

    logger.info("=========================================")
    logger.info(" MKV Track & Font Extractor")
    logger.info(" Anime / Indonesian Fansub Edition")
    logger.info("=========================================")
    logger.info("Workers: %d | Input: %s", runtime.workers, root_input_dir)

    check_dependencies("mkvmerge", "mkvextract")
    logger.info("Using mkvmerge  : %s", shutil.which('mkvmerge'))
    logger.info("Using mkvextract: %s", shutil.which('mkvextract'))

    mkvs_by_dir = collect_release_dirs(
        root_input_dir,
        ignore_paths={out_override} if out_override else None,
    )

    emit_event(
        "batch_start",
        total=sum(len(files) for files in mkvs_by_dir.values()),
        input_dir=str(root_input_dir),
    )

    def fonts_dir_for(mkv_dir: Path) -> Path:
        if fonts_override:
            return fonts_override
        if out_override:
            return out_override / "fonts"
        return mkv_dir / "fonts"

    font_hash_memo = FontHashMemo(get_default_font_hash_memo_file())

    shared_font_manager: FontManager | None = None
    if fonts_override or out_override:
        shared_font_manager = FontManager(fonts_dir_for(root_input_dir), hash_memo=font_hash_memo)

    def process_directory(pool: ThreadPoolExecutor, mkv_dir: Path, dir_mkvs: list[ParsedFile]) -> bool:
        current_output_dir = resolve_output_dir(mkv_dir, root_input_dir, out_override, default_subdir=None)
        if not runtime.dry_run:
            current_output_dir.mkdir(parents=True, exist_ok=True)
        return _run(
            current_output_dir,
            fonts_dir_for(mkv_dir),
            dir_mkvs,
            pool,
            runtime.workers,
            runtime.dry_run,
            persistent_cache=runtime.persistent_cache,
            io_semaphore=runtime.io_semaphore,
            font_manager=shared_font_manager,
            root_input_dir=root_input_dir,
            font_hash_memo=font_hash_memo,
            cancel_token=runtime.cancel_token,
        )

    try:
        overall_success = run_batch(runtime, mkvs_by_dir, process_directory)
    except CancelledError:
        logger.warning("Cancelled by user.")
        overall_success = False

    cancelled = runtime.cancel_token.cancelled

    font_hash_memo.save()
    # The shared manager's staging directory spans the whole batch; remove it
    # once here instead of per-directory pass.
    if shared_font_manager is not None:
        shared_font_manager.cleanup_staging()

    emit_event("batch_complete", success=overall_success and not cancelled, cancelled=cancelled)

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
