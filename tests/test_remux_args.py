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
    def test_no_chapters_before_temp_opus_and_subtitle(self, mod):
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

        # Verify --no-chapters precedes temp_opus
        temp_opus_idx = args.index(str(temp_opus))
        assert args[temp_opus_idx - 5:temp_opus_idx] == [
            "--no-chapters", "--language", "0:jpn", "--track-name", "0:Japanese"
        ]

        # Verify --no-chapters precedes subtitle_file
        sub_idx = args.index(str(job.subtitle_file))
        assert args[sub_idx - 7:sub_idx] == [
            "--no-chapters", "--language", "0:eng", "--track-name", "0:English", "--default-track-flag", "0:yes"
        ]

    def test_ffmpeg_flac_to_opus_disables_chapter_mapping(self, mod):
        mock_res = MagicMock()
        mock_res.returncode = 0
        mock_res.stderr = ""

        temp_dir = Path("/dummy/temp")
        video = Path("/dummy/video.mkv")

        with patch.object(mod, "run_subprocess", return_value=mock_res) as mock_run:
            if mod is mkv_remux:
                log = []
                mod._convert_flac_to_opus(video, 1, 0, temp_dir, "video", "128k", log, 2)
            else:
                mod._convert_flac_to_opus(video, 1, 0, temp_dir, "video", "128k", 10, 2)

            cmd = mock_run.call_args[0][0]

        assert "-map_chapters" in cmd
        map_chap_idx = cmd.index("-map_chapters")
        assert cmd[map_chap_idx + 1] == "-1"
