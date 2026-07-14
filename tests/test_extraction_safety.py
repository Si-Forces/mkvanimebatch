"""Failure-path tests for the public extractor."""

from pathlib import Path
from types import SimpleNamespace

import pytest

import mkv_extract
from mkv_common import FontRegistry, ParsedFile


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
            tmp_path / "fonts",
            FontRegistry(),
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
        tmp_path / "fonts",
        FontRegistry(),
    )

    assert succeeded is True
    assert subtitle.read_bytes() == b"complete subtitle"
    assert not list(tmp_path.glob(".tmp_extract_*"))
