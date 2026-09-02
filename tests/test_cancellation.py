"""
Unit tests for cooperative cancellation: CancelToken, the cancellable
subprocess runner, worker entry checks, and batch-level stop scheduling.
"""

import importlib
import signal
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import MagicMock

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

import json

mkv_runner = importlib.import_module("mkv_runner")
mkv_scan = importlib.import_module("mkv_scan")
mkv_remux = importlib.import_module("mkv_remux")
mkv_remux_job = importlib.import_module("mkv_remux_job")
mkv_extract = importlib.import_module("mkv_extract")

import mkv_proc
from mkv_domain import EpisodeInfo
from mkv_fonts import FontInfo, FontManager
from mkv_proc import CancelledError, CancelToken, run_subprocess_cancellable
from mkv_runner import CliRuntime
from mkv_scan import InspectedVideo, InspectionResult, ParsedFile, PersistentInspectCache


def _cancelled_token() -> CancelToken:
    token = CancelToken()
    token.request()
    return token


# ─── CancelToken ──────────────────────────────────────────────────────────────
def test_cancel_token_starts_uncancelled():
    token = CancelToken()
    assert token.cancelled is False
    token.check()


def test_cancel_token_request_flips_state_and_check_raises():
    token = CancelToken()
    token.request()
    assert token.cancelled is True
    with pytest.raises(CancelledError):
        token.check()


def test_cancelled_error_is_base_exception_not_exception():
    """Broad `except Exception` handlers must not swallow user cancellation."""
    assert issubclass(CancelledError, BaseException)
    assert not issubclass(CancelledError, Exception)


# ─── run_subprocess_cancellable ───────────────────────────────────────────────
def test_cancellable_subprocess_completes_normally():
    res = run_subprocess_cancellable(
        [sys.executable, "-c", "print('hello')"],
        cancel_token=CancelToken(),
        timeout=60,
    )
    assert res.returncode == 0
    assert "hello" in res.stdout


def test_cancellable_subprocess_terminates_on_cancel():
    token = CancelToken()

    def cancel_soon():
        time.sleep(0.5)
        token.request()

    canceller = threading.Thread(target=cancel_soon)
    canceller.start()
    start = time.monotonic()
    with pytest.raises(CancelledError):
        run_subprocess_cancellable(
            [sys.executable, "-c", "import time; time.sleep(120)"],
            cancel_token=token,
            timeout=300,
            poll_interval=0.1,
        )
    elapsed = time.monotonic() - start
    canceller.join()
    assert elapsed < 30


def test_cancellable_subprocess_timeout_raises_timeoutexpired(tmp_path):
    with pytest.raises(subprocess.TimeoutExpired):
        run_subprocess_cancellable(
            [sys.executable, "-c", "import time; time.sleep(120)"],
            cancel_token=CancelToken(),
            timeout=1,
            poll_interval=0.1,
        )


def test_no_token_delegates_to_plain_run_subprocess(monkeypatch):
    calls = []

    def fake_run(cmd, *, capture_stdout=True, timeout=None):
        calls.append((cmd, capture_stdout, timeout))
        return "sentinel"

    monkeypatch.setattr(mkv_proc, "run_subprocess", fake_run)
    out = run_subprocess_cancellable(["tool"], timeout=7)
    assert out == "sentinel"
    assert calls == [(["tool"], True, 7)]


def test_cancelled_token_prevents_subprocess_spawn(monkeypatch):
    token = _cancelled_token()
    popen = MagicMock()
    monkeypatch.setattr(mkv_proc.subprocess, "Popen", popen)

    with pytest.raises(CancelledError):
        run_subprocess_cancellable(["tool"], cancel_token=token)

    popen.assert_not_called()


# ─── worker entry checks ──────────────────────────────────────────────────────


