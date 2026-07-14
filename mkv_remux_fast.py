#!/usr/bin/env python3
"""
Batch MKV Re-Mux Utility (High Performance & Simplified)
Optimized for speed and minimal output.
"""

import sys
sys.dont_write_bytecode = True

import re
import os
import shutil
import tempfile
import argparse
import uuid
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path

from mkv_common_fast import (
    COVER_MIME,
    FONT_MIME,
    LANG_LABEL,
    LANG_MKV_TAG,
    check_dependencies,
    detect_sub_lang,
    parse_attachments,
    find_fonts_dir,
    has_chapters,
    is_chapter_xml,
    is_cover_filename,
    is_dialogue_track,
    output_stem,
    parse_episode,
    run_subprocess,
    inspect_videos,
    select_best_subtitle_track,
    natural_sort_key,
    ParsedFile,
    scan_mkv_files,
    scan_assets,
    EpisodeInfo,
    MAX_DEFAULT_WORKERS,
    DEFAULT_WORKER_MULTIPLIER,
)

SCRIPT_DIR = Path(__file__).resolve().parent


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
            if is_ja:
                score = 2
            else:
                score = 1

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


def seasons_match(mkv_season: str, sub_season: str) -> bool:
    if not mkv_season or not sub_season:
        return True
    return mkv_season == sub_season


def _ancestor_dirs(mkv_dir: Path, root_input_dir: Path) -> list[Path]:
    try:
        rel = mkv_dir.relative_to(root_input_dir)
    except ValueError:
        return [mkv_dir]
    dirs = [root_input_dir]
    current = root_input_dir
    for part in rel.parts:
        current = current / part
        dirs.append(current)
    return dirs


@dataclass(frozen=True)
class FontInfo:
    args:  tuple[str, ...]
    count: int


def scan_fonts(fonts_dir: Path) -> FontInfo:
    font_args = []
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


class SubIndex:
    def __init__(self) -> None:
        self._index = {}

    def add(self, sub_file: ParsedFile) -> None:
        if not sub_file.parsed:
            return
        season, ep_type, ep = sub_file.parsed
        lang = detect_sub_lang(sub_file.stem)
        suffix = sub_file.path.suffix.lower()
        key = f"{ep_type}|{ep}"
        existing = self._index.setdefault(key, [])
        for f in existing:
            if f.parsed[0] == season and detect_sub_lang(f.stem) == lang and f.path.suffix.lower() == suffix:
                return
        existing.append(sub_file)

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


class ChapterIndex:
    def __init__(self) -> None:
        self._index = {}
        self._unmatched = []

    def add(self, xml_file: ParsedFile) -> None:
        if not is_chapter_xml(xml_file.path):
            return
        if xml_file.parsed:
            season, ep_type, ep = xml_file.parsed
            key = f"{ep_type}|{ep}"
            existing = self._index.setdefault(key, [])
            for path, s in existing:
                if s == season:
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
    def unmatched(self) -> list[Path]:
        return list(self._unmatched)


_COVER_FILE_RE = re.compile(r'^(?P<stem>.+)_cover$', re.IGNORECASE)


class CoverIndex:
    def __init__(self) -> None:
        self._index = {}
        self._global = None

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
            return

        assert parsed is not None
        season, ep_type, ep = parsed
        key = f"{ep_type}|{ep}"
        existing = self._index.setdefault(key, [])
        for path, s in existing:
            if s == season:
                return
        existing.append((image_file.path, season))

    def lookup(self, ep_type: str, ep: str, mkv_season: str) -> Path | None:
        key = f"{ep_type}|{ep}"
        if key in self._index:
            for path, season in self._index[key]:
                if seasons_match(mkv_season, season):
                    return path
        return self._global


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


