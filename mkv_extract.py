#!/usr/bin/env python3
"""
MKV Track & Font Extractor
Optimized for anime & Indonesian fansub releases — including OVA, OAD, ONA, SP.

Extracts, per MKV: best-match ASS subtitle, chapters (if present), the
source's front-cover attachment (if any), and any embedded fonts (deduped
across files into a shared fonts/ folder).

Requires: mkvmerge, mkvextract (MKVToolNix)
"""

import sys
sys.dont_write_bytecode = True

import os
import re
import argparse
import hashlib
import shutil
import tempfile
import threading
import logging
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path

from mkv_common import (
    FONT_EXTENSIONS,
    FontRegistry,
    check_dependencies,
    ep_type_label,
    parse_attachments,
    has_chapters,
    inspect_videos,
    parse_episode,
    run_subprocess,
    safe_filename,
    select_best_subtitle_track,
    setup_logging,
    natural_sort_key,
    ParsedFile,
    scan_mkv_files,
    MAX_DEFAULT_WORKERS,
    DEFAULT_WORKER_MULTIPLIER,
    shift_ass_timestamps,
)

SCRIPT_DIR = Path(__file__).resolve().parent
SUPPORTED_SUB_CODECS = frozenset({"s_text/ass"})
IO_LOCK = threading.Lock()

# Initialize local child logger
logger = logging.getLogger("mkv_automation.extract")

# ─── DATA TYPES ──────────────────────────────────────────────────────────────
@dataclass
class MkvMetadata:
    tracks: list[dict] = field(default_factory=list)
    attachments: list[dict] = field(default_factory=list)
    has_chapters: bool = False


def parse_mkv_metadata(json_data: dict) -> MkvMetadata:
    tracks = []
    for t in json_data.get("tracks") or []:
        props = t.get("properties") or {}
        tracks.append({
            "id":         t.get("id", ""),
            "type":       t.get("type", ""),
            "codec_id":   (props.get("codec_id") or "").lower(),
            "language":   props.get("language") or "und",
            "track_name": props.get("track_name") or "",
        })
    attachments = []
    for a in json_data.get("attachments") or []:
        attachments.append({
            "id":           a.get("id", ""),
            "file_name":    a.get("file_name", ""),
            "content_type": a.get("content_type") or "",
            "size":         a.get("size", 0),
        })
    return MkvMetadata(tracks=tracks, attachments=attachments,
                       has_chapters=has_chapters(json_data))


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
def _run_mkvextract(mkv_path: Path, args: list[str], log: list[str]) -> None:
    """Run a single mkvextract command with mixed modes (tracks, chapters, attachments)."""
    if not args:
        log.append("   -> Nothing to extract.")
        return
        
    result = run_subprocess(["mkvextract", str(mkv_path), *args])
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
        
def _hash_file(filepath: Path) -> str:
    """Calculate BLAKE2b hash of a file for content-based font deduplication."""
    h = hashlib.blake2b()
    with filepath.open('rb') as f:
        while chunk := f.read(8192):
            h.update(chunk)
    return h.hexdigest()


# ─── PER-FILE WORKER ─────────────────────────────────────────────────────────
def _cleanup_stale_subtitle_offsets(base_sub_path: Path, current_final_path: Path) -> None:
    """Remove any stale offset-suffixed subtitle files for the same episode and language."""
    if base_sub_path.exists() and base_sub_path.resolve() != current_final_path.resolve():
        base_sub_path.unlink(missing_ok=True)
    parent = base_sub_path.parent
    stem = base_sub_path.stem
    suffix = base_sub_path.suffix
    pattern = re.compile(
        rf"^{re.escape(stem)}\.\[[-+]?\d+(?:\.\d+)?s\]{re.escape(suffix)}$",
        re.IGNORECASE,
    )
    try:
        for entry in os.scandir(str(parent)):
            if entry.is_file() and pattern.match(entry.name):
                p = Path(entry.path)
                if p.resolve() != current_final_path.resolve():
                    p.unlink(missing_ok=True)
    except OSError:
        pass