def test_process_job_reports_cancelled_without_start_event(tmp_path, monkeypatch):
    video = tmp_path / "Show - S01E01.mkv"
    video.write_bytes(b"dummy")
    parsed = ParsedFile(
        path=video, name=video.name, stem=video.stem,
        parsed=EpisodeInfo("1", "E", "01"),
    )
    job = mkv_remux_job.PlannedJob(
        video=parsed.path,
        video_name=parsed.name,
        base=parsed.stem,
        output_file=tmp_path / "out" / "out.mkv",
        subtitle_file=None,
        subtitle_label="",
        subtitle_mkv_lang="und",
        chapter_file=None,
        cover_file=None,
        item_id=parsed.name,
    )

    start = MagicMock()
    complete = MagicMock()
    monkeypatch.setattr(mkv_remux_job, "emit_item_start", start)
    monkeypatch.setattr(mkv_remux_job, "emit_item_complete", complete)

    result = mkv_remux_job.process_job(job=job,
    temp_dir=None,
    fonts=FontInfo(args=(), count=0),
    opus_bitrate="192k", inspection_results={}, cancel_token=_cancelled_token(),)

    start.assert_not_called()
    complete.assert_called_once_with(job.item_id, job.video_name, "cancelled")
    assert result.cancelled is True
    assert result.failed is False
    assert any("CANCELLED" in line for line in result.log)


def test_process_file_reports_cancelled_without_start_event(tmp_path, monkeypatch):
    video = tmp_path / "Show - S01E01.mkv"
    video.write_bytes(b"dummy")
    parsed = ParsedFile(
        path=video, name=video.name, stem=video.stem,
        parsed=EpisodeInfo("1", "E", "01"),
    )
    font_manager = FontManager(tmp_path / "fonts")

    start = MagicMock()
    complete = MagicMock()
    monkeypatch.setattr(mkv_extract, "emit_item_start", start)
    monkeypatch.setattr(mkv_extract, "emit_item_complete", complete)

    _, log, ok = mkv_extract.process_file(
        parsed,
        None,
        font_manager,
        cancel_token=_cancelled_token(),
    )

    start.assert_not_called()
    complete.assert_called_once_with(parsed.name, parsed.name, "cancelled")
    assert ok is False
    assert any("CANCELLED" in line for line in log)


# ─── batch scheduling ─────────────────────────────────────────────────────────
def _runtime(tmp_path: Path) -> CliRuntime:
    return mkv_runner.CliRuntime(
        workers=2,
        dry_run=True,
        io_semaphore=None,
        persistent_cache=PersistentInspectCache(cache_file=tmp_path / "cache.json"),
    )


def test_run_batch_raises_before_remaining_directories_once_cancelled(tmp_path):
    runtime = _runtime(tmp_path)
    visited = []

    def process_directory(pool, mkv_dir, dir_mkvs):
        visited.append(mkv_dir.name)
        runtime.cancel_token.request()
        return True

    grouped = {Path("/src/a"): [], Path("/src/b"): [], Path("/src/c"): []}
    with pytest.raises(CancelledError):
        mkv_runner.run_batch(runtime, grouped, process_directory)

    assert visited == ["a"]


def test_run_batch_unaffected_without_cancellation(tmp_path):
    runtime = _runtime(tmp_path)
    visited = []

    def process_directory(pool, mkv_dir, dir_mkvs):
        visited.append(mkv_dir.name)
        return True

    grouped = {Path("/src/a"): [], Path("/src/b"): []}
    overall = mkv_runner.run_batch(runtime, grouped, process_directory)

    assert overall is True
    assert visited == ["a", "b"]


# ─── token pass-through to the subprocess layer ──────────────────────────────
def test_process_job_forwards_token_into_ffmpeg_conversion(monkeypatch, tmp_path):
    """A FLAC job must hand the cancel token to the cancellable runner."""
    video = tmp_path / "Show - S01E01.mkv"
    video.write_bytes(b"dummy")
    parsed = ParsedFile(
        path=video, name=video.name, stem=video.stem,
        parsed=EpisodeInfo("1", "E", "01"),
    )
    json_data = InspectedVideo({
        "tracks": [
            {"id": 0, "type": "video"},
            {
                "id": 1,
                "type": "audio",
                "properties": {"codec_id": "A_FLAC", "audio_channels": 2, "language": "jpn"},
            },
        ],
        "attachments": [],
    })
    job = mkv_remux_job.PlannedJob(
        video=parsed.path,
        video_name=parsed.name,
        base=parsed.stem,
        output_file=tmp_path / "out" / "out.mkv",
        subtitle_file=None,
        subtitle_label="",
        subtitle_mkv_lang="und",
        chapter_file=None,
        cover_file=None,
        item_id=parsed.name,
    )

    seen: dict = {}

    def fake_run(cmd, *, cancel_token=None, timeout=None, capture_stdout=True):
        seen["cmd"] = cmd[0] if cmd else None
        seen["cancel_token"] = cancel_token
        raise CancelledError("terminated")

    monkeypatch.setattr(mkv_remux_job, "run_subprocess_cancellable", fake_run)
    token = CancelToken()

    with pytest.raises(CancelledError):
        mkv_remux_job.process_job(job=job,
        temp_dir=tmp_path,
        fonts=FontInfo(args=(), count=0),
        opus_bitrate="192k", inspection_results={parsed.path: InspectionResult(video=json_data)}, cancel_token=token,)

    assert seen["cmd"] == "ffmpeg"
    assert seen["cancel_token"] is token