def _convert_flac_to_opus(
    video: Path, audio_id: int, audio_index: int, temp_dir: Path,
    base: str, opus_bitrate: str, complexity: int = 10, ffmpeg_threads: int = 2,
) -> Path | None:
    unique_id = uuid.uuid4().hex[:8]
    temp_opus = temp_dir / f"{base}_{unique_id}_audio.opus"

    result = run_subprocess([
        "ffmpeg", "-y", "-nostdin", "-threads", str(ffmpeg_threads),
        "-i", str(video),
        "-map", f"0:a:{audio_index}",
        "-vn", "-c:a", "libopus",
        "-b:a", opus_bitrate,
        "-vbr", "on",
        "-application", "audio",
        "-compression_level", str(complexity),
        str(temp_opus),
    ], capture_stdout=False)
    if result.returncode != 0:
        print(f"  [ERROR] ffmpeg transcoding failed (exit {result.returncode}):\n{result.stderr.strip()}")
        temp_opus.unlink(missing_ok=True)
        return None
    return temp_opus


def _build_mkvmerge_args(
    job: PlannedJob, tr: TrackSelection,
    temp_opus: Path | None, fonts: FontInfo,
    output_path: Path,
) -> list[str]:
    """Construct the mkvmerge argument list for a planned job."""
    args: list[str] = ["-o", str(output_path), "--video-tracks", str(tr.video_id)]

    keep_source_cover = job.cover_file is None and tr.source_cover_id is not None
    keep_attachment_ids = []
    if job.use_source_fonts:
        keep_attachment_ids += tr.source_font_ids
    if keep_source_cover:
        keep_attachment_ids.append(tr.source_cover_id)

    if keep_attachment_ids:
        args += ["--attachments", ",".join(str(i) for i in keep_attachment_ids)]
    else:
        args.append("--no-attachments")

    if job.embedded_sub_id is not None:
        args += ["--subtitle-tracks", str(job.embedded_sub_id), "--default-track-flag", f"{job.embedded_sub_id}:yes"]
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
        args += ["--language", f"0:{track_lang}", "--track-name", f"0:{track_name}", str(temp_opus)]

    if job.subtitle_file is not None:
        args += ["--language", f"0:{job.subtitle_mkv_lang}", "--track-name", f"0:{job.subtitle_label}",
                 "--default-track-flag", "0:yes", str(job.subtitle_file)]

    if fonts.args:
        args.extend(fonts.args)

    if job.cover_file:
        cover_mime = COVER_MIME.get(job.cover_file.suffix.lower(), "image/jpeg")
        args += ["--attachment-name", f"cover{job.cover_file.suffix.lower()}", "--attachment-mime-type", cover_mime,
                 "--attach-file", str(job.cover_file)]

    if job.chapter_file:
        args += ["--chapters", str(job.chapter_file)]

    return args


def _resolve_subtitle(
    video: Path, parsed: EpisodeInfo,
    sub_index: SubIndex, json_data: dict | None,
) -> tuple[Path | None, int | None, str, str, str, bool]:
    mkv_season, mkv_ep_type, mkv_ep = parsed
    sub_result = sub_index.lookup(mkv_ep_type, mkv_ep, mkv_season)

    if sub_result is not None:
        subtitle_file, subtitle_lang = sub_result
        return (subtitle_file, None, subtitle_lang,
                LANG_LABEL.get(subtitle_lang, subtitle_lang),
                LANG_MKV_TAG.get(subtitle_lang, subtitle_lang), True)

    _embedded = select_best_subtitle_track(json_data.get("tracks", [])) if json_data else None
    if _embedded is not None:
        embedded_sub_id, subtitle_lang, _ = _embedded
        return (None, embedded_sub_id, subtitle_lang,
                LANG_LABEL.get(subtitle_lang, subtitle_lang),
                LANG_MKV_TAG.get(subtitle_lang, subtitle_lang), True)

    return (None, None, "und", LANG_LABEL["und"], LANG_MKV_TAG["und"], False)


