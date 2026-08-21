"""Failure-path tests for the public extractor."""

import importlib
from pathlib import Path
from types import SimpleNamespace

import pytest

import mkv_extract
from mkv_common import FontManager, ParsedFile


def test_failed_extraction_does_not_replace_existing_subtitle(tmp_path, monkeypatch):
    video = tmp_path / "Show.mkv"
    video.touch()
    parsed_video = ParsedFile(
        path=video,
        name=video.name,
        stem=video.stem,
        parsed=None,
    )
    subtitle = tmp_path / "Show_Show_ind_track2.ass"
    subtitle.write_bytes(b"existing subtitle")
    metadata = {
        "tracks": [
            {
                "id": 2,
                "type": "subtitles",
                "properties": {
                    "codec_id": "S_TEXT/ASS",
                    "language": "ind",
                    "track_name": "Dialogue",
                },
            }
        ]
    }

    def fail_after_partial_write(command, **_kwargs):
        track_target = Path(command[3].split(":", 1)[1])
        track_target.write_bytes(b"partial subtitle")
        return SimpleNamespace(returncode=2, stderr="simulated failure")

    monkeypatch.setattr(mkv_extract, "run_subprocess", fail_after_partial_write)

    with pytest.raises(RuntimeError, match="mkvextract failed"):
        mkv_extract.process_file(
            parsed_video,
            metadata,
            FontManager(tmp_path / "fonts"),
        )

    assert subtitle.read_bytes() == b"existing subtitle"


def test_successful_extraction_promotes_staged_subtitle(tmp_path, monkeypatch):
    video = tmp_path / "Show.mkv"
    video.touch()
    parsed_video = ParsedFile(
        path=video,
        name=video.name,
        stem=video.stem,
        parsed=None,
    )
    subtitle = tmp_path / "Show_Show_ind_track2.ass"
    metadata = {
        "tracks": [
            {
                "id": 2,
                "type": "subtitles",
                "properties": {
                    "codec_id": "S_TEXT/ASS",
                    "language": "ind",
                    "track_name": "Dialogue",
                },
            }
        ]
    }

    def write_complete_output(command, **_kwargs):
        track_target = Path(command[3].split(":", 1)[1])
        track_target.write_bytes(b"complete subtitle")
        return SimpleNamespace(returncode=0, stderr="")

    monkeypatch.setattr(mkv_extract, "run_subprocess", write_complete_output)

    _, _, succeeded = mkv_extract.process_file(
        parsed_video,
        metadata,
        FontManager(tmp_path / "fonts"),
    )

    assert succeeded is True
    assert subtitle.read_bytes() == b"complete subtitle"
    assert not list(tmp_path.glob(".tmp_extract_*"))


def test_stale_subtitle_cleanup(tmp_path):
    """
    _cleanup_stale_subtitle_offsets should remove old offset-suffixed versions.
    """
    base_sub = tmp_path / "episode_01_E01_ind_track2.ass"
    base_sub.touch()
    old_offset = tmp_path / "episode_01_E01_ind_track2.[+2s].ass"
    old_offset.touch()
    new_offset = tmp_path / "episode_01_E01_ind_track2.[+5s].ass"
    new_offset.touch()

    manual_sub = tmp_path / "Demon Lord 2099 - 01.ind.ass"
    manual_sub.touch()

    active_map = {"episode_01_E01_ind_track2": "episode_01_E01_ind_track2.[+5s].ass"}
    mkv_extract.cleanup_stale_subtitle_offsets_bulk(tmp_path, active_map)

    assert not base_sub.exists()
    assert not old_offset.exists()
    assert new_offset.exists()
    assert manual_sub.exists()


@pytest.mark.parametrize("module_name", ["mkv_extract"])
def test_stale_subtitle_cleanup_handles_case_variant_stems(tmp_path, module_name):
    module = importlib.import_module(module_name)
    current = tmp_path / "Episode_01_E01_ind_track2.[+5s].ass"
    current.touch()
    old_offset = tmp_path / "episode_01_e01_ind_track2.[+2s].ass"
    old_offset.touch()
    old_base = tmp_path / "episode_01_e01_ind_track2.ass"
    old_base.touch()

    active_map = {"Episode_01_E01_ind_track2": current.name}
    module.cleanup_stale_subtitle_offsets_bulk(tmp_path, active_map)

    assert current.exists()
    assert not old_offset.exists()
    assert not old_base.exists()


@pytest.mark.parametrize("module_name", ["mkv_extract"])
def test_stale_subtitle_cleanup_preserves_multiple_active_casefold_collisions(tmp_path, module_name):
    module = importlib.import_module(module_name)
    current_a = tmp_path / "Straße_01_E01_ind_track2.[+5s].ass"
    current_b = tmp_path / "Strasse_01_E01_ind_track2.[+7s].ass"
    stale_a = tmp_path / "Straße_01_E01_ind_track2.ass"
    stale_b = tmp_path / "Strasse_01_E01_ind_track2.ass"

    for path in (current_a, current_b, stale_a, stale_b):
        path.touch()

    active_map = {
        "Straße_01_E01_ind_track2": current_a.name,
        "Strasse_01_E01_ind_track2": current_b.name,
    }
    module.cleanup_stale_subtitle_offsets_bulk(tmp_path, active_map)

    assert current_a.exists()
    assert current_b.exists()
    assert not stale_a.exists()
    assert not stale_b.exists()