def test_cleanup_error_does_not_mask_cancellation(monkeypatch, tmp_path):
    video = tmp_path / "Show - S01E01.mkv"
    video.write_bytes(b"dummy")
    output = tmp_path / "out.mkv"
    temp_output = output.with_suffix(".tmp")
    job = mkv_remux_job.PlannedJob(
        video=video,
        video_name=video.name,
        base=video.stem,
        output_file=output,
        subtitle_file=None,
        subtitle_label="",
        subtitle_mkv_lang="und",
        chapter_file=None,
        cover_file=None,
        item_id=video.name,
    )
    selection = mkv_remux_job.TrackSelection(
        video_id=0,
        audio_id=1,
        audio_index=0,
        audio_codec="A_AAC",
        audio_channels=2,
        audio_lang="jpn",
        audio_name="Japanese",
        audio_reason="test",
        has_source_chapters=False,
        source_cover_id=None,
    )
    real_unlink = Path.unlink

    def cancel_with_partial_output(*_args, **_kwargs):
        temp_output.write_bytes(b"partial")
        raise CancelledError("terminated")

    def fail_existing_temp_cleanup(path, *, missing_ok=False):
        if path == temp_output and path.exists():
            raise PermissionError("file still in use")
        return real_unlink(path, missing_ok=missing_ok)

    monkeypatch.setattr(mkv_remux_job, "select_tracks", lambda *_args: selection)
    monkeypatch.setattr(mkv_remux_job, "run_subprocess_cancellable", cancel_with_partial_output)
    monkeypatch.setattr(Path, "unlink", fail_existing_temp_cleanup)

    with pytest.raises(CancelledError, match="terminated"):
        mkv_remux_job.process_job(
            job=job,
            temp_dir=None,
            fonts=FontInfo(args=(), count=0),
            opus_bitrate="192k",
            inspection_results={video: InspectionResult.success({})},
            cancel_token=CancelToken(),
        )

    assert temp_output.read_bytes() == b"partial"


def test_process_file_forwards_token_into_mkvextract(monkeypatch, tmp_path):
    """An extraction job must hand the cancel token to _run_mkvextract."""
    video = tmp_path / "Show - S01E01.mkv"
    video.write_bytes(b"dummy")
    parsed = ParsedFile(
        path=video, name=video.name, stem=video.stem,
        parsed=EpisodeInfo("1", "E", "01"),
    )
    json_data = InspectedVideo({
        "tracks": [
            {
                "id": 1,
                "type": "subtitles",
                "properties": {"codec_id": "s_text/ass", "language": "ind"},
            }
        ],
        "attachments": [],
    })
    font_manager = FontManager(tmp_path / "fonts")

    seen: dict = {}

    def fake_extract(mkv_path, args, log, io_semaphore=None, cancel_token=None):
        seen["cancel_token"] = cancel_token
        raise CancelledError("terminated")

    monkeypatch.setattr(mkv_extract, "_run_mkvextract", fake_extract)
    token = CancelToken()

    with pytest.raises(CancelledError):
        mkv_extract.process_file(
            parsed,
            json_data,
            font_manager,
            output_dir=tmp_path,
            root_input_dir=tmp_path,
            cancel_token=token,
        )

    assert seen["cancel_token"] is token


