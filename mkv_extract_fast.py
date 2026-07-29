#!/usr/bin/env python3
"""
MKV Track & Font Extractor (High Performance & Simplified)
Optimized for speed and minimal output.
"""
from __future__ import annotations

import sys
sys.dont_write_bytecode = True
if hasattr(sys.stdout, "reconfigure"):
    try:
        sys.stdout.reconfigure(line_buffering=True)
    except Exception:
        pass

import builtins
_orig_print = builtins.print
def print(*args, **kwargs):
    kwargs.setdefault("flush", True)
    _orig_print(*args, **kwargs)

import time
import argparse
import os
import re
import shutil
import tempfile
import threading
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path

from mkv_common_fast import (
    FONT_EXTENSIONS,
    FontRegistry,
    acquire_semaphore,
    check_dependencies,
    ep_type_label,
    parse_attachments,
    has_chapters,
    run_subprocess,
    inspect_videos,
    PersistentInspectCache,
    safe_filename,
    select_best_subtitle_track,
    shift_ass_timestamps,
    natural_sort_key,
    scan_mkv_files,
    resolve_output_dir,
    EpisodeInfo,
    ParsedFile,
    MAX_DEFAULT_WORKERS,
    DEFAULT_WORKER_MULTIPLIER,
    cleanup_stale_subtitle_offsets_bulk as _cleanup_stale_subtitle_offsets_bulk,
)

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


SCRIPT_DIR = Path(__file__).resolve().parent
SUPPORTED_SUB_CODECS = frozenset({"s_text/ass"})
IO_LOCK = threading.Lock()