def process_file(
    mkv_file:      ParsedFile,
    json_data:     dict | None,
    fonts_dir:     Path,
    font_registry: FontRegistry,
    sub_offset:    float = 0.0,
) -> tuple[Path, list[str], bool]:
    mkv_path = mkv_file.path
    log: list[str] = [f"Processing: {mkv_path.name}"]

    if json_data is None:
        log.append(f"  WARNING: mkvmerge inspection failed for '{mkv_path.name}', skipping.")
        return mkv_path, log, False

    meta = parse_mkv_metadata(json_data)
    file_stem = mkv_path.stem
    file_dir  = mkv_path.parent

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

    extract_args: list[str] = []
    queued_outputs: list[tuple[Path, str]] = []

    sub_result = select_subtitle(json_data, file_stem, file_dir, ep_tag, log)
    final_sub_path = None
    if sub_result:
        track_id, sub_path = sub_result
        base_sub_path = sub_path
        if sub_offset != 0.0:
            sub_path = sub_path.with_name(f"{sub_path.stem}.[{sub_offset:+g}s]{sub_path.suffix}")
        final_sub_path = sub_path
        log.append(f"   -> Queued subtitle track {track_id} → {sub_path.name}")
        extract_args += ["tracks", f"{track_id}:{sub_path}"]
        queued_outputs.append((sub_path, "Subtitle"))
    else:
        log.append("   -> No suitable ASS subtitle tracks found.")

    chapter_path: Path | None = None
    if meta.has_chapters:
        chapter_path = file_dir / f"{chapter_stem}_chapters.xml"
        log.append(f"   -> Queued chapters → {chapter_path.name}")
        extract_args += ["chapters", str(chapter_path)]
        queued_outputs.append((chapter_path, "Chapter"))
    else:
        log.append("   -> No chapters found in source.")

    cover_path: Path | None = None
    cover, font_attachments = parse_attachments(meta.attachments)
    if cover:
        ext = Path(cover["file_name"]).suffix.lower() or ".jpg"
        cover_path = file_dir / f"{file_stem}_cover{ext}"
        if cover_path.exists():
            log.append(f"   -> Cover already exists ({cover_path.name}), skipping.")
            cover_path = None
        else:
            log.append(f"   -> Queued cover image → {cover_path.name}")
            extract_args += ["attachments", f"{cover['id']}:{cover_path}"]
            queued_outputs.append((cover_path, "Cover"))
    else:
        log.append("   -> No cover image found in source.")

    temp_dir = None
    claimed_this_call = []
    extracted_ok: set[tuple[str, int]] = set()
    try:
        temp_dir = Path(tempfile.mkdtemp(prefix=".tmp_extract_", dir=file_dir))
        staged_outputs: list[tuple[Path, Path, str]] = []
        for final_path, item_name in queued_outputs:
            staged_path = temp_dir / final_path.name
            final_text = str(final_path)
            for index, arg in enumerate(extract_args):
                if arg == final_text:
                    extract_args[index] = str(staged_path)
                elif arg.endswith(final_text):
                    extract_args[index] = f"{arg[:-len(final_text)]}{staged_path}"
            staged_outputs.append((staged_path, final_path, item_name))

        font_count = 0
        font_args: list[str] = []
        _attachments_open = "attachments" in extract_args

        new_font_attachments = []
        if font_attachments:
            for a in font_attachments:
                if a.get("id") is None or not a.get("file_name"):
                    continue
                safe_name = safe_filename(a["file_name"])
                size = a.get("size", 0)
                if font_registry.try_claim_name_size(safe_name, size):
                    new_font_attachments.append(a)
                    claimed_this_call.append((safe_name, size))
        if new_font_attachments:
            for a in new_font_attachments:
                safe_name = safe_filename(a["file_name"])
                temp_font_path = temp_dir / f"{a['id']}_{safe_name}"
                font_args.append(f"{a['id']}:{temp_font_path}")

        if font_args:
            log.append(f"   -> Queued {len(font_args)} font(s) for extraction...")
            if not _attachments_open:
                extract_args += ["attachments"]
            extract_args += font_args

        log.append("   -> Running mkvextract...")
        _run_mkvextract(mkv_path, extract_args, log)

        staged_output_paths = {staged for staged, _, _ in staged_outputs}
        for staged_path, final_path, item_name in staged_outputs:
            _clean_empty_file(staged_path, log, item_name)
            if not staged_path.exists():
                continue
            if final_path == final_sub_path and sub_offset != 0.0:
                shift_ass_timestamps(staged_path, sub_offset, log)
            staged_path.replace(final_path)

        if final_sub_path and final_sub_path.exists():
            _cleanup_stale_subtitle_offsets(base_sub_path, final_sub_path)

        if font_args and temp_dir:
            id_to_meta = {str(a["id"]): a for a in new_font_attachments}
            for temp_font_path in temp_dir.iterdir():
                if not temp_font_path.is_file() or temp_font_path.stat().st_size == 0:
                    continue
                if temp_font_path in staged_output_paths:
                    continue
                try:
                    file_hash = _hash_file(temp_font_path)
                except OSError:
                    continue

                parts = temp_font_path.name.split("_", 1)
                if len(parts) < 2:
                    continue
                fid, safe_name = parts
                att_meta = id_to_meta.get(fid)
                if not att_meta:
                    continue
                size = att_meta.get("size", 0)
                extracted_ok.add((safe_name, size))

                if font_registry.try_claim_hash(file_hash):
                    with IO_LOCK:
                        final_path = fonts_dir / safe_name
                        if final_path.exists():
                            stem = final_path.stem
                            suffix = final_path.suffix
                            final_path = fonts_dir / f"{stem}_{file_hash[:8]}{suffix}"
                        
                        shutil.move(str(temp_font_path), str(final_path))
                    
                    font_count += 1
                    log.append(f"   -> Saved font: {final_path.name}")
                else:
                    temp_font_path.unlink(missing_ok=True)
            log.append(f"   -> Extracted {font_count} new font(s).")

        # Release any fonts that were claimed but not successfully extracted/verified
        for name, size in claimed_this_call:
            if (name, size) not in extracted_ok:
                font_registry.release_name_size(name, size)

    except Exception:
        for name, size in claimed_this_call:
            if (name, size) not in extracted_ok:
                font_registry.release_name_size(name, size)
        raise
    finally:
        if temp_dir is not None:
            shutil.rmtree(temp_dir, ignore_errors=True)

    log.append(f"Done: {mkv_path.name}")
    return mkv_path, log, True



