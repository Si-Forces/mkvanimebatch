"""Smoke tests for CLI main() entrypoints to prevent missing import regressions."""

import sys
from unittest.mock import patch

import pytest

import mkv_extract
import mkv_remux
import mkv_scan


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_remux, id="mkv_remux"),
    pytest.param(mkv_extract, id="mkv_extract"),
])
def test_cli_dry_run_smoke(tmp_path, mod):
    """Run main() with --dry-run and --no-cache on a mock folder structure to catch any missing imports or undefined variables."""
    # Create mock MKV file, subtitle file, cover image, and font dir
    sample_mkv = tmp_path / "Show - S01E01.mkv"
    sample_mkv.write_bytes(b"dummy_mkv")

    sample_sub = tmp_path / "Show - S01E01.ass"
    sample_sub.write_text("[Script Info]\nTitle: Test\n[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Test\n", encoding="utf-8")

    fonts_dir = tmp_path / "fonts"
    fonts_dir.mkdir()
    (fonts_dir / "font.ttf").write_bytes(b"dummy_font")

    sample_cover = tmp_path / "Show - 01_cover.jpg"
    sample_cover.write_bytes(b"dummy_cover")

    argv = ["script.py", "--input-dir", str(tmp_path), "--dry-run", "--no-cache"]

    json_data = {
        "tracks": [
            {"id": 0, "type": "video"},
            {"id": 1, "type": "audio", "properties": {"codec_id": "A_AAC", "audio_channels": 2}},
        ],
        "attachments": [],
    }

    with patch.object(sys, "argv", argv), \
         patch.object(mod, "check_dependencies"), \
         patch.object(mkv_scan, "run_mkvmerge_json", return_value=json_data):
        mod.main()
