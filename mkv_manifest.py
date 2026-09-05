"""Typed inspection manifest generation for desktop sidecar consumers."""
from __future__ import annotations

import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import TypedDict

from mkv_assets import SubIndex
from mkv_domain import ancestor_dirs
from mkv_events import get_item_id
from mkv_proc import CancelToken
from mkv_scan import (
    InspectedVideo,
    MkvTrack,
    ParsedFile,
    PersistentInspectCache,
    ScanResults,
    TrackProperties,
    inspect_videos,
    scan_assets,
    scan_mkv_files,
)
from mkv_storage import resolve_io_jobs


class InspectionMetadata(TypedDict):
    resolution: str | None
    video_codec: str | None
    video_tag: str | None
    audio_summary: str | None
    has_flac: bool | None
    sub_summary: str | None
    fonts_count: int | None


class RequiredManifestItem(TypedDict):
    id: str
    name: str
    release: str
    metadata: InspectionMetadata


class ManifestItem(RequiredManifestItem, total=False):
    inspection_error: str


class InspectionManifest(TypedDict):
    version: int
    subtitle_candidates: dict[str, list[str]]
    items: list[ManifestItem]


def _parse_resolution(pixel_dimensions: str) -> str | None:
    if not pixel_dimensions:
        return None
    parts = pixel_dimensions.split("x")
    if len(parts) == 2:
        try:
            width, height = int(parts[0].strip()), int(parts[1].strip())
            if width >= 3800 or height >= 2100:
                return "4K"
            if width >= 2500 or height >= 1400:
                return "1440p"
            if width >= 1900 or height >= 1000:
                return "1080p"
            if width >= 1200 or height >= 700:
                return "720p"
            if height >= 480:
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
    combined = f"{codec} {codec_id}".lower()
    if "hevc" in combined or "h.265" in combined:
        return "HEVC"
    if "avc" in combined or "h.264" in combined:
        return "AVC"
    if "av1" in combined:
        return "AV1"
    if "vp9" in combined:
        return "VP9"
    return codec or None


_CHANNEL_LABELS = {8: "7.1", 6: "5.1", 1: "1.0"}


def _parse_audio_track_info(track: MkvTrack) -> tuple[str, bool]:
    codec = track.get("codec", "")
    properties: TrackProperties = track.get("properties", {})
    codec_id = properties.get("codec_id", "")
    combined = f"{codec} {codec_id}".lower()
    is_flac = "flac" in combined
    channels = properties.get("audio_channels")
    if channels is not None:
        # mkvmerge reports total channels including LFE: 6 -> 5.1, 8 -> 7.1.
        channel_label = _CHANNEL_LABELS.get(
            channels,
            f"{channels}.0" if channels <= 2 else f"{channels - 1}.1",
        )
    else:
        channel_label = None

    if is_flac:
        audio_name = "FLAC"
    elif "aac" in combined:
        audio_name = "AAC"
    elif "opus" in combined:
        audio_name = "Opus"
    elif "ac3" in combined or "ac-3" in combined:
        audio_name = "AC3"
    elif codec:
        audio_name = codec
    else:
        audio_name = "Audio"

    if channel_label is not None:
        return f"{audio_name} {channel_label}", is_flac
    return audio_name, is_flac