def pipeline_worker(
    mkv_path: Path,
    mkv_parsed: EpisodeInfo | None,
    json_data: dict | None,
    fonts_dir: Path,
    font_registry: FontRegistry,
    stale_subs_map: dict[str, str],
    stale_subs_lock: threading.Lock,
    sub_offset: float = 0.0,
    io_semaphore: threading.Semaphore | None = None,
    output_dir: Path | None = None,
) -> bool:
    start_time = time.perf_counter()
    print(f"[JOB_START] {mkv_path.name}")
    # 1. Inspect (pre-fetched)
    if json_data is None:
        print(f"[JOB_FAIL] {mkv_path.name} | mkvmerge inspection failed")
        print(f"  [-] Failed: {mkv_path.name} (mkvmerge inspection failed)")
        return False

    meta = parse_mkv_metadata(json_data)
    file_stem = mkv_path.stem
    file_dir  = output_dir if output_dir is not None else mkv_path.parent

    if mkv_parsed:
        ep_tag = f"{ep_type_label(mkv_parsed[1])}{mkv_parsed[2]}"
        chapter_stem = f"{file_stem}_{ep_tag}"
    else:
        ep_tag = file_stem
        chapter_stem = file_stem

    temp_dir: Path | None = None
    claimed_this_call: list[tuple[str, int]] = []
    extracted_ok: set[tuple[str, int]] = set()

    def get_temp_dir() -> Path:
        nonlocal temp_dir
        if temp_dir is None:
            temp_dir = Path(tempfile.mkdtemp(prefix="extract_job_", dir=file_dir))
        return temp_dir

    try:
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
                temp_sub_path = get_temp_dir() / sub_path.name
                extract_args += ["tracks", f"{track_id}:{temp_sub_path}"]
                newly_extracted_sub_path = temp_sub_path

        final_chapter_path = None
        if meta.has_chapters:
            chapter_path = file_dir / f"{chapter_stem}_chapters.xml"
            final_chapter_path = chapter_path
            if not chapter_path.exists() or chapter_path.stat().st_size == 0:
                temp_chapter_path = get_temp_dir() / chapter_path.name
                extract_args += ["chapters", str(temp_chapter_path)]

        final_cover_path = None
        cover, font_attachments = parse_attachments(meta.attachments)
        if cover and cover.get("id") is not None and cover.get("file_name"):
            ext = Path(cover["file_name"]).suffix.lower() or ".jpg"
            cover_path = file_dir / f"{file_stem}_cover{ext}"
            final_cover_path = cover_path
            if not cover_path.exists() or cover_path.stat().st_size == 0:
                temp_cover_path = get_temp_dir() / cover_path.name
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
                temp_font_path = get_temp_dir() / f"{a['id']}_{safe_filename(a['file_name'])}"
                font_args.append(f"{a['id']}:{temp_font_path}")

            if "attachments" not in extract_args:
                extract_args += ["attachments"]
            extract_args += font_args

        if extract_args:
            with acquire_semaphore(io_semaphore):
                result = run_subprocess(["mkvextract", str(mkv_path), *extract_args], capture_stdout=False)
            if result.returncode > 1:
                raise RuntimeError(f"mkvextract failed (exit {result.returncode}): {result.stderr.strip()}")

        if newly_extracted_sub_path is not None and sub_offset != 0.0 and newly_extracted_sub_path.exists():
            shift_ass_timestamps(newly_extracted_sub_path, sub_offset)

        # Move successfully extracted files to final locations
        if newly_extracted_sub_path and newly_extracted_sub_path.exists():
            final_sub_path.unlink(missing_ok=True)
            shutil.move(str(newly_extracted_sub_path), str(final_sub_path))

        if final_sub_path and final_sub_path.exists():
            with stale_subs_lock:
                stale_subs_map[base_sub_path.stem] = final_sub_path.name

        if temp_dir is not None:
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

                    with IO_LOCK:
                        final_path = fonts_dir / safe_name
                        if final_path.exists():
                            stem, suffix = final_path.stem, final_path.suffix
                            final_path = fonts_dir / f"{stem}_{actual_size:x}{suffix}"
                        
                        if not final_path.exists():
                            shutil.move(str(temp_font_path), str(final_path))
                        else:
                            temp_font_path.unlink(missing_ok=True)
                        
                        font_registry.try_claim(final_path.name, actual_size)
                    extracted_ok.add((safe_name, mkv_size))

                for name, size in claimed_this_call:
                    if (name, size) not in extracted_ok:
                        font_registry.release(name, size)
        action = "Extracted" if extract_args else "Skipped (already complete)"
        elapsed = time.perf_counter() - start_time
        print(f"[JOB_DONE] {mkv_path.name} | {elapsed:.1f}s")
        print(f"  [+] {action}: {mkv_path.name} in {elapsed:.1f}s")
        return True
    except Exception as e:
        for name, size in claimed_this_call:
            if (name, size) not in extracted_ok:
                font_registry.release(name, size)
        print(f"[JOB_FAIL] {mkv_path.name} | {e}")
        print(f"  [-] Failed: {mkv_path.name} ({e})")
        return False
    finally:
        if temp_dir is not None:
            shutil.rmtree(temp_dir, ignore_errors=True)


def _run(output_dir: Path, fonts_dir: Path, mkv_files: list[ParsedFile], pool: ThreadPoolExecutor, inspect_cache: dict[Path, dict | None] | None = None, dry_run: bool = False, sub_offset: float = 0.0, persistent_cache: PersistentInspectCache | None = None, io_semaphore: threading.Semaphore | None = None) -> bool:
    if not mkv_files:
        return False

    if dry_run:
        print(f"  [Dry Run] Discovered {len(mkv_files)} file(s):")
        for mkv in mkv_files:
            print(f"[JOB_DRY_RUN] {mkv.name}")
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
    stale_subs_map: dict[str, str] = {}
    stale_subs_lock = threading.Lock()

    if inspect_cache is None:
        inspect_cache = inspect_videos([mkv.path for mkv in mkv_files], pool, persistent_cache=persistent_cache)

    success_count = 0
    futures = {
        pool.submit(
            pipeline_worker,
            mkv.path,
            mkv.parsed,
            inspect_cache.get(mkv.path),
            fonts_dir,
            font_registry,
            stale_subs_map,
            stale_subs_lock,
            sub_offset,
            io_semaphore,
            output_dir=output_dir,
        ): mkv
        for mkv in mkv_files
    }
    
    for future in as_completed(futures):
        mkv = futures[future]
        try:
            if future.result():
                success_count += 1
        except Exception as e:
            print(f"  [-] Failed: {mkv.name} ({e})")

    _cleanup_stale_subtitle_offsets_bulk(output_dir, stale_subs_map)

    print(f"Finished folder: {success_count}/{len(mkv_files)} succeeded.")
    return success_count == len(mkv_files)


