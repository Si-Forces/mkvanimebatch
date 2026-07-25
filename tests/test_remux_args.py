"""Tests for mkvmerge and ffmpeg argument generation in remux modules."""

import importlib
from pathlib import Path

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
        sub_idx = args.index(str(job.subtitle_file))
        assert "--no-chapters" not in args[:sub_idx]

    def test_chapter_matrix_no_external_no_opus(self, mod):
        """No external chapter & no opus conversion -> source chapters preserved from video."""
        job = mod.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=None,
            subtitle_label="English",
            subtitle_mkv_lang="eng",
            chapter_file=None,
            cover_file=None,
        )
        tr = mod.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="A_AAC",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=True, source_cover_id=None
        )
        args = mod._build_mkvmerge_args(job, tr, None, mod.FontInfo(args=(), count=0), Path("/dummy/out.mkv"))

        assert "--no-chapters" not in args
        assert "--chapters" not in args

    def test_chapter_matrix_no_external_with_opus(self, mod):
        """No external chapter & with opus conversion -> source chapters preserved from video, opus chapters suppressed."""
        job = mod.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=None,
            subtitle_label="English",
            subtitle_mkv_lang="eng",
            chapter_file=None,
            cover_file=None,
        )
        tr = mod.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="flac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=True, source_cover_id=None
        )
        temp_opus = Path("/dummy/audio.opus")
        args = mod._build_mkvmerge_args(job, tr, temp_opus, mod.FontInfo(args=(), count=0), Path("/dummy/out.mkv"))

        video_idx = args.index(str(job.video))
        opus_idx = args.index(str(temp_opus))

        # Video input must NOT have --no-chapters (so source chapters are kept)
        assert "--no-chapters" not in args[:video_idx]

        # Opus input MUST have --no-chapters (so opus chapters don't duplicate source chapters)
        assert "--no-chapters" in args[video_idx:opus_idx]
        assert "--chapters" not in args

    def test_chapter_matrix_with_external_no_opus(self, mod):
        """With external chapter & no opus conversion -> source chapters suppressed, external chapters used."""
        ext_chap = Path("/dummy/chapters.xml")
        job = mod.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=None,
            subtitle_label="English",
            subtitle_mkv_lang="eng",
            chapter_file=ext_chap,
            cover_file=None,
        )
        tr = mod.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="A_AAC",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=True, source_cover_id=None
        )
        args = mod._build_mkvmerge_args(job, tr, None, mod.FontInfo(args=(), count=0), Path("/dummy/out.mkv"))

        video_idx = args.index(str(job.video))
        assert "--no-chapters" in args[:video_idx]

        chap_idx = args.index("--chapters")
        assert args[chap_idx + 1] == str(ext_chap)

    def test_chapter_matrix_with_external_with_opus(self, mod):
        """With external chapter & with opus conversion -> source & opus chapters suppressed, external chapters used."""
        ext_chap = Path("/dummy/chapters.xml")
        job = mod.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=None,
            subtitle_label="English",
            subtitle_mkv_lang="eng",
            chapter_file=ext_chap,
            cover_file=None,
        )
        tr = mod.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="flac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=True, source_cover_id=None
        )
        temp_opus = Path("/dummy/audio.opus")
        args = mod._build_mkvmerge_args(job, tr, temp_opus, mod.FontInfo(args=(), count=0), Path("/dummy/out.mkv"))

        video_idx = args.index(str(job.video))
        opus_idx = args.index(str(temp_opus))

        assert "--no-chapters" in args[:video_idx]
        assert "--no-chapters" in args[video_idx:opus_idx]

        chap_idx = args.index("--chapters")
        assert args[chap_idx + 1] == str(ext_chap)

    def test_convert_flac_to_opus_complexity(self, mod, monkeypatch):
        """Verify _convert_flac_to_opus passes compression_level corresponding to complexity."""
        captured_cmd = []
        def mock_run(cmd, *, capture_stdout=True, timeout=None):
            captured_cmd.extend(cmd)
            class DummyResult:
                returncode = 0
                stderr = ""
            return DummyResult()

        monkeypatch.setattr(mod, "run_subprocess", mock_run)
        if mod.__name__ == "mkv_remux":
            res = mod._convert_flac_to_opus(
                Path("/dummy/video.mkv"), 1, 0, Path("/dummy/temp"),
                "video", "192k", [], 2, complexity=8,
            )
        else:
            res = mod._convert_flac_to_opus(
                Path("/dummy/video.mkv"), 1, 0, Path("/dummy/temp"),
                "video", "192k", complexity=8, ffmpeg_threads=2,
            )
        assert res is not None
        assert captured_cmd[captured_cmd.index("-compression_level") + 1] == "8"

    def test_cli_fonts_dir_and_validation(self, mod, monkeypatch, tmp_path):
        """Verify CLI accepts --fonts-dir and validates bitrate/complexity."""
        test_dir = tmp_path / "input"
        test_dir.mkdir()
        fonts_dir = tmp_path / "custom_fonts"
        fonts_dir.mkdir()
        (test_dir / "ep1.mkv").touch()

        monkeypatch.setattr(mod, "check_dependencies", lambda *a: None)
        monkeypatch.setattr(mod, "inspect_videos", lambda *a, **k: {})
        monkeypatch.setattr(mod, "_run", lambda *a, **k: True)

        test_argv = [
            "script", "--input-dir", str(test_dir),
            "--fonts-dir", str(fonts_dir),
            "--complexity", "5", "--bitrate", "128k",
            "--dry-run"
        ]
        monkeypatch.setattr("sys.argv", test_argv)

        # Should complete main without SystemExit / error
        try:
            mod.main()
        except SystemExit as exc:
            assert exc.code == 0

    def test_cli_assets_dir_support(self, mod, monkeypatch, tmp_path):
        """Verify CLI accepts --assets-dir fallback directory."""
        test_dir = tmp_path / "input"
        test_dir.mkdir()
        assets_dir = tmp_path / "extracted_assets"
        assets_dir.mkdir()
        (test_dir / "ep1.mkv").touch()

        monkeypatch.setattr(mod, "check_dependencies", lambda *a: None)
        monkeypatch.setattr(mod, "inspect_videos", lambda *a, **k: {})
        captured_kwargs = {}
        def mock_run(*args, **kwargs):
            captured_kwargs.update(kwargs)
            return True
        monkeypatch.setattr(mod, "_run", mock_run)

        test_argv = [
            "script", "--input-dir", str(test_dir),
            "--assets-dir", str(assets_dir),
            "--dry-run"
        ]
        monkeypatch.setattr("sys.argv", test_argv)

        try:
            mod.main()
        except SystemExit as exc:
            assert exc.code == 0

        assert captured_kwargs.get("assets_dir") == assets_dir.resolve()


