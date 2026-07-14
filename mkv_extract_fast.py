#!/usr/bin/env python3
"""
MKV Track & Font Extractor (High Performance & Simplified)
Optimized for speed and minimal output.
"""

import sys
sys.dont_write_bytecode = True

import argparse
import os
import re
import shutil
import tempfile
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path

from mkv_common_fast import (
    FONT_EXTENSIONS,
    FontRegistry,
    check_dependencies,
    ep_type_label,
    parse_attachments,
    has_chapters,
    run_subprocess,
    inspect_videos,
    safe_filename,
    select_best_subtitle_track,
    shift_ass_timestamps,
    natural_sort_key,
    scan_mkv_files,
    EpisodeInfo,
    MAX_DEFAULT_WORKERS,
    DEFAULT_WORKER_MULTIPLIER,
)

SCRIPT_DIR = Path(__file__).resolve().parent
SUPPORTED_SUB_CODECS = frozenset({"s_text/ass"})
IO_LOCK = threading.Lock()


@dataclass
class MkvMetadata:
    tracks: list[dict] = field(default_factory=list)
    attachments: list[dict] = field(default_factory=list)
    has_chapters: bool = False


def parse_mkv_metadata(json_data: dict) -> MkvMetadata:
    return MkvMetadata(
        tracks=json_data.get("tracks") or [],
        attachments=json_data.get("attachments") or [],
        has_chapters=has_chapters(json_data)
    )


def select_subtitle(json_data: dict, file_stem: str, file_dir: Path, ep_tag: str) -> tuple[str, Path] | None:
    result = select_best_subtitle_track(json_data.get("tracks", []), codec_filter=SUPPORTED_SUB_CODECS)
    if result is None:
        return None
    track_id, language, _ = result
    return str(track_id), file_dir / f"{file_stem}_{ep_tag}_{language}_track{track_id}.ass"