def main() -> None:
    cpu_count = os.cpu_count() or 4
    default_workers = min(cpu_count * DEFAULT_WORKER_MULTIPLIER, MAX_DEFAULT_WORKERS) # IO bound adaptive worker count

    parser = argparse.ArgumentParser(description="Fast MKV Extractor")
    parser.add_argument("--workers", type=int, default=default_workers, help="Concurrency level")
    parser.add_argument("--out-dir", type=str, default=None, help="Directory where extracted assets (subtitles, covers, chapters, fonts) will be saved.")
    parser.add_argument("--input-dir", type=str, default=None, help="Directory path")
    parser.add_argument("--fonts-dir", type=str, default=None, help="Directory to save extracted fonts (defaults to <out-dir>/fonts or <input-dir>/fonts).")
    parser.add_argument("--dry-run", action="store_true", help="Dry run mode")
    parser.add_argument("--sub-offset", type=float, default=0.0, help="Shift extracted subtitle timestamps by this many seconds (negative = earlier, positive = later). Applied once, at extraction time.")
    parser.add_argument("--io-jobs", type=int, default=2, help="Maximum concurrent disk I/O jobs for mkvextract. Set to 0 to disable throttling.")
    parser.add_argument("--no-cache", action="store_true", help="Bypass persistent inspection cache")
    parser.add_argument("--clear-cache", action="store_true", help="Clear persistent inspection cache")
    cfg = parser.parse_args()

    if cfg.workers < 1:
        parser.error("workers count must be at least 1")

    if cfg.clear_cache:
        PersistentInspectCache().clear()
        print("Persistent inspection cache cleared.")
        sys.exit(0)

    persistent_cache = PersistentInspectCache(enabled=not cfg.no_cache)
    io_semaphore = threading.BoundedSemaphore(cfg.io_jobs) if cfg.io_jobs > 0 else None

    check_dependencies("mkvmerge", "mkvextract")
    root_input_dir = Path(cfg.input_dir).resolve() if cfg.input_dir else SCRIPT_DIR
    out_override = Path(cfg.out_dir).resolve() if cfg.out_dir else None
    fonts_override = Path(cfg.fonts_dir).resolve() if cfg.fonts_dir else None

    # Use scandir recursive scan
    ignore_paths = {out_override} if out_override else None
    all_mkvs = scan_mkv_files(root_input_dir, ignore_paths=ignore_paths)
    if not all_mkvs:
        print("[ERROR] No MKV files found.")
        sys.exit(1)

    mkvs_by_dir: dict[Path, list[ParsedFile]] = defaultdict(list)
    for mkv in all_mkvs:
        mkvs_by_dir[mkv.path.parent].append(mkv)

    mkv_dirs = sorted(list(mkvs_by_dir.keys()), key=natural_sort_key)
    overall_success = True
    
    # Global ThreadPool reuse
    with ThreadPoolExecutor(max_workers=cfg.workers) as pool:
        inspect_cache = inspect_videos([mkv.path for mkv in all_mkvs], pool, persistent_cache=persistent_cache) if not cfg.dry_run else {}
        for mkv_dir in mkv_dirs:
            print(f"\nFolder: {mkv_dir}")
            try:
                current_output_dir = resolve_output_dir(
                    mkv_dir,
                    root_input_dir,
                    out_override,
                    default_subdir=None,
                )

                if not cfg.dry_run:
                    current_output_dir.mkdir(parents=True, exist_ok=True)

                if fonts_override:
                    current_fonts_dir = fonts_override
                elif out_override:
                    current_fonts_dir = out_override / "fonts"
                else:
                    current_fonts_dir = mkv_dir / "fonts"

                dir_mkvs = mkvs_by_dir[mkv_dir]
                if not _run(current_output_dir, current_fonts_dir, dir_mkvs, pool, inspect_cache=inspect_cache, dry_run=cfg.dry_run, sub_offset=cfg.sub_offset, persistent_cache=persistent_cache, io_semaphore=io_semaphore):
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
