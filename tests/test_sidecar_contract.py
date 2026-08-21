"""Tests for Sidecar Contract, --plan mode, and event emissions."""
from __future__ import annotations

import io
import json
import sys
from pathlib import Path
from unittest.mock import patch

import pytest

import mkv_assets
import mkv_common
import mkv_extract
import mkv_remux
from mkv_assets import AssetResolver
from mkv_common import (
    FontInfo,
    ParsedFile,
    emit_event,
    set_events_enabled,
)


def test_emit_event_stdout_format(monkeypatch):
    """emit_event must produce a single @event: JSON-line on stdout with version: 1 when enabled."""
    buf = io.StringIO()
    monkeypatch.setattr(sys, "stdout", buf)

    # Disabled by default
    set_events_enabled(False)
    emit_event("test_event", item_id="ep01.mkv", file="ep01.mkv", status="completed", count=42)
    assert buf.getvalue() == ""

    # Enabled
    set_events_enabled(True)
    emit_event("test_event", item_id="Season 01/ep01.mkv", file="ep01.mkv", status="completed", count=42)
    set_events_enabled(False)

    output = buf.getvalue()
    assert output.startswith("@event: ")
    assert output.endswith("\n")

    payload = json.loads(output[8:].strip())
    assert payload["version"] == 1
    assert payload["type"] == "test_event"
    assert payload["id"] == "Season 01/ep01.mkv"
    assert payload["file"] == "ep01.mkv"
    assert payload["status"] == "completed"
    assert payload["count"] == 42


def test_plan_mode_cli_output(tmp_path, monkeypatch, capsys):
    """mkv-remux --plan outputs valid JSON object with version: 1 and items list."""
    sub_dir = tmp_path / "Season 01"
    sub_dir.mkdir()
    mkv1 = sub_dir / "[Fansub] Show - 01 [1080p].mkv"
    mkv1.write_bytes(b"dummy1")
    mkv2 = tmp_path / "[Fansub] Show - 02 [1080p].mkv"
    mkv2.write_bytes(b"dummy2")

    mock_json = {
        "tracks": [
            {
                "id": 0,
                "type": "video",
                "codec": "HEVC",
                "properties": {"pixel_dimensions": "1920x1080"},
            },
            {
                "id": 1,
                "type": "audio",
                "codec": "FLAC",
                "properties": {"audio_channels": 2, "codec_id": "A_FLAC"},
            },
            {
                "id": 2,
                "type": "subtitles",
                "properties": {"language": "ind", "codec_id": "S_TEXT/ASS"},
            },
        ],
        "attachments": [
            {"id": 1, "file_name": "font.ttf", "content_type": "application/x-truetype-font"},
            {"id": 2, "file_name": "cover.jpg", "content_type": "image/jpeg"},
        ],
    }

    argv = ["mkv_remux.py", "--plan", "--input-dir", str(tmp_path)]
    with patch.object(sys, "argv", argv), \
         patch.object(mkv_common, "run_mkvmerge_json", return_value=mock_json):
        with pytest.raises(SystemExit) as exc:
            mkv_remux.main()
        assert exc.value.code == 0

    captured = capsys.readouterr()
    manifest = json.loads(captured.out.strip())
    assert manifest["version"] == 1
    items = manifest["items"]
    assert len(items) == 2
    for item in items:
        assert "id" in item
        assert "name" in item
        assert item["name"].endswith(".mkv")
        meta = item["metadata"]
        assert meta["resolution"] == "1080p"
        assert meta["video_codec"] == "HEVC"
        assert meta["video_tag"] == "1080p HEVC"
        assert meta["audio_summary"] == "FLAC 2.0"
        assert meta["has_flac"] is True
        assert "IND" in meta["sub_summary"]
        assert meta["fonts_count"] == 1  # Only the true font attachment, not the cover