# ─── RUN ORCHESTRATOR ────────────────────────────────────────────────────────
def _run(input_dir: Path, fonts_dir: Path, mkv_files: list[ParsedFile], pool: ThreadPoolExecutor, max_workers: int, dry_run: bool, sub_offset: float = 0.0) -> bool:
    """Returns True if every file in this directory was processed without
    a crash or mkvmerge-inspection failure; False otherwise. Propagated up
    to main() so the process exits non-zero if anything actually failed."""
    if not mkv_files:
        logger.warning("No MKV files found in '%s', skipping.", input_dir.name)
        return False
    logger.info("MKV files discovered: %d", len(mkv_files))

    if dry_run:
        logger.info("--- DRY RUN: Files that would be processed ---")
        for f in mkv_files:
            logger.info("  %s", f.name)
        logger.info("Total: %d file(s). No extraction performed.", len(mkv_files))
        return True

    fonts_dir.mkdir(parents=True, exist_ok=True)
    json_cache = inspect_videos([f.path for f in mkv_files], pool)

    existing_hashes = set()
    if fonts_dir.is_dir():
        with os.scandir(str(fonts_dir)) as it:
            for entry in it:
                if entry.is_file() and Path(entry.name).suffix.lower() in FONT_EXTENSIONS:
                    try:
                        existing_hashes.add(_hash_file(Path(entry.path)))
                    except OSError:
                        pass
    font_registry = FontRegistry(existing=existing_hashes)

    logger.info("Processing %d file(s) (up to %d at a time)...", len(mkv_files), max_workers)

    results: dict[Path, list[str]] = {}
    total_files = len(mkv_files)
    completed_files = 0
    any_failures = False

    futures = {
        pool.submit(
            process_file,
            mkv_file,
            json_cache.get(mkv_file.path),
            fonts_dir,
            font_registry,
            sub_offset,
        ): mkv_file.path
        for mkv_file in mkv_files
    }

    for future in as_completed(futures):
        mkv_path = futures[future]
        completed_files += 1
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

        except Exception:
            any_failures = True
            # 1. Logs a single ERROR with the progress text and attaches the traceback
            logger.error(
                "[%d/%d (%.0f%%)] CRASHED: %s",
                completed_files, total_files, progress * 100, mkv_path.name,
                exc_info=True
            )

            # 2. Use WARNING for the deferred log so the final print loop
            # doesn't trigger a second ERROR-level log for the same crash.
            results[mkv_path] = [
                f"Processing: {mkv_path.name}",
                "  WARNING: Worker crashed. See traceback above.",
            ]

    for mkv_file in mkv_files:
        mkv_path = mkv_file.path
        logger.info("-" * 60)
        for line in results.get(mkv_path, []):
            # Prefix check (not substring) — a filename containing "ERROR" or
            # "WARNING" anywhere in it must not get misclassified as an error.
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


