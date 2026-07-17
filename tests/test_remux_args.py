"""Tests for mkvmerge and ffmpeg argument generation in remux modules."""

import importlib
from pathlib import Path
from unittest.mock import patch, MagicMock

import pytest

mkv_remux = importlib.import_module("mkv_remux")
mkv_remux_fast = importlib.import_module("mkv_remux_fast")


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_remux, id="mkv_remux"),
    pytest.param(mkv_remux_fast, id="mkv_remux_fast"),
])
class TestRemuxArgs:
    def test_no_chapters_before_temp_opus_only(self, mod):
        """--no-chapters must precede the opus file to prevent chapter doubling,
        but must NOT precede the subtitle file (.ass files never carry chapters)."""
        job = mod.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=Path("/dummy/sub.ass"),
            subtitle_label="English",
            subtitle_mkv_lang="eng",
            chapter_file=None,
            cover_file=None,
            embedded_sub_id=None,
            use_source_fonts=False,
        )
        tr = mod.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="flac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        fonts = mod.FontInfo(args=(), count=0)
        temp_opus = Path("/dummy/audio.opus")
        output_path = Path("/dummy/out.mkv")

        args = mod._build_mkvmerge_args(job, tr, temp_opus, fonts, output_path)

        # --no-chapters must immediately precede the opus file's per-file flags
        temp_opus_idx = args.index(str(temp_opus))
        assert args[temp_opus_idx - 5:temp_opus_idx] == [
            "--no-chapters", "--language", "0:jpn", "--track-name", "0:Japanese"
        ]

        # --no-chapters must NOT appear in the per-file flags immediately before subtitle
        sub_idx = args.index(str(job.subtitle_file))
        assert "--no-chapters" not in args[sub_idx - 7:sub_idx]

    def test_no_chapters_absent_when_no_opus(self, mod):
        """When there is no FLAC->Opus conversion, --no-chapters should not
        appear before any secondary input (subtitle, fonts, cover)."""
        job = mod.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=Path("/dummy/sub.ass"),
            subtitle_label="English",
            subtitle_mkv_lang="eng",
            chapter_file=None,
            cover_file=None,
            embedded_sub_id=None,
            use_source_fonts=False,
        )
        tr = mod.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="A_AAC",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        fonts = mod.FontInfo(args=(), count=0)
        output_path = Path("/dummy/out.mkv")

        args = mod._build_mkvmerge_args(job, tr, None, fonts, output_path)

        # No opus temp file → --no-chapters should NOT appear at all
        # (the only --no-chapters allowed is before source.mkv when chapter_file is set)
        sub_idx = args.index(str(job.subtitle_file))
        assert "--no-chapters" not in args[:sub_idx] or (
            job.chapter_file is not None
        ), "--no-chapters must not appear before subtitle when no opus and no external chapter"
