"""Tests for mkvmerge and ffmpeg argument generation in remux modules."""

import importlib
from pathlib import Path

import pytest

import mkv_proc
import mkv_remux_job
from mkv_fonts import FontInfo
from mkv_scan import InspectionResult

mkv_remux = importlib.import_module("mkv_remux")


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_remux, id="mkv_remux"),
])
class TestRemuxArgs:
    def test_no_chapters_before_temp_opus_only(self, mod):
        """--no-chapters must precede the opus file to prevent chapter doubling,
        but must NOT precede the subtitle file (.ass files never carry chapters)."""
        job = mkv_remux_job.PlannedJob(
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
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="flac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        fonts = FontInfo(args=(), count=0)
        temp_opus = Path("/dummy/audio.opus")
        output_path = Path("/dummy/out.mkv")

        args = mkv_remux_job._build_mkvmerge_args(job, tr, temp_opus, fonts, output_path)

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
        job = mkv_remux_job.PlannedJob(
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
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="A_AAC",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        fonts = FontInfo(args=(), count=0)
        output_path = Path("/dummy/out.mkv")

        args = mkv_remux_job._build_mkvmerge_args(job, tr, None, fonts, output_path)

        # No opus temp file → --no-chapters should NOT appear at all
        sub_idx = args.index(str(job.subtitle_file))
        assert "--no-chapters" not in args[:sub_idx]

    def test_external_cover_preserves_source_fonts(self, mod):
        job = mkv_remux_job.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=Path("/dummy/sub.ass"),
            subtitle_label="English",
            subtitle_mkv_lang="eng",
            chapter_file=None,
            cover_file=Path("/dummy/cover.jpg"),
            use_source_fonts=True,
        )
        tracks = mkv_remux_job.TrackSelection(
            video_id=0,
            audio_id=1,
            audio_index=0,
            audio_codec="A_AAC",
            audio_channels=2,
            audio_lang="jpn",
            audio_name="Japanese",
            audio_reason="test",
            has_source_chapters=False,
            source_cover_id=9,
            source_font_ids=[3, 5],
        )

        args = mkv_remux_job._build_mkvmerge_args(
            job,
            tracks,
            None,
            FontInfo(args=(), count=0),
            Path("/dummy/out.mkv"),
        )

        video_idx = args.index(str(job.video))
        assert args[args.index("--attachments") + 1] == "3,5"
        assert "--no-attachments" not in args[:video_idx]
        assert "9" not in args[:video_idx]

    def test_chapter_matrix_no_external_no_opus(self, mod):
        """No external chapter & no opus conversion -> source chapters preserved from video."""
        job = mkv_remux_job.PlannedJob(
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
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="A_AAC",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=True, source_cover_id=None
        )
        args = mkv_remux_job._build_mkvmerge_args(job, tr, None, FontInfo(args=(), count=0), Path("/dummy/out.mkv"))

        assert "--no-chapters" not in args
        assert "--chapters" not in args

    def test_chapter_matrix_no_external_with_opus(self, mod):
        """No external chapter & with opus conversion -> source chapters preserved from video, opus chapters suppressed."""
        job = mkv_remux_job.PlannedJob(
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
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="flac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=True, source_cover_id=None
        )
        temp_opus = Path("/dummy/audio.opus")
        args = mkv_remux_job._build_mkvmerge_args(job, tr, temp_opus, FontInfo(args=(), count=0), Path("/dummy/out.mkv"))

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
        job = mkv_remux_job.PlannedJob(
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
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="A_AAC",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=True, source_cover_id=None
        )
        args = mkv_remux_job._build_mkvmerge_args(job, tr, None, FontInfo(args=(), count=0), Path("/dummy/out.mkv"))

        video_idx = args.index(str(job.video))
        assert "--no-chapters" in args[:video_idx]

        chap_idx = args.index("--chapters")
        assert args[chap_idx + 1] == str(ext_chap)

    def test_chapter_matrix_with_external_with_opus(self, mod):
        """With external chapter & with opus conversion -> source & opus chapters suppressed, external chapters used."""
        ext_chap = Path("/dummy/chapters.xml")
        job = mkv_remux_job.PlannedJob(
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
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="flac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=True, source_cover_id=None
        )
        temp_opus = Path("/dummy/audio.opus")
        args = mkv_remux_job._build_mkvmerge_args(job, tr, temp_opus, FontInfo(args=(), count=0), Path("/dummy/out.mkv"))

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

        monkeypatch.setattr(mkv_proc, "run_subprocess", mock_run)
        res = mkv_remux_job._convert_flac_to_opus(
            Path("/dummy/video.mkv"), 0, Path("/dummy/temp"),
            "video", "192k", [], 2, complexity=8,
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
        from types import SimpleNamespace

        test_dir = tmp_path / "input"
        test_dir.mkdir()
        assets_dir = tmp_path / "extracted_assets"
        assets_dir.mkdir()
        (test_dir / "ep1.mkv").touch()

        monkeypatch.setattr(mod, "check_dependencies", lambda *a: None)
        monkeypatch.setattr(mod, "inspect_videos", lambda *a, **k: {})
        captured_kwargs = {}
        def fake_prepare(*args, **kwargs):
            captured_kwargs["assets_dir"] = kwargs.get("assets_dir")
            return SimpleNamespace(fonts_origin="default", fonts_dir=test_dir / "fonts", videos=[])
        monkeypatch.setattr(mod, "prepare_release_directory", fake_prepare)

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
        job = mkv_remux_job.PlannedJob(
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
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="flac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese Audio",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        fonts = FontInfo(args=(), count=0)
        temp_opus = Path("/dummy/audio.opus")
        output_path = Path("/dummy/out.mkv")
        sub_offset = 2.5  # +2500 ms

        args = mkv_remux_job._build_mkvmerge_args(job, tr, temp_opus, fonts, output_path, sub_offset=sub_offset)

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

    def test_sync_offset_for_embedded_subtitle_track(self, mod):
        """When using embedded subtitles with a sync offset, --sync <embedded_sub_id>:<ms>
        must precede the source video file."""
        job = mkv_remux_job.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=None,
            subtitle_label="",
            subtitle_mkv_lang="",
            chapter_file=None,
            cover_file=None,
            embedded_sub_id=3,
            use_source_fonts=False,
        )
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="aac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese Audio",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        fonts = FontInfo(args=(), count=0)
        output_path = Path("/dummy/out.mkv")

        # 1. Non-zero offset (-1.25s -> -1250ms)
        args = mkv_remux_job._build_mkvmerge_args(job, tr, None, fonts, output_path, sub_offset=-1.25)
        video_idx = args.index(str(job.video))
        pre_video_args = args[:video_idx]

        assert "--subtitle-tracks" in pre_video_args
        assert pre_video_args[pre_video_args.index("--subtitle-tracks") + 1] == "3"
        assert "--sync" in pre_video_args
        assert pre_video_args[pre_video_args.index("--sync") + 1] == "3:-1250"

        # 2. Zero offset -> no --sync
        args_zero = mkv_remux_job._build_mkvmerge_args(job, tr, None, fonts, output_path, sub_offset=0.0)
        assert "--sync" not in args_zero

    def test_sync_offset_for_external_subtitles_does_not_affect_video(self, mod):
        """When using external subtitles with a sync offset, --sync 0:<ms> precedes the
        external subtitle file, and NO --sync argument precedes job.video."""
        job = mkv_remux_job.PlannedJob(
            video=Path("/dummy/video.mkv"),
            video_name="video.mkv",
            base="video",
            output_file=Path("/dummy/out.mkv"),
            subtitle_file=Path("/dummy/sub.ass"),
            subtitle_label="English",
            subtitle_mkv_lang="eng",
            chapter_file=None,
            cover_file=None,
            embedded_sub_id=2,  # Present in source but ignored because external sub exists
            use_source_fonts=False,
        )
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="aac",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese Audio",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        fonts = FontInfo(args=(), count=0)
        output_path = Path("/dummy/out.mkv")

        args = mkv_remux_job._build_mkvmerge_args(job, tr, None, fonts, output_path, sub_offset=1.5)
        video_idx = args.index(str(job.video))
        sub_idx = args.index(str(job.subtitle_file))

        pre_video_args = args[:video_idx]
        assert "--no-subtitles" in pre_video_args
        assert "--sync" not in pre_video_args

        sub_args = args[video_idx + 1:sub_idx]
        assert "--sync" in sub_args
        assert sub_args[sub_args.index("--sync") + 1] == "0:1500"

    def test_ffmpeg_timeout_handling(self, mod, monkeypatch, tmp_path):
        """Verify _convert_flac_to_opus handles subprocess.TimeoutExpired gracefully."""
        import subprocess

        def mock_run_timeout(*_a, **_k):
            raise subprocess.TimeoutExpired(cmd=["ffmpeg"], timeout=3600)

        monkeypatch.setattr(mkv_proc, "run_subprocess", mock_run_timeout)
        log = []
        res = mkv_remux_job._convert_flac_to_opus(
            Path("/dummy/video.mkv"), 0, tmp_path,
            "video", "192k", log, 2, complexity=8,
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

        monkeypatch.setattr(mkv_proc, "run_subprocess", mock_run_timeout)

        job = mkv_remux_job.PlannedJob(
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
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="A_AAC",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        monkeypatch.setattr(mkv_remux_job, "select_tracks", lambda *a, **k: tr)

        result = mkv_remux_job.process_job(
            job, "192k", {job.video: InspectionResult.success({})}, ffmpeg_threads=2,
        )
        assert result.failed is True
        assert result.skipped_reason is None
        assert result.error_reason is not None
        assert "timed out" in result.error_reason
        assert any("mkvmerge timed out" in line for line in result.log)
        assert not (tmp_path / "out.mkv").exists()

    def test_mkvmerge_discards_stdout(self, mod, monkeypatch, tmp_path):
        """Verify process_job runs mkvmerge with stdout discarded (stderr stays captured)."""
        import subprocess

        calls = {}

        def mock_run(cmd, **kwargs):
            calls[cmd[0]] = kwargs
            if cmd[0] == "mkvmerge":
                Path(cmd[cmd.index("-o") + 1]).touch()
            res = subprocess.CompletedProcess(cmd, 0, stdout=None, stderr="")
            return res

        monkeypatch.setattr(mkv_remux_job, "run_subprocess_cancellable", mock_run)

        job = mkv_remux_job.PlannedJob(
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
        tr = mkv_remux_job.TrackSelection(
            video_id=0, audio_id=1, audio_index=0, audio_codec="A_FLAC",
            audio_channels=2, audio_lang="jpn", audio_name="Japanese",
            audio_reason="test", has_source_chapters=False, source_cover_id=None
        )
        monkeypatch.setattr(mkv_remux_job, "select_tracks", lambda *a, **k: tr)

        result = mkv_remux_job.process_job(
            job, "192k", {job.video: InspectionResult.success({})}, ffmpeg_threads=2,
        )
        assert result.failed is False
        assert calls["mkvmerge"]["capture_stdout"] is False
        assert calls["ffmpeg"].get("capture_stdout", True) is True

    def test_cli_ffmpeg_threads_support(self, mod, monkeypatch, tmp_path):
        """Verify CLI accepts --ffmpeg-threads and forwards it."""
        test_dir = tmp_path / "input"
        test_dir.mkdir()
        (test_dir / "ep1.mkv").touch()

        monkeypatch.setattr(mod, "check_dependencies", lambda *a: None)
        monkeypatch.setattr(mod, "inspect_videos", lambda *a, **k: {})
        captured = {}
        def mock_execute(jobs, pool, inspection_results, context):
            captured["context"] = context
            return mkv_remux_job.ExecutionSummary()
        monkeypatch.setattr(mod, "execute_jobs", mock_execute)

        test_argv = [
            "script", "--input-dir", str(test_dir),
            "--ffmpeg-threads", "4",
        ]
        monkeypatch.setattr("sys.argv", test_argv)

        try:
            mod.main()
        except SystemExit as exc:
            assert exc.code == 0

        assert captured["context"].ffmpeg_threads == 4

    def test_cli_sub_assignments_loading(self, mod, monkeypatch, tmp_path):
        """--sub-assignments loads the JSON map and resolves relative paths
        against the input root; malformed files abort with exit 1."""
        import json as json_lib

        test_dir = tmp_path / "input"
        test_dir.mkdir()
        (test_dir / "ep1.mkv").touch()
        assign_file = tmp_path / "assign.json"

        captured_kwargs = {}
        real_plan_jobs = mod._plan_jobs
        def spy_plan_jobs(*args, **kwargs):
            captured_kwargs["sub_assignments"] = kwargs.get("sub_assignments")
            return real_plan_jobs(*args, **kwargs)

        monkeypatch.setattr(mod, "check_dependencies", lambda *a: None)
        monkeypatch.setattr(mod, "inspect_videos", lambda *a, **k: {})
        monkeypatch.setattr(mod, "_plan_jobs", spy_plan_jobs)

        # Valid file with a relative path -> resolved against input root
        assign_file.write_text(json_lib.dumps({
            "version": 1,
            "assignments": {"Season 01/ep1.mkv": "Season 01/sub.ass"},
        }), encoding="utf-8")
        monkeypatch.setattr("sys.argv", [
            "script", "--input-dir", str(test_dir), "--dry-run",
            "--sub-assignments", str(assign_file),
        ])
        try:
            mod.main()
        except SystemExit as exc:
            assert exc.code == 0

        passed = captured_kwargs["sub_assignments"]
        assert passed is not None
        assert passed["Season 01/ep1.mkv"] == test_dir / "Season 01" / "sub.ass"

        # Malformed JSON -> exit 1
        assign_file.write_text("{ not json", encoding="utf-8")
        monkeypatch.setattr("sys.argv", [
            "script", "--input-dir", str(test_dir), "--dry-run",
            "--sub-assignments", str(assign_file),
        ])
        try:
            mod.main()
            raised = False
        except SystemExit as exc:
            raised = True
            assert exc.code == 1
        assert raised

    def test_ffmpeg_threads_capping_behavior(self, mod, monkeypatch):
        """Verify default_ffmpeg_threads caps at MAX_FFMPEG_AUDIO_THREADS and scales down."""
        import os

        from mkv_domain import MAX_FFMPEG_AUDIO_THREADS, default_ffmpeg_threads

        # 64-core machine with 1 worker should be capped at MAX_FFMPEG_AUDIO_THREADS (8)
        monkeypatch.setattr(os, "cpu_count", lambda: 64)
        assert default_ffmpeg_threads(1) == MAX_FFMPEG_AUDIO_THREADS
        assert default_ffmpeg_threads(1) == 8

        # 4-core machine with 2 workers should calculate 2
        monkeypatch.setattr(os, "cpu_count", lambda: 4)
        assert default_ffmpeg_threads(2) == 2

        # Never returns less than 1, even with more workers than cores
        assert default_ffmpeg_threads(16) == 1


    def test_cli_bitrate_rejects_zero(self, mod, monkeypatch, tmp_path):
        """--bitrate 0 / 0k / 0M must abort at CLI validation, not fail per-job in ffmpeg."""
        test_dir = tmp_path / "input"
        test_dir.mkdir()
        (test_dir / "ep1.mkv").touch()

        monkeypatch.setattr(mod, "check_dependencies", lambda *a: None)
        monkeypatch.setattr("sys.argv", [
            "script", "--input-dir", str(test_dir), "--dry-run", "--bitrate", "0k",
        ])
        with pytest.raises(SystemExit) as exc:
            mod.main()
        assert exc.value.code == 1


def test_prompt_for_subtitle_eof_treated_as_skip(monkeypatch, tmp_path):
    """stdin closing mid-prompt (EOF) must return None, not crash the worker."""
    import builtins

    from mkv_scan import _create_parsed_file

    def raise_eof(*_a, **_k):
        raise EOFError

    monkeypatch.setattr(builtins, "input", raise_eof)
    video = _create_parsed_file(tmp_path / "Show - 01.mkv")
    assert mkv_remux._prompt_for_subtitle(video, [tmp_path / "sub.ass"]) is None


# ─── global pipeline ──────────────────────────────────────────────────────────
def _pipeline_config(runtime, root_input_dir):
    from mkv_assets import InteractionMode

    return mkv_remux.RemuxRunConfig(
        runtime=runtime,
        root_input_dir=root_input_dir,
        opus_bitrate="192k",
        complexity=5,
        ffmpeg_threads=2,
        interaction_mode=InteractionMode.NON_INTERACTIVE,
    )


def _pipeline_runtime(tmp_path, workers):
    from mkv_runner import CliRuntime
    from mkv_scan import PersistentInspectCache

    return CliRuntime(
        workers=workers,
        dry_run=False,
        io_semaphore=None,
        persistent_cache=PersistentInspectCache(cache_file=tmp_path / "cache.json"),
    )


def _make_release(root, name, ep):
    rel = root / name
    rel.mkdir()
    (rel / f"Show - {ep}.mkv").write_bytes(b"video")
    (rel / f"Show - {ep}.ass").write_bytes(b"sub")
    return rel


def _fake_inspect(videos, pool, **_kwargs):
    return {v: InspectionResult.success({"tracks": [{"id": 0, "type": "video"}]}) for v in videos}


def test_pipeline_runs_jobs_from_different_releases_concurrently(tmp_path, monkeypatch):
    """8 single-file releases with 4 workers must overlap execution across
    releases (no per-release barrier) while honouring the worker bound."""
    import threading
    import time

    from mkv_runner import collect_release_dirs

    for i in range(8):
        _make_release(tmp_path, f"Release {i + 1}", f"{i + 1:02d}")

    monkeypatch.setattr(mkv_remux, "inspect_videos", _fake_inspect)

    lock = threading.Lock()
    state = {"active": 0, "peak": 0}

    def fake_process_job(job, *args, **kwargs):
        with lock:
            state["active"] += 1
            state["peak"] = max(state["peak"], state["active"])
        time.sleep(0.05)
        with lock:
            state["active"] -= 1
        return mkv_remux_job.JobResult()

    monkeypatch.setattr(mkv_remux_job, "process_job", fake_process_job)

    runtime = _pipeline_runtime(tmp_path, workers=4)
    mkvs_by_dir = collect_release_dirs(tmp_path)
    assert len(mkvs_by_dir) == 8

    assert mkv_remux._run_pipeline(mkvs_by_dir, _pipeline_config(runtime, tmp_path)) is True
    assert state["peak"] > 1
    assert state["peak"] <= 4


def test_pipeline_shares_font_scan_but_refreshes_next_run(tmp_path, monkeypatch):
    import os
    from dataclasses import replace

    from mkv_runner import collect_release_dirs

    for name in ("First", "Second"):
        _make_release(tmp_path, name, "01")
    fonts = tmp_path / "shared-fonts"
    fonts.mkdir()
    (fonts / "font.ttf").touch()
    releases = collect_release_dirs(tmp_path)
    real_scandir = os.scandir
    scans = []

    def scandir(path):
        if Path(path) == fonts:
            scans.append(path)
        return real_scandir(path)

    monkeypatch.setattr(os, "scandir", scandir)
    monkeypatch.setattr(mkv_remux, "inspect_videos", _fake_inspect)
    jobs = []

    def execute(batch, *_args):
        jobs.extend(batch)
        return mkv_remux_job.ExecutionSummary()

    monkeypatch.setattr(mkv_remux, "execute_jobs", execute)
    config = replace(_pipeline_config(_pipeline_runtime(tmp_path, 2), tmp_path),
                     fonts_override=fonts, keep_fonts=True)
    assert mkv_remux._run_pipeline(releases, config)
    assert len(scans) == 1
    assert [job.fonts.count for job in jobs] == [1, 1]
    assert jobs[0].fonts is jobs[1].fonts

    (fonts / "another.otf").touch()
    jobs.clear()
    assert mkv_remux._run_pipeline(releases, config)
    assert len(scans) == 2
    assert [job.fonts.count for job in jobs] == [2, 2]


def test_fonts_cleanup_deferred_and_skipped_for_failed_release(tmp_path, monkeypatch):
    """Fonts directories go only after the whole queue drains, and only for
    releases whose jobs all succeeded; a failed release keeps its fonts dir."""
    from mkv_runner import collect_release_dirs

    rel_ok = _make_release(tmp_path, "Good", "01")
    rel_bad = _make_release(tmp_path, "Bad", "02")
    for rel in (rel_ok, rel_bad):
        fonts = rel / "fonts"
        fonts.mkdir()
        (fonts / "font.ttf").write_bytes(b"f")

    monkeypatch.setattr(mkv_remux, "inspect_videos", _fake_inspect)

    fonts_seen_during_jobs = []

    def fake_process_job(job, *args, **kwargs):
        # Deferral: while ANY job runs, NO fonts dir may have been removed yet.
        fonts_seen_during_jobs.append(
            ((rel_ok / "fonts").is_dir(), (rel_bad / "fonts").is_dir())
        )
        result = mkv_remux_job.JobResult()
        if job.video.parent == rel_bad:
            result.failed = True
            result.error_reason = "boom"
        return result

    monkeypatch.setattr(mkv_remux_job, "process_job", fake_process_job)

    runtime = _pipeline_runtime(tmp_path, workers=2)
    mkvs_by_dir = collect_release_dirs(tmp_path)

    assert mkv_remux._run_pipeline(mkvs_by_dir, _pipeline_config(runtime, tmp_path)) is False
    assert fonts_seen_during_jobs and all(ok and bad for ok, bad in fonts_seen_during_jobs)
    assert not (rel_ok / "fonts").exists()   # fully successful release cleaned
    assert (rel_bad / "fonts").exists()      # failed release keeps its fonts


def test_shared_fallback_fonts_dir_deleted_once_when_all_sharers_succeed(tmp_path, monkeypatch):
    """A release's local 'fonts' dir shared via fallback by a nested release is
    deleted after the whole batch when every sharing release succeeded; fallback
    origins on the sharers must not block cleanup."""
    from mkv_runner import collect_release_dirs

    (tmp_path / "Show - 01.mkv").write_bytes(b"video")
    fonts = tmp_path / "fonts"
    fonts.mkdir()
    (fonts / "font.ttf").write_bytes(b"f")
    nested = _make_release(tmp_path, "Season 2", "02")  # no fonts dir: falls back to root's

    monkeypatch.setattr(mkv_remux, "inspect_videos", _fake_inspect)
    monkeypatch.setattr(mkv_remux_job, "process_job", lambda job, *a, **k: mkv_remux_job.JobResult())

    runtime = _pipeline_runtime(tmp_path, workers=2)
    mkvs_by_dir = collect_release_dirs(tmp_path)
    assert len(mkvs_by_dir) == 2

    assert mkv_remux._run_pipeline(mkvs_by_dir, _pipeline_config(runtime, tmp_path)) is True
    assert not fonts.exists()  # deleted once, fallback sharers don't block it
    assert nested.is_dir()