def pipeline_worker(
    video: Path,
    video_parsed: EpisodeInfo | None,
    json_data: dict | None,
    chapter_fallback_count: int,
    chapter_unmatched_claimed: set[Path],
    chapter_unmatched_claimed_lock: threading.Lock,
    sub_index: SubIndex,
    chap_index: ChapterIndex,
    cover_index: CoverIndex,
    output_dir: Path,
    fonts: FontInfo,
    opus_bitrate: str,
    complexity: int,
    ffmpeg_threads: int,
    temp_parent_dir: Path | None,
    dry_run: bool,
) -> bool:
    if not video_parsed:
        return False

    mkv_season, mkv_ep_type, mkv_ep = video_parsed
    out_stem    = output_stem(video.stem, mkv_ep_type, mkv_ep)
    output_file = output_dir / f"{out_stem}.mkv"
    if output_file.exists() and output_file.stat().st_size > 0:
        return True

    # 1. Inspect (pre-fetched)
    if json_data is None:
        if not dry_run:
            print(f"  [-] Failed: {video.name} (mkvmerge inspection failed)")
            return False
        json_data = {}

    # 2. Plan
    tr = select_tracks(json_data)
    if tr.video_id is None and not dry_run:
        print(f"  [-] Failed: {video.name} (No video track found)")
        return False

    sub_file, embedded_id, sub_lang, sub_label, sub_mkv_lang, matched = _resolve_subtitle(
        video, video_parsed, sub_index, json_data
    )
    if not matched:
        print(f"  [-] Skipped: {video.name} (No matching subtitle found)")
        return False

    chapter_file = chap_index.lookup(mkv_ep_type, mkv_ep, mkv_season)
    if chapter_file is None and not tr.has_source_chapters and chap_index.unmatched:
        with chapter_unmatched_claimed_lock:
            unclaimed = [f for f in chap_index.unmatched if f not in chapter_unmatched_claimed]
            if len(unclaimed) == 1 and chapter_fallback_count == 1:
                chapter_file = unclaimed[0]
                chapter_unmatched_claimed.add(chapter_file)

    cover_file = cover_index.lookup(mkv_ep_type, mkv_ep, mkv_season)

    job = PlannedJob(
        video=video,
        video_name=video.name,
        base=video.stem,
        output_file=output_file,
        subtitle_file=sub_file,
        subtitle_label=sub_label,
        subtitle_mkv_lang=sub_mkv_lang,
        chapter_file=chapter_file,
        cover_file=cover_file,
        embedded_sub_id=embedded_id,
        use_source_fonts=fonts.count == 0,
    )

    if dry_run:
        sub = job.subtitle_file.name if job.subtitle_file else (f"embedded:{job.embedded_sub_id}" if job.embedded_sub_id is not None else "None")
        print(f"  {job.video_name} -> sub: {sub}, chap: {job.chapter_file.name if job.chapter_file else 'none'}, cover: {job.cover_file.name if job.cover_file else 'none'}")
        return True

    # 3. Process
    temp_dir = None
    if temp_parent_dir:
        temp_dir = Path(tempfile.mkdtemp(prefix="job_", dir=temp_parent_dir))

    temp_opus = None
    try:
        if tr.audio_id is not None and tr.audio_codec == "A_FLAC":
            if temp_dir is None:
                raise RuntimeError("temp_dir is required for FLAC→Opus conversion but was None")
            temp_opus = _convert_flac_to_opus(video, tr.audio_id, tr.audio_index, temp_dir, job.base, opus_bitrate, complexity, ffmpeg_threads)
            if temp_opus is None:
                raise RuntimeError("ffmpeg FLAC to Opus conversion failed")

        # Determine temporary output destination to prevent corrupted/partial files in case of interruption
        if temp_dir:
            temp_output = temp_dir / job.output_file.name
        else:
            temp_output = job.output_file.with_suffix(".tmp")

        temp_output.unlink(missing_ok=True)
        args = _build_mkvmerge_args(job, tr, temp_opus, fonts, temp_output)
        run_result = run_subprocess(["mkvmerge", *args], capture_stdout=False)
        if run_result.returncode > 1:
            temp_output.unlink(missing_ok=True)
            raise RuntimeError(f"mkvmerge failed (exit {run_result.returncode}): {run_result.stderr.strip()}")
        
        # Atomically promote the temp file to the final destination on success
        output_file.unlink(missing_ok=True)
        shutil.move(str(temp_output), str(output_file))
        print(f"  [+] Remuxed: {job.video_name}")
        return True
    except Exception as e:
        print(f"  [-] Failed: {job.video_name} ({e})")
        return False
    finally:
        if temp_opus is not None:
            temp_opus.unlink(missing_ok=True)
        if temp_dir is not None:
            shutil.rmtree(temp_dir, ignore_errors=True)