def test_plan_mode_duplicate_basenames_distinct_ids(tmp_path, monkeypatch, capsys):
    """Items with identical basenames in different subdirectories have unique root-relative POSIX IDs."""
    s1 = tmp_path / "Season 01"
    s2 = tmp_path / "Season 02"
    s1.mkdir()
    s2.mkdir()
    (s1 / "Show - 01.mkv").write_bytes(b"s1e1")
    (s2 / "Show - 01.mkv").write_bytes(b"s2e1")

    argv = ["mkv_remux.py", "--plan", "--input-dir", str(tmp_path)]
    with patch.object(sys, "argv", argv), \
         patch.object(mkv_common, "run_mkvmerge_json", return_value={"tracks": []}):
        with pytest.raises(SystemExit) as exc:
            mkv_remux.main()
        assert exc.value.code == 0

    captured = capsys.readouterr()
    manifest = json.loads(captured.out.strip())
    ids = [item["id"] for item in manifest["items"]]
    assert len(ids) == 2
    assert "Season 01/Show - 01.mkv" in ids
    assert "Season 02/Show - 01.mkv" in ids


def test_dry_run_skip_event_keeps_parenthesized_filename_id(tmp_path, monkeypatch):
    video_path = tmp_path / "Show - 01 (BD 1080p).mkv"
    video = mkv_common._create_parsed_file(video_path)
    resolver = AssetResolver.discover([tmp_path], tmp_path / "fonts")
    emitted = []
    monkeypatch.setattr(mkv_remux, "emit_event", lambda event_type, **payload: emitted.append((event_type, payload)))

    mkv_remux._plan_jobs(
        [video],
        {video_path: None},
        resolver,
        output_dir=tmp_path / "Output",
        dry_run=True,
        root_input_dir=tmp_path,
    )

    terminal = [payload for event_type, payload in emitted if event_type == "item_complete"]
    assert len(terminal) == 1
    assert terminal[0]["item_id"] == "Show - 01 (BD 1080p).mkv"


def test_plan_metadata_keeps_unknown_resolution_null():
    metadata = mkv_remux._extract_mkv_metadata({
        "tracks": [{
            "type": "video",
            "codec": "AVC",
            "properties": {"pixel_dimensions": "640x360"},
        }],
    })

    assert metadata["resolution"] is None
    assert metadata["video_tag"] is None


def test_plan_mode_stdout_purity_corrupt_cache(tmp_path, monkeypatch, capsys):
    """Ensure --plan mode stdout is pure JSON even when PersistentInspectCache fails to load due to corrupt JSON."""
    mkv1 = tmp_path / "Show - 01.mkv"
    mkv1.write_bytes(b"dummy1")

    cache_file = tmp_path / "corrupt_cache.json"
    cache_file.write_text("{ bad json")

    monkeypatch.setattr(mkv_common, "get_default_cache_file", lambda: cache_file)

    argv = ["mkv_remux.py", "--plan", "--input-dir", str(tmp_path)]
    with patch.object(sys, "argv", argv):
        with pytest.raises(SystemExit) as exc:
            mkv_remux.main()
        assert exc.value.code == 0

    captured = capsys.readouterr()
    manifest = json.loads(captured.out)
    assert manifest["version"] == 1
    assert isinstance(manifest["items"], list)
    assert "Failed to load inspect cache" in captured.err


def test_plan_mode_stdout_purity_with_mock(tmp_path, monkeypatch, capsys):
    """Ensure --plan mode stdout is pure JSON even when warnings occur later during inspection."""
    import logging

    mkv1 = tmp_path / "Show - 01.mkv"
    mkv1.write_bytes(b"dummy1")

    def mock_inspect(*args, **kwargs):
        logging.getLogger().warning("This is a mock warning that must not corrupt stdout")
        return {mkv1: {"tracks": []}}

    monkeypatch.setattr(mkv_remux, "inspect_videos", mock_inspect)

    argv = ["mkv_remux.py", "--plan", "--input-dir", str(tmp_path)]
    with patch.object(sys, "argv", argv):
        with pytest.raises(SystemExit) as exc:
            mkv_remux.main()
        assert exc.value.code == 0

    captured = capsys.readouterr()
    manifest = json.loads(captured.out)
    assert manifest["version"] == 1
    assert len(manifest["items"]) == 1
    assert "This is a mock warning that must not corrupt stdout" in captured.err