# ─── cache save concurrency ───────────────────────────────────────────────────
def test_save_preserves_entries_put_during_write(tmp_path, monkeypatch):
    cache = PersistentInspectCache(cache_file=tmp_path / "cache.json")
    first = tmp_path / "a.mkv"
    first.write_bytes(b"x")
    cache.put(first, {"tracks": []})

    second = tmp_path / "b.mkv"
    real_dump = json.dump
    interleaved = {"done": False}

    def dump_with_interleaved_put(obj, fh):
        # Interleave exactly once: rewriting the file on every dump call
        # would change its mtime after save() snapshots live_data, and the
        # cache's staleness guard would then (correctly) reject the entry
        # on reload — platform-timing flake, not the behavior under test.
        if not interleaved["done"]:
            second.write_bytes(b"y")
            cache.put(second, {"tracks": [1]})
            interleaved["done"] = True
        real_dump(obj, fh)

    monkeypatch.setattr(json, "dump", dump_with_interleaved_put)
    cache.save()

    # The entry added during the write survives in memory...
    assert cache.get(second) == {"tracks": [1]}
    # ...and reaches disk on the next flush (dirty flag stayed set).
    cache.save()
    reloaded = PersistentInspectCache(cache_file=tmp_path / "cache.json")
    assert reloaded.get(first) == {"tracks": []}
    assert reloaded.get(second) == {"tracks": [1]}


# ─── signal wiring ────────────────────────────────────────────────────────────
def test_install_cancel_handlers_routes_signals_into_token(monkeypatch):
    token = CancelToken()
    captured = {}

    def fake_signal(sig, handler):
        captured[sig] = handler

    monkeypatch.setattr(mkv_runner.signal, "signal", fake_signal)
    mkv_runner.install_cancel_handlers(token)

    assert signal.SIGINT in captured
    assert signal.SIGTERM in captured

    captured[signal.SIGINT](signal.SIGINT, None)
    assert token.cancelled is True

def test_semaphore_wait_is_cancellation_aware():
    token = CancelToken()
    sem = threading.Semaphore(0)  # zero permits, will block

    def canceller():
        time.sleep(0.1)
        token.request()

    t = threading.Thread(target=canceller)
    t.start()

    with pytest.raises(CancelledError):
        with mkv_proc.acquire_semaphore(sem, cancel_token=token):
            pass

    t.join()


def test_inspect_videos_does_not_launch_children_if_cancelled(monkeypatch):
    token = CancelToken()
    token.request()  # pre-cancel
    pool = ThreadPoolExecutor(max_workers=1)

    videos = [Path("a.mkv")]
    # Should not even call run_mkvmerge_json
    mock_run = MagicMock()
    monkeypatch.setattr(mkv_scan, "run_mkvmerge_json", mock_run)

    with pytest.raises(CancelledError):
        mkv_scan.inspect_videos(videos, pool, cancel_token=token)

    mock_run.assert_not_called()
    pool.shutdown(wait=True)

def test_run_batch_saves_persistent_cache_on_cancellation():
    """run_batch must flush persistent_cache.save() in finally even when cancelled."""
    token = CancelToken()
    token.request()  # pre-cancel

    mock_cache = MagicMock()
    runtime = CliRuntime(
        workers=1,
        dry_run=False,
        io_semaphore=None,
        persistent_cache=mock_cache,
        cancel_token=token,
    )

    mkvs_by_dir = {
        Path("/fake/dir"): [ParsedFile(path=Path("/fake/dir/01.mkv"), name="01.mkv", stem="01", parsed=None)]
    }

    with pytest.raises(CancelledError):
        mkv_runner.run_batch(runtime, mkvs_by_dir, lambda *args: True)

    mock_cache.save.assert_called_once_with()


def test_plan_mode_cancellation_exits_130(tmp_path, monkeypatch):
    monkeypatch.setattr(
        sys,
        "argv",
        ["mkv_remux.py", "--plan", "--input-dir", str(tmp_path)],
    )

    def mock_manifest(*args, **kwargs):
        raise CancelledError("User cancelled")

    monkeypatch.setattr(mkv_remux, "generate_inspection_manifest", mock_manifest)

    with pytest.raises(SystemExit) as exc_info:
        mkv_remux.main()

    assert exc_info.value.code == 130