def _run(
    input_dir:    Path,
    temp_parent_dir: Path | None,
    output_dir:   Path,
    fonts_dir:    Path,
    search_dirs:  list[Path],
    pool:         ThreadPoolExecutor,
    workers:      int,
    opus_bitrate: str,
    complexity:   int,
    dry_run:      bool = False,
) -> bool:
    # Discover assets
    fonts = scan_fonts(fonts_dir)
    if fonts.count > 0:
        print(f"Detected {fonts.count} external font(s) in: {fonts_dir.name}")
    else:
        print("No external fonts detected in fonts directory.")

    # Use scandir wrapper non-recursively for the target folder
    videos = scan_mkv_files(input_dir, recursive=False)
    if not videos:
        return False

    sub_index = SubIndex()
    chap_index = ChapterIndex()
    cover_index = CoverIndex()

    # Single-pass assets scan
    for d in reversed(search_dirs):
        assets = scan_assets(d)
        for sub_file in assets.subs:
            sub_index.add(sub_file)
        for xml_file in assets.chapters:
            chap_index.add(xml_file)
        for image_file in assets.covers:
            cover_index.add(image_file)

    # Filter out already remuxed videos to avoid redundant processing
    unprocessed_videos = []
    videos_to_inspect = []
    already_remuxed_count = 0
    parseable_count = 0
    
    for v in videos:
        if not v.parsed:
            continue
        parseable_count += 1
        mkv_season, mkv_ep_type, mkv_ep = v.parsed
        out_stem = output_stem(v.stem, mkv_ep_type, mkv_ep)
        output_file = output_dir / f"{out_stem}.mkv"
        if output_file.exists() and output_file.stat().st_size > 0:
            already_remuxed_count += 1
            continue
            
        unprocessed_videos.append(v)
        
        # During dry runs, we only need to inspect videos that lack an external subtitle (to check for embedded ones)
        if dry_run:
            if sub_index.lookup(mkv_ep_type, mkv_ep, mkv_season) is None:
                videos_to_inspect.append(v.path)
        else:
            videos_to_inspect.append(v.path)

    if not unprocessed_videos:
        if not dry_run:
            print(f"Finished folder: {parseable_count}/{parseable_count} succeeded (all already remuxed).")
        return True

    chapter_fallback_count = sum(1 for v in unprocessed_videos if v.parsed and not chap_index.has_match(v.parsed[1], v.parsed[2], v.parsed[0]))
    chapter_unmatched_claimed = set()
    chapter_unmatched_claimed_lock = threading.Lock()

    if dry_run:
        print("--- DRY RUN: Planned jobs ---")
    else:
        print(f"Remuxing {len(unprocessed_videos)} file(s)...")

    # Distribute available CPU cores evenly among the concurrent workers to prevent core thrashing
    ffmpeg_threads = max(1, (os.cpu_count() or workers) // workers)

    # Create temp dir lazily only when there is real work to do
    temp_dir: Path | None = None
    if not dry_run and temp_parent_dir is not None:
        temp_dir = Path(tempfile.mkdtemp(prefix=".tmp_remux_", dir=temp_parent_dir))

    try:
        # Pre-inspect only the videos that actually need it
        inspect_cache = inspect_videos(videos_to_inspect, pool) if videos_to_inspect else {}

        success_count = already_remuxed_count
        futures = {
            pool.submit(
                pipeline_worker,
                v.path,
                v.parsed,
                inspect_cache.get(v.path),
                chapter_fallback_count,
                chapter_unmatched_claimed,
                chapter_unmatched_claimed_lock,
                sub_index,
                chap_index,
                cover_index,
                output_dir,
                fonts,
                opus_bitrate,
                complexity,
                ffmpeg_threads,
                temp_dir,
                dry_run,
            ): v
            for v in unprocessed_videos
        }

        for future in as_completed(futures):
            v = futures[future]
            try:
                if future.result():
                    success_count += 1
            except Exception as e:
                print(f"  [-] Failed: {v.name} ({e})")

        print(f"Finished folder: {success_count}/{parseable_count} succeeded.")
        return success_count == parseable_count
    finally:
        if temp_dir is not None:
            shutil.rmtree(temp_dir, ignore_errors=True)


def main() -> None:
    cpu_count = os.cpu_count() or 4
    default_workers = min(cpu_count * DEFAULT_WORKER_MULTIPLIER, MAX_DEFAULT_WORKERS) # I/O bound adaptive worker count

    parser = argparse.ArgumentParser(description="Fast MKV Remuxer")
    parser.add_argument("--workers", type=int, default=default_workers, help="Concurrency level")
    parser.add_argument("--bitrate", type=str, default="192k", help="Audio target bitrate")
    parser.add_argument("--complexity", type=int, default=10, help="Opus complexity")
    parser.add_argument("--out-dir", type=str, default=None, help="Output directory")
    parser.add_argument("--input-dir", type=str, default=None, help="Input directory")
    parser.add_argument("--dry-run", action="store_true", help="Dry run mode")
    parser.add_argument("--keep-fonts", action="store_true", help="Keep temporary fonts folder")
    cfg = parser.parse_args()

    if cfg.workers < 1:
        parser.error("workers count must be at least 1")

    check_dependencies("mkvmerge", "ffmpeg")
    root_input_dir = Path(cfg.input_dir).resolve() if cfg.input_dir else SCRIPT_DIR
    out_override = Path(cfg.out_dir).resolve() if cfg.out_dir else None

    ignore_paths = {out_override} if out_override else None
    all_mkvs = scan_mkv_files(root_input_dir, ignore_paths=ignore_paths)
    if not all_mkvs:
        print("[ERROR] No MKV files found.")
        sys.exit(1)

    mkv_dirs = sorted(list(set(mkv.path.parent for mkv in all_mkvs)), key=natural_sort_key)
    overall_success = True

    # Global thread pool reuse
    with ThreadPoolExecutor(max_workers=cfg.workers) as pool:
        for mkv_dir in mkv_dirs:
            print(f"\nFolder: {mkv_dir}")
            current_output_dir = out_override if out_override else mkv_dir / "Output"
            if not cfg.dry_run:
                current_output_dir.mkdir(parents=True, exist_ok=True)
            search_dirs = _ancestor_dirs(mkv_dir, root_input_dir)

            # Resolve fonts directory
            current_fonts_dir = None
            for d in reversed(search_dirs):
                candidate = find_fonts_dir(d)
                if candidate.is_dir():
                    current_fonts_dir = candidate
                    break
            if current_fonts_dir is None:
                current_fonts_dir = mkv_dir / "fonts"

            try:
                success = _run(mkv_dir, mkv_dir if not cfg.dry_run else None, current_output_dir, current_fonts_dir, search_dirs, pool, cfg.workers, cfg.bitrate, cfg.complexity, cfg.dry_run)
                if not success:
                    overall_success = False
                
                # Clean up fonts folder if successful and keep-fonts wasn't set
                if success and not cfg.dry_run and not cfg.keep_fonts and current_fonts_dir.is_dir() and current_fonts_dir.name == "fonts" and current_fonts_dir.parent == mkv_dir:
                    try:
                        shutil.rmtree(current_fonts_dir)
                    except Exception:
                        pass
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