def _cleanup_stale_subtitle_offsets(base_sub_path: Path, current_final_path: Path) -> None:
    """Remove any stale offset-suffixed subtitle files for the same episode and language."""
    parent = base_sub_path.parent
    stem = base_sub_path.stem
    suffix = base_sub_path.suffix
    pattern = re.compile(
        rf"^{re.escape(stem)}(?:\.\[[-+]?\d+(?:\.\d+)?s\])?{re.escape(suffix)}$",
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


def pipeline_worker(
    mkv_path: Path,
    mkv_parsed: EpisodeInfo | None,
    json_data: dict | None,
    fonts_dir: Path,
    font_registry: FontRegistry,
    sub_offset: float = 0.0,
) -> bool:
    # 1. Inspect (pre-fetched)
    if json_data is None:
        print(f"  [-] Failed: {mkv_path.name} (mkvmerge inspection failed)")
        return False

    meta = parse_mkv_metadata(json_data)
    file_stem = mkv_path.stem
    file_dir  = mkv_path.parent

    if mkv_parsed:
        ep_tag = f"{ep_type_label(mkv_parsed[1])}{mkv_parsed[2]}"
        chapter_stem = f"{file_stem}_{ep_tag}"
    else:
        ep_tag = file_stem
        chapter_stem = file_stem

    temp_dir = None
    claimed_this_call: list[tuple[str, int]] = []
    extracted_ok: set[tuple[str, int]] = set()

    try:
        temp_dir = Path(tempfile.mkdtemp(prefix="extract_job_", dir=file_dir))

        extract_args: list[str] = []
        final_sub_path = None
        newly_extracted_sub_path = None

        sub_result = select_subtitle(json_data, file_stem, file_dir, ep_tag)
        if sub_result:
            track_id, sub_path = sub_result
            base_sub_path = sub_path
            if sub_offset != 0.0:
                sub_path = sub_path.with_name(f"{sub_path.stem}.[{sub_offset:+g}s]{sub_path.suffix}")
            final_sub_path = sub_path
            if not sub_path.exists() or sub_path.stat().st_size == 0:
                temp_sub_path = temp_dir / sub_path.name
                extract_args += ["tracks", f"{track_id}:{temp_sub_path}"]
                newly_extracted_sub_path = temp_sub_path

        final_chapter_path = None
        if meta.has_chapters:
            chapter_path = file_dir / f"{chapter_stem}_chapters.xml"
            final_chapter_path = chapter_path
            if not chapter_path.exists() or chapter_path.stat().st_size == 0:
                temp_chapter_path = temp_dir / chapter_path.name
                extract_args += ["chapters", str(temp_chapter_path)]

        final_cover_path = None
        cover, font_attachments = parse_attachments(meta.attachments)
        if cover and cover.get("id") is not None and cover.get("file_name"):
            ext = Path(cover["file_name"]).suffix.lower() or ".jpg"
            cover_path = file_dir / f"{file_stem}_cover{ext}"
            final_cover_path = cover_path
            if not cover_path.exists() or cover_path.stat().st_size == 0:
                temp_cover_path = temp_dir / cover_path.name
                extract_args += ["attachments", f"{cover['id']}:{temp_cover_path}"]

        new_font_attachments = []
        for a in font_attachments:
            if a.get("id") is None or not a.get("file_name"):
                continue
            key_name = safe_filename(a["file_name"])
            key_size = a.get("size", 0)

            if font_registry.try_claim(key_name, key_size):
                new_font_attachments.append(a)
                claimed_this_call.append((key_name, key_size))

        if new_font_attachments:
            font_args = []
            for a in new_font_attachments:
                temp_font_path = temp_dir / f"{a['id']}_{safe_filename(a['file_name'])}"
                font_args.append(f"{a['id']}:{temp_font_path}")

            if "attachments" not in extract_args:
                extract_args += ["attachments"]
            extract_args += font_args

        if extract_args:
            result = run_subprocess(["mkvextract", str(mkv_path), *extract_args], capture_stdout=False)
            if result.returncode > 1:
                raise RuntimeError(f"mkvextract failed (exit {result.returncode}): {result.stderr.strip()}")

        if newly_extracted_sub_path is not None and sub_offset != 0.0 and newly_extracted_sub_path.exists():
            shift_ass_timestamps(newly_extracted_sub_path, sub_offset)

        # Move successfully extracted files to final locations
        if newly_extracted_sub_path and newly_extracted_sub_path.exists():
            final_sub_path.unlink(missing_ok=True)
            shutil.move(str(newly_extracted_sub_path), str(final_sub_path))
            _cleanup_stale_subtitle_offsets(base_sub_path, final_sub_path)

        if final_chapter_path:
            temp_chapter = temp_dir / final_chapter_path.name
            if temp_chapter.exists():
                final_chapter_path.unlink(missing_ok=True)
                shutil.move(str(temp_chapter), str(final_chapter_path))

        if final_cover_path:
            temp_cover = temp_dir / final_cover_path.name
            if temp_cover.exists():
                final_cover_path.unlink(missing_ok=True)
                shutil.move(str(temp_cover), str(final_cover_path))

        if new_font_attachments:
            id_to_meta = {str(a["id"]): a for a in new_font_attachments}

            for temp_font_path in temp_dir.iterdir():
                if not temp_font_path.is_file() or temp_font_path.stat().st_size == 0:
                    continue

                if temp_font_path.name in (final_sub_path.name if final_sub_path else "", 
                                           final_chapter_path.name if final_chapter_path else "", 
                                           final_cover_path.name if final_cover_path else ""):
                    continue

                parts = temp_font_path.name.split("_", 1)
                if len(parts) < 2:
                    continue
                fid, safe_name = parts

                att_meta = id_to_meta.get(fid)
                if not att_meta:
                    continue

                actual_size = temp_font_path.stat().st_size
                mkv_size = att_meta.get("size", 0)

                move_succeeded = False
                with IO_LOCK:
                    final_path = fonts_dir / safe_name
                    if final_path.exists():
                        stem, suffix = final_path.stem, final_path.suffix
                        final_path = fonts_dir / f"{stem}_{actual_size:x}{suffix}"
                    
                    if not final_path.exists():
                        shutil.move(str(temp_font_path), str(final_path))
                        font_registry.try_claim(final_path.name, actual_size)
                        move_succeeded = True
                    else:
                        # File already exists at final_path, delete temp and mark as succeeded
                        temp_font_path.unlink(missing_ok=True)
                        font_registry.try_claim(final_path.name, actual_size)
                        move_succeeded = True
                
                if move_succeeded:
                    extracted_ok.add((safe_name, mkv_size))

            for name, size in claimed_this_call:
                if (name, size) not in extracted_ok:
                    font_registry.release(name, size)
        print(f"  [+] Extracted: {mkv_path.name}")
        return True
    except Exception as e:
        for name, size in claimed_this_call:
            if (name, size) not in extracted_ok:
                font_registry.release(name, size)
        print(f"  [-] Failed: {mkv_path.name} ({e})")
        return False
    finally:
        if temp_dir is not None:
            shutil.rmtree(temp_dir, ignore_errors=True)


def _run(input_dir: Path, fonts_dir: Path, pool: ThreadPoolExecutor, dry_run: bool = False, sub_offset: float = 0.0) -> bool:
    # Use scandir wrapper non-recursively for the target folder
    mkv_files = scan_mkv_files(input_dir, recursive=False)
    if not mkv_files:
        return False

    if dry_run:
        print(f"  [Dry Run] Discovered {len(mkv_files)} file(s):")
        for mkv in mkv_files:
            print(f"    - {mkv.name}")
        return True
    
    fonts_dir.mkdir(parents=True, exist_ok=True)
    print(f"Extracting {len(mkv_files)} file(s)...")

    existing_fonts = set()
    if fonts_dir.is_dir():
        try:
            with os.scandir(str(fonts_dir)) as it:
                for entry in it:
                    if entry.is_file() and Path(entry.name).suffix.lower() in FONT_EXTENSIONS:
                        try:
                            existing_fonts.add((entry.name, entry.stat().st_size))
                        except OSError:
                            pass
        except OSError:
            pass
            
    font_registry = FontRegistry(existing=existing_fonts)

    # Pre-inspect videos using the global pool
    inspect_cache = inspect_videos([mkv.path for mkv in mkv_files], pool)

    success_count = 0
    futures = {
        pool.submit(pipeline_worker, mkv.path, mkv.parsed, inspect_cache.get(mkv.path), fonts_dir, font_registry, sub_offset): mkv
        for mkv in mkv_files
    }
    
    for future in as_completed(futures):
        mkv = futures[future]
        try:
            if future.result():
                success_count += 1
        except Exception as e:
            print(f"  [-] Failed: {mkv.name} ({e})")
                
    print(f"Finished folder: {success_count}/{len(mkv_files)} succeeded.")
    return success_count == len(mkv_files)


def main() -> None:
    cpu_count = os.cpu_count() or 4
    default_workers = min(cpu_count * DEFAULT_WORKER_MULTIPLIER, MAX_DEFAULT_WORKERS) # IO bound adaptive worker count

    parser = argparse.ArgumentParser(description="Fast MKV Extractor")
    parser.add_argument("--workers", type=int, default=default_workers, help="Concurrency level")
    parser.add_argument("--input-dir", type=str, default=None, help="Directory path")
    parser.add_argument("--dry-run", action="store_true", help="Dry run mode")
    parser.add_argument("--sub-offset", type=float, default=0.0, help="Shift extracted subtitle timestamps by this many seconds (negative = earlier, positive = later). Applied once, at extraction time.")
    cfg = parser.parse_args()

    if cfg.workers < 1:
        parser.error("workers count must be at least 1")

    check_dependencies("mkvmerge", "mkvextract")
    root_input_dir = Path(cfg.input_dir).resolve() if cfg.input_dir else SCRIPT_DIR

    # Use scandir recursive scan
    all_mkvs = scan_mkv_files(root_input_dir)
    if not all_mkvs:
        print("[ERROR] No MKV files found.")
        sys.exit(1)

    mkv_dirs = sorted(list(set(mkv.path.parent for mkv in all_mkvs)), key=natural_sort_key)
    overall_success = True
    
    # Global ThreadPool reuse
    with ThreadPoolExecutor(max_workers=cfg.workers) as pool:
        for mkv_dir in mkv_dirs:
            print(f"\nFolder: {mkv_dir}")
            try:
                if not _run(mkv_dir, mkv_dir / "fonts", pool, cfg.dry_run, cfg.sub_offset):
                    overall_success = False
            except Exception as e:
                print(f"  [ERROR] Folder failed: {e}")
                overall_success = False

    if not overall_success:
        sys.exit(1)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nCancelled.")
        sys.exit(130)