def test_mkvextract_timeout_raises_runtime_error(tmp_path, monkeypatch):
    import subprocess
    video = tmp_path / "Show.mkv"
    video.touch()
    log = []

    def mock_run_timeout(*_a, **_k):
        raise subprocess.TimeoutExpired(cmd=["mkvextract"], timeout=3600)

    monkeypatch.setattr(mkv_extract, "run_subprocess", mock_run_timeout)

    with pytest.raises(RuntimeError, match="mkvextract timed out after 3600s"):
        mkv_extract._run_mkvextract(video, ["tracks", f"0:{tmp_path / 'sub.ass'}"], log)

    assert any("mkvextract timed out after 3600s" in line for line in log)


@pytest.mark.parametrize("file_name, expected_ext", [
    ("cover.png", ".png"),
    ("cover.jpg", ".jpg"),
    ("cover.JPEG", ".jpeg"),
    ("cover.webp", ".webp"),
    ("cover.gif", ".gif"),
    ("cover.avif", ".avif"),
    ("cover.exe", ".jpg"),
    ("cover.bad<chars>", ".jpg"),
    ("cover", ".jpg"),
    ("poster.unknown", ".jpg"),
])
def test_cover_extension_whitelisting(tmp_path, monkeypatch, file_name, expected_ext):
    video = tmp_path / "Show.mkv"
    video.touch()
    parsed_video = ParsedFile(path=video, name=video.name, stem=video.stem, parsed=None)
    metadata = {
        "attachments": [
            {
                "id": 1,
                "file_name": file_name,
                "content_type": "image/jpeg",
                "size": 100,
            }
        ]
    }

    def mock_run(command, **_kwargs):
        for arg in command:
            s_arg = str(arg)
            if ":" in s_arg:
                parts = s_arg.split(":", 1)
                if len(parts) == 2 and parts[0].isdigit() and Path(parts[1]).parent.name.startswith(".tmp_extract_"):
                    Path(parts[1]).write_bytes(b"image data")
        return SimpleNamespace(returncode=0, stderr="")

    monkeypatch.setattr(mkv_extract, "run_subprocess", mock_run)

    _, _, succeeded = mkv_extract.process_file(
        parsed_video,
        metadata,
        FontManager(tmp_path / "fonts"),
    )
    assert succeeded is True
    expected_file = tmp_path / f"Show_cover{expected_ext}"
    assert expected_file.exists()


def test_font_registry_shared_across_dirs(tmp_path, monkeypatch):
    """When a shared FontManager is passed across multiple directories,
    fonts with the same content hash are deduplicated."""
    fonts_dir = tmp_path / "shared_fonts"
    fonts_dir.mkdir()

    shared_manager = FontManager(fonts_dir)

    dir_a = tmp_path / "dir_a"
    dir_b = tmp_path / "dir_b"
    dir_a.mkdir()
    dir_b.mkdir()

    video_a = dir_a / "Show_A.mkv"
    video_b = dir_b / "Show_B.mkv"
    video_a.touch()
    video_b.touch()

    parsed_a = ParsedFile(path=video_a, name=video_a.name, stem=video_a.stem, parsed=None)
    parsed_b = ParsedFile(path=video_b, name=video_b.name, stem=video_b.stem, parsed=None)

    meta_a = {
        "attachments": [
            {"id": 1, "file_name": "font_a.ttf", "content_type": "application/x-truetype-font", "size": 12}
        ]
    }
    meta_b = {
        "attachments": [
            {"id": 1, "file_name": "font_b.ttf", "content_type": "application/x-truetype-font", "size": 12}
        ]
    }

    def mock_run(command, **_kwargs):
        for arg in command:
            s_arg = str(arg)
            if ":" in s_arg:
                parts = s_arg.split(":", 1)
                if len(parts) == 2 and parts[0].isdigit():
                    Path(parts[1]).write_bytes(b"font data 12")
        return SimpleNamespace(returncode=0, stderr="")

    monkeypatch.setattr(mkv_extract, "run_subprocess", mock_run)

    # Process file A with shared registry
    _, _, ok_a = mkv_extract.process_file(parsed_a, meta_a, shared_manager)
    assert ok_a is True
    assert (fonts_dir / "font_a.ttf").exists()

    # Process file B with same shared registry
    _, _, ok_b = mkv_extract.process_file(parsed_b, meta_b, shared_manager)
    assert ok_b is True
    # font_b should be deduped (not saved as duplicate or with hash suffix)
    assert not (fonts_dir / "font_b.ttf").exists()
    shared_manager.cleanup_staging()
    assert len(list(fonts_dir.iterdir())) == 1