def test_asset_resolver_discovery_and_resolve_for(tmp_path):
    """AssetResolver.discover correctly indexes ancestor hierarchies and resolves per episode."""
    root = tmp_path / "Anime"
    season = root / "Season 01"
    season.mkdir(parents=True)

    # Assets in season
    (season / "Show - S01E01.ass").write_bytes(b"sub1")
    (season / "Show - S01E01_chapters.xml").write_bytes(b"<Chapters><EditionEntry></EditionEntry></Chapters>")
    (season / "Show - 01_cover.jpg").write_bytes(b"cover1")

    fonts_dir = season / "fonts"
    fonts_dir.mkdir()
    (fonts_dir / "arial.ttf").write_bytes(b"fontdata")

    resolver = AssetResolver.discover([root, season], fonts_dir)
    assert resolver.fonts.count == 1
    assert resolver.sub_index.indexed_count == 1
    assert resolver.chap_index.indexed_count == 1
    assert resolver.cover_index.indexed_count == 1

    video = ParsedFile(
        path=season / "Show - S01E01.mkv",
        name="Show - S01E01.mkv",
        stem="Show - S01E01",
        parsed=("1", "E", "01"),
    )

    resolved = resolver.resolve_for(video, json_data=None)
    assert resolved.matched_subtitle is True
    assert resolved.subtitle_file == season / "Show - S01E01.ass"
    assert resolved.chapter_file == season / "Show - S01E01_chapters.xml"
    assert resolved.cover_file == season / "Show - 01_cover.jpg"
    assert resolved.fonts.count == 1


def test_crashed_job_emits_error_event(tmp_path, monkeypatch):
    """Crashed remux and extract workers must emit error status event."""
    emitted = []

    def mock_emit(event_type, **payload):
        emitted.append((event_type, payload))

    monkeypatch.setattr(mkv_remux, "emit_event", mock_emit)

    job = mkv_remux.PlannedJob(
        video=tmp_path / "crash.mkv",
        video_name="crash.mkv",
        base="crash",
        output_file=tmp_path / "out.mkv",
        subtitle_file=None,
        subtitle_label="Indonesian",
        subtitle_mkv_lang="ind",
        chapter_file=None,
        cover_file=None,
        embedded_sub_id=None,
        use_source_fonts=False,
        item_id="crash.mkv",
    )

    with patch.object(mkv_remux, "process_job", side_effect=RuntimeError("Subprocess exploded")):
        from concurrent.futures import ThreadPoolExecutor
        with ThreadPoolExecutor(max_workers=1) as pool:
            skipped_files = []
            mkv_remux._execute_jobs(
                jobs=[job],
                temp_dir=None,
                fonts=FontInfo(args=(), count=0),
                opus_bitrate="192k",
                pool=pool,
                max_workers=1,
                ffmpeg_threads=1,
                json_cache={job.video: {}},
                skipped_files=skipped_files,
            )

    assert len(skipped_files) == 1
    assert "exception: Subprocess exploded" in skipped_files[0]
    error_events = [e for e in emitted if e[0] == "item_complete" and e[1].get("status") == "error"]
    assert len(error_events) == 1
    assert error_events[0][1]["file"] == "crash.mkv"
    assert error_events[0][1]["item_id"] == "crash.mkv"


def test_extract_mkv_metadata_4k_hevc_flac():
    json_val = {
        "tracks": [
            {
                "type": "video",
                "codec": "HEVC/H.265",
                "properties": {
                    "pixel_dimensions": "3840x2160"
                }
            },
            {
                "type": "audio",
                "codec": "FLAC",
                "properties": {
                    "audio_channels": 6
                }
            },
            {
                "type": "subtitles",
                "codec": "SubStationAlpha",
                "properties": {
                    "language": "ind"
                }
            }
        ],
        "attachments": [
            {"id": 1, "file_name": "font.ttf", "content_type": "font/ttf"}
        ]
    }

    meta = mkv_remux._extract_mkv_metadata(json_val)
    assert meta["resolution"] == "4K"
    assert meta["video_codec"] == "HEVC"
    assert meta["video_tag"] == "4K HEVC"
    assert meta["has_flac"] is True
    assert meta["audio_summary"] == "FLAC 5.1"
    assert meta["sub_summary"] == "ASS (IND)"
    assert meta["fonts_count"] == 1


