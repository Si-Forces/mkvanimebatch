"""Tests for mkvmerge and ffmpeg argument generation in remux modules."""

import importlib
from pathlib import Path

import pytest

mkv_remux = importlib.import_module("mkv_remux")


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_remux, id="mkv_remux"),
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

    def test_temp_opus_external_sub_sync_arg_ordering(self, mod):
        """When a job has BOTH temp_opus (FLAC conversion) AND external subtitle AND sync offset,
        --sync 0:<ms> must immediately precede the subtitle file, and temp_opus per-file options
        (--no-chapters, --language, --track-name) must precede temp_opus and not be affected."""
        job = mod.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=Path("/dummy/sub.ass"),
            subtitle_label="Indonesian",
            subtitle_mkv_lang="ind",
            chapter_file=None,
            cover_file=None,
            embedded_sub_id=None,
            use_source_fonts=False,
        )
        tr = mod.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="flac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese Audio",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        fonts = mod.FontInfo(args=(), count=0)
        temp_opus = Path("/dummy/audio.opus")
        output_path = Path("/dummy/out.mkv")
        sub_offset = 2.5  # +2500 ms

        args = mod._build_mkvmerge_args(job, tr, temp_opus, fonts, output_path, sub_offset=sub_offset)

        video_idx = args.index(str(job.video))
        opus_idx = args.index(str(temp_opus))
        sub_idx = args.index(str(job.subtitle_file))

        # Video comes before opus, and opus comes before subtitle
        assert video_idx < opus_idx < sub_idx

        # Opus arguments between video and temp_opus
        opus_args = args[video_idx + 1:opus_idx]
        assert opus_args == [
            "--no-chapters",
            "--language", "0:jpn",
            "--track-name", "0:Japanese Audio",
        ]
        assert "--sync" not in opus_args

        # Subtitle arguments between temp_opus and subtitle_file
        sub_args = args[opus_idx + 1:sub_idx]
        assert sub_args == [
            "--sync", "0:2500",
            "--language", "0:ind",
            "--track-name", "0:Indonesian",
            "--default-track-flag", "0:yes",
        ]

    def test_ffmpeg_timeout_handling(self, mod, monkeypatch, tmp_path):
        """Verify _convert_flac_to_opus handles subprocess.TimeoutExpired gracefully."""
        import subprocess

        def mock_run_timeout(*_a, **_k):
            raise subprocess.TimeoutExpired(cmd=["ffmpeg"], timeout=3600)

        monkeypatch.setattr(mod, "run_subprocess", mock_run_timeout)
        log = []
        if mod.__name__ == "mkv_remux":
            res = mod._convert_flac_to_opus(
                Path("/dummy/video.mkv"), 1, 0, tmp_path,
                "video", "192k", log, 2, complexity=8,
            )
        else:
            res = mod._convert_flac_to_opus(
                Path("/dummy/video.mkv"), 1, 0, tmp_path,
                "video", "192k", log=log, complexity=8, ffmpeg_threads=2,
            )
        assert res is None
        assert any("conversion timed out" in line for line in log)

    def test_mkvmerge_timeout_handling(self, mod, monkeypatch, tmp_path):
        """Verify process_job handles mkvmerge subprocess.TimeoutExpired gracefully."""
        import subprocess

        def mock_run_timeout(cmd, **_k):
            if cmd[0] == "mkvmerge":
                raise subprocess.TimeoutExpired(cmd=cmd, timeout=3600)
            return None

        monkeypatch.setattr(mod, "run_subprocess", mock_run_timeout)

        job = mod.PlannedJob(
            video=tmp_path / "video.mkv",
            video_name="video.mkv",
            base="video",
            output_file=tmp_path / "out.mkv",
            subtitle_file=None,
            subtitle_label="English",
            subtitle_mkv_lang="eng",
            chapter_file=None,
            cover_file=None,
        )
        tr = mod.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="A_AAC",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        monkeypatch.setattr(mod, "select_tracks", lambda *a, **k: tr)

        result = mod.process_job(
            job, tmp_path / "temp", mod.FontInfo(args=(), count=0),
            "192k", {job.video: {}}, ffmpeg_threads=2,
        )
        assert result.skipped_reason is not None
        assert "timed out" in result.skipped_reason
        assert any("mkvmerge timed out" in line for line in result.log)
        assert not (tmp_path / "out.mkv").exists()