def main() -> None:
    cpu_count = os.cpu_count() or 4
    default_workers = min(cpu_count * DEFAULT_WORKER_MULTIPLIER, MAX_DEFAULT_WORKERS)

    parser = argparse.ArgumentParser(
        description="MKV Track & Font Extractor for Anime & Fansubs",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--workers", type=int, default=default_workers, choices=range(1, 33),
                        metavar="[1-32]",
                        help="Number of concurrent files to process.")
    parser.add_argument("--input-dir", type=str, default=None,
                        help="Directory containing MKV files (defaults to script directory).")
    parser.add_argument("--dry-run", action="store_true",
                        help="List discovered MKV files without extracting anything.")
    parser.add_argument("--sub-offset", type=float, default=0.0,
                        help="Shift extracted subtitle timestamps by this many seconds (negative = earlier, positive = later). Applied once, at extraction time.")
    cfg = parser.parse_args()

    max_workers = cfg.workers
    root_input_dir = Path(cfg.input_dir).resolve() if cfg.input_dir else SCRIPT_DIR
    dry_run     = cfg.dry_run

    setup_logging(logging.INFO)

    logger.info("=========================================")
    logger.info(" MKV Track & Font Extractor")
    logger.info(" Anime / Indonesian Fansub Edition")
    logger.info("=========================================")
    logger.info("Workers: %d | Input: %s", max_workers, root_input_dir)

    check_dependencies("mkvmerge", "mkvextract")
    logger.info("Using mkvmerge  : %s", shutil.which('mkvmerge'))
    logger.info("Using mkvextract: %s", shutil.which('mkvextract'))

    # Discover all MKV files recursively to find which subdirectories contain them
    all_mkvs = scan_mkv_files(root_input_dir)
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

            current_fonts_dir = mkv_dir / "fonts"
            dir_mkvs = mkvs_by_dir[mkv_dir]
            success = _run(mkv_dir, current_fonts_dir, dir_mkvs, pool, max_workers, dry_run, sub_offset=cfg.sub_offset)
            if not success:
                overall_success = False

    if not overall_success:
        sys.exit(1)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        logger.warning("Cancelled by user.")
        sys.exit(130)