def test_extract_mkv_metadata_1440p_opus_7_1():
    json_val = {
        "tracks": [
            {
                "type": "video",
                "codec": "VP9",
                "properties": {
                    "pixel_dimensions": "2560x1440"
                }
            },
            {
                "type": "audio",
                "codec": "Opus",
                "properties": {
                    "audio_channels": 8
                }
            }
        ]
    }

    meta = mkv_remux._extract_mkv_metadata(json_val)
    assert meta["resolution"] == "1440p"
    assert meta["video_codec"] == "VP9"
    assert meta["audio_summary"] == "Opus 7.1"
    assert meta["has_flac"] is False


def test_extract_mkv_metadata_empty_and_null_fields():
    """Empty or failed inspection metadata must return None for unknown fields, not fabricated defaults."""
    json_val = None
    meta = mkv_remux._extract_mkv_metadata(json_val)
    assert meta["resolution"] is None
    assert meta["video_codec"] is None
    assert meta["video_tag"] is None
    assert meta["audio_summary"] is None
    assert meta["has_flac"] is None
    assert meta["sub_summary"] is None
    assert meta["fonts_count"] is None


def test_extract_mkv_metadata_subtitle_formats():
    """Subtitles with SRT/PGS/mixed must report correct format summaries."""
    json_srt = {
        "tracks": [
            {"type": "subtitles", "codec": "SubRip/SRT", "properties": {"language": "eng"}},
        ]
    }
    meta_srt = mkv_remux._extract_mkv_metadata(json_srt)
    assert meta_srt["sub_summary"] == "SRT (ENG)"

    json_pgs = {
        "tracks": [
            {"type": "subtitles", "codec": "HDMV PGS", "properties": {"language": "jpn"}},
        ]
    }
    meta_pgs = mkv_remux._extract_mkv_metadata(json_pgs)
    assert meta_pgs["sub_summary"] == "PGS (JPN)"

    json_mixed = {
        "tracks": [
            {"type": "subtitles", "codec": "SubRip/SRT", "properties": {"language": "eng"}},
            {"type": "subtitles", "codec": "HDMV PGS", "properties": {"language": "jpn"}},
        ]
    }
    meta_mixed = mkv_remux._extract_mkv_metadata(json_mixed)
    assert meta_mixed["sub_summary"] == "Subs (ENG, JPN)"

    json_no_lang = {
        "tracks": [
            {"type": "subtitles", "codec": "SubRip/SRT", "properties": {}},
        ]
    }
    meta_no_lang = mkv_remux._extract_mkv_metadata(json_no_lang)
    assert meta_no_lang["sub_summary"] == "SRT (UND)"


def test_extract_mkv_metadata_dimension_string_fallback():
    json_val = {
        "tracks": [
            {
                "type": "video",
                "codec": "VP9",
                "properties": {
                    "pixel_dimensions": "1920 / 1080"
                }
            }
        ]
    }
    meta = mkv_remux._extract_mkv_metadata(json_val)
    assert meta["resolution"] == "1080p"
    assert meta["video_codec"] == "VP9"


def test_extract_mkv_metadata_codec_id_only_flac():
    json_val = {
        "tracks": [
            {
                "type": "audio",
                "codec": "A_UNKNOWN",
                "properties": {
                    "codec_id": "A_FLAC",
                    "audio_channels": 2
                }
            }
        ]
    }
    meta = mkv_remux._extract_mkv_metadata(json_val)
    assert meta["has_flac"] is True
    assert meta["audio_summary"] == "FLAC 2.0"