def extract_mkv_metadata(inspected: InspectedVideo | None) -> InspectionMetadata:
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

    resolution: str | None = None
    video_codec: str | None = None
    audio_parts: list[str] = []
    has_flac = False
    has_audio = False
    subtitle_languages: list[str] = []
    subtitle_formats: list[str] = []

    for track in inspected.tracks:
        track_type = track.get("type", "")
        properties: TrackProperties = track.get("properties", {})

        if track_type == "video":
            video_codec = _parse_video_codec(track.get("codec", ""), properties.get("codec_id", ""))
            resolution = _parse_resolution(properties.get("pixel_dimensions", ""))
        elif track_type == "audio":
            has_audio = True
            audio, flac_found = _parse_audio_track_info(track)
            if flac_found:
                has_flac = True
            audio_parts.append(audio)
        elif track_type == "subtitles":
            combined = f"{track.get('codec', '')} {properties.get('codec_id', '')}".lower()
            if "ass" in combined or "substation" in combined:
                format_name = "ASS"
            elif "srt" in combined or "subrip" in combined or "utf8" in combined:
                format_name = "SRT"
            elif "pgs" in combined or "hdmv" in combined:
                format_name = "PGS"
            elif "vob" in combined:
                format_name = "VobSub"
            else:
                format_name = "Subtitles"

            if format_name not in subtitle_formats:
                subtitle_formats.append(format_name)

            language = (properties.get("language") or "und").upper()
            if language not in subtitle_languages:
                subtitle_languages.append(language)

    video_tag = f"{resolution} {video_codec}" if resolution and video_codec else None
    audio_summary = " / ".join(audio_parts) if audio_parts else (None if not has_audio else "No Audio")

    if subtitle_languages:
        subtitle_format = subtitle_formats[0] if len(subtitle_formats) == 1 else "Subs"
        subtitle_summary = f"{subtitle_format} ({', '.join(subtitle_languages)})"
    elif subtitle_formats:
        subtitle_summary = subtitle_formats[0] if len(subtitle_formats) == 1 else "Subtitles"
    else:
        subtitle_summary = None

    return {
        "resolution": resolution,
        "video_codec": video_codec,
        "video_tag": video_tag,
        "audio_summary": audio_summary,
        "has_flac": has_flac if has_audio else None,
        "sub_summary": subtitle_summary,
        "fonts_count": inspected.fonts_count(),
    }


def generate_inspection_manifest(
    input_dir: Path,
    persistent_cache: PersistentInspectCache | None = None,
    cancel_token: CancelToken | None = None,
    *,
    workers: int | None = None,
    io_jobs: int | None = None,
) -> InspectionManifest:
    all_mkvs = scan_mkv_files(input_dir)
    if not all_mkvs:
        return {"version": 2, "subtitle_candidates": {}, "items": []}

    # Pool stays wide so cache validation runs unrestricted; actual mkvmerge
    # probes are throttled to the storage-appropriate concurrency.
    worker_limit = workers if workers is not None else 16
    pool_workers = min(len(all_mkvs), worker_limit)
    probe_limit = resolve_io_jobs(io_jobs, input_dir)
    probe_semaphore = threading.BoundedSemaphore(probe_limit) if probe_limit > 0 else None
    with ThreadPoolExecutor(max_workers=pool_workers) as pool:
        inspection_results = inspect_videos(
            [m.path for m in all_mkvs],
            pool=pool,
            persistent_cache=persistent_cache,
            cancel_token=cancel_token,
            probe_semaphore=probe_semaphore,
        )

    videos_by_dir: dict[Path, list[ParsedFile]] = {}
    for video in all_mkvs:
        videos_by_dir.setdefault(video.path.parent, []).append(video)

    scan_cache: dict[Path, ScanResults] = {}
    release_by_dir: dict[Path, str] = {}
    subtitle_candidates: dict[str, list[str]] = {}
    for mkv_dir in sorted(videos_by_dir):
        release = get_item_id(mkv_dir, input_dir)
        release_by_dir[mkv_dir] = release
        # Reuse candidate ordering without discovering fonts or validating chapters.
        sub_index = SubIndex()
        for directory in reversed(ancestor_dirs(mkv_dir, input_dir)):
            if cancel_token is not None:
                cancel_token.check()
            for subtitle in scan_assets(directory, scan_cache).subs:
                sub_index.add(subtitle)
        candidates = [get_item_id(path, input_dir) for path in sub_index.all_subtitles]
        if candidates:
            subtitle_candidates[release] = candidates

    if persistent_cache is not None:
        persistent_cache.save()

    items: list[ManifestItem] = []
    for mkv in all_mkvs:
        inspection = inspection_results[mkv.path]
        item: ManifestItem = {
            "id": get_item_id(mkv.path, input_dir),
            "name": mkv.name,
            "release": release_by_dir[mkv.path.parent],
            "metadata": extract_mkv_metadata(inspection.video),
        }
        if inspection.error is not None:
            item["inspection_error"] = inspection.error
        items.append(item)

    return {"version": 2, "subtitle_candidates": subtitle_candidates, "items": items}
