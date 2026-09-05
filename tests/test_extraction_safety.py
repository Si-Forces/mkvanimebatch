"""Failure-path and global-pipeline tests for the public extractor."""

import threading
from pathlib import Path
from types import SimpleNamespace

import pytest

import mkv_extract
import mkv_proc
from mkv_fonts import FontManager
from mkv_runner import CliRuntime
from mkv_scan import InspectedVideo, ParsedFile, PersistentInspectCache


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
    metadata = InspectedVideo({
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
    })

    def fail_after_partial_write(command, **_kwargs):
        track_target = Path(command[3].split(":", 1)[1])
        track_target.write_bytes(b"partial subtitle")
        return SimpleNamespace(returncode=2, stderr="simulated failure")

    monkeypatch.setattr(mkv_proc, "run_subprocess", fail_after_partial_write)

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
    metadata = InspectedVideo({
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
    })

    def write_complete_output(command, **_kwargs):
        track_target = Path(command[3].split(":", 1)[1])
        track_target.write_bytes(b"complete subtitle")
        return SimpleNamespace(returncode=0, stderr="")

    monkeypatch.setattr(mkv_proc, "run_subprocess", write_complete_output)

    _, _, succeeded = mkv_extract.process_file(
        parsed_video,
        metadata,
        FontManager(tmp_path / "fonts"),
    )

    assert succeeded is True
    assert subtitle.read_bytes() == b"complete subtitle"
    assert not list(tmp_path.glob(".tmp_extract_*"))


def test_mkvextract_timeout_raises_runtime_error(tmp_path, monkeypatch):
    import subprocess
    video = tmp_path / "Show.mkv"
    video.touch()
    log = []

    def mock_run_timeout(*_a, **_k):
        raise subprocess.TimeoutExpired(cmd=["mkvextract"], timeout=3600)

    monkeypatch.setattr(mkv_proc, "run_subprocess", mock_run_timeout)

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
    metadata = InspectedVideo({
        "attachments": [
            {
                "id": 1,
                "file_name": file_name,
                "content_type": "image/jpeg",
                "size": 100,
            }
        ]
    })

    def mock_run(command, **_kwargs):
        for arg in command:
            s_arg = str(arg)
            if ":" in s_arg:
                parts = s_arg.split(":", 1)
                if len(parts) == 2 and parts[0].isdigit() and Path(parts[1]).parent.name.startswith(".tmp_extract_"):
                    Path(parts[1]).write_bytes(b"image data")
        return SimpleNamespace(returncode=0, stderr="")

    monkeypatch.setattr(mkv_proc, "run_subprocess", mock_run)

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

    meta_a = InspectedVideo({
        "attachments": [
            {"id": 1, "file_name": "font_a.ttf", "content_type": "application/x-truetype-font", "size": 12}
        ]
    })
    meta_b = InspectedVideo({
        "attachments": [
            {"id": 1, "file_name": "font_b.ttf", "content_type": "application/x-truetype-font", "size": 12}
        ]
    })

    def mock_run(command, **_kwargs):
        for arg in command:
            s_arg = str(arg)
            if ":" in s_arg:
                parts = s_arg.split(":", 1)
                if len(parts) == 2 and parts[0].isdigit():
                    Path(parts[1]).write_bytes(b"font data 12")
        return SimpleNamespace(returncode=0, stderr="")

    monkeypatch.setattr(mkv_proc, "run_subprocess", mock_run)

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


# ─── GLOBAL PIPELINE ─────────────────────────────────────────────────────────
def _pipeline_runtime(tmp_path, workers=2, dry_run=False):
    return CliRuntime(
        workers=workers,
        dry_run=dry_run,
        io_semaphore=None,
        persistent_cache=PersistentInspectCache(cache_file=tmp_path / "inspect_cache.json"),
    )


def _release(tmp_path, dir_name, file_name):
    directory = tmp_path / dir_name
    directory.mkdir(parents=True, exist_ok=True)
    video = directory / file_name
    video.touch()
    return directory, ParsedFile(path=video, name=video.name, stem=video.stem, parsed=None)


def _fake_inspect_no_assets(monkeypatch):
    inspected = InspectedVideo({"tracks": [], "attachments": []})

    def fake_inspect(videos, pool, **_kwargs):
        return {p: SimpleNamespace(video=inspected) for p in videos}

    monkeypatch.setattr(mkv_extract, "inspect_videos", fake_inspect)


def test_global_queue_reaches_cross_release_concurrency(tmp_path, monkeypatch):
    """Files from different single-file releases must run concurrently: the
    per-release execution barrier is gone."""
    workers = 4
    grouped = {}
    for i in range(8):
        directory, parsed = _release(tmp_path / "in", f"rel{i:02d}", f"Show {i:02d} - 01.mkv")
        grouped[directory] = [parsed]

    _fake_inspect_no_assets(monkeypatch)

    barrier = threading.Barrier(workers)
    lock = threading.Lock()
    state = {"active": 0, "peak": 0}

    def fake_process_file(mkv_file, inspected, font_manager, **_kwargs):
        with lock:
            state["active"] += 1
            state["peak"] = max(state["peak"], state["active"])
        barrier.wait(timeout=10)
        with lock:
            state["active"] -= 1
        return mkv_file.path, [f"Processing: {mkv_file.name}"], True

    monkeypatch.setattr(mkv_extract, "process_file", fake_process_file)

    config = mkv_extract.ExtractRunConfig(
        runtime=_pipeline_runtime(tmp_path, workers=workers),
        root_input_dir=tmp_path / "in",
    )

    assert mkv_extract._run_pipeline(grouped, config) is True
    assert state["peak"] == workers


def _mock_extract_writes_font(monkeypatch, payload=b"font data 12"):
    def mock_run(command, **_kwargs):
        for arg in command:
            s_arg = str(arg)
            if ":" in s_arg:
                parts = s_arg.split(":", 1)
                if len(parts) == 2 and parts[0].isdigit():
                    Path(parts[1]).write_bytes(payload)
        return SimpleNamespace(returncode=0, stderr="")

    monkeypatch.setattr(mkv_extract, "run_subprocess_cancellable", mock_run)


def _fake_inspect_one_font(monkeypatch):
    def fake_inspect(videos, pool, **_kwargs):
        return {
            p: SimpleNamespace(video=InspectedVideo({
                "attachments": [{
                    "id": 1,
                    "file_name": f"font_{p.stem}.ttf",
                    "content_type": "application/x-truetype-font",
                    "size": 12,
                }],
            }))
            for p in videos
        }

    monkeypatch.setattr(mkv_extract, "inspect_videos", fake_inspect)


def _spy_font_managers(monkeypatch):
    created = []
    real_manager = mkv_extract.FontManager

    def spy(fonts_dir, **kwargs):
        manager = real_manager(fonts_dir, **kwargs)
        created.append(manager)
        return manager

    monkeypatch.setattr(mkv_extract, "FontManager", spy)
    return created


def test_shared_fonts_destination_reuses_one_manager_and_dedups(tmp_path, monkeypatch):
    """Two releases whose outputs share a fonts destination share ONE
    FontManager, so identical fonts dedup across releases."""
    input_dir = tmp_path / "in"
    out_dir = tmp_path / "out"
    dir_a, parsed_a = _release(input_dir, "Rel A", "Show A - 01.mkv")
    dir_b, parsed_b = _release(input_dir, "Rel B", "Show B - 01.mkv")

    _fake_inspect_one_font(monkeypatch)
    _mock_extract_writes_font(monkeypatch)
    created = _spy_font_managers(monkeypatch)

    config = mkv_extract.ExtractRunConfig(
        runtime=_pipeline_runtime(tmp_path),
        root_input_dir=input_dir,
        out_override=out_dir,
    )

    assert mkv_extract._run_pipeline({dir_a: [parsed_a], dir_b: [parsed_b]}, config) is True
    assert len(created) == 1
    fonts = [p for p in (out_dir / "fonts").iterdir() if p.is_file()]
    assert len(fonts) == 1  # identical content promoted exactly once
    assert not (out_dir / "fonts" / ".font_staging").exists()


def test_distinct_local_fonts_dirs_stay_isolated(tmp_path, monkeypatch):
    """Without overrides each release keeps its own local fonts directory and
    manager; identical fonts are NOT deduped across releases."""
    input_dir = tmp_path / "in"
    dir_a, parsed_a = _release(input_dir, "Rel A", "Show A - 01.mkv")
    dir_b, parsed_b = _release(input_dir, "Rel B", "Show B - 01.mkv")

    _fake_inspect_one_font(monkeypatch)
    _mock_extract_writes_font(monkeypatch)
    created = _spy_font_managers(monkeypatch)

    config = mkv_extract.ExtractRunConfig(
        runtime=_pipeline_runtime(tmp_path),
        root_input_dir=input_dir,
    )

    assert mkv_extract._run_pipeline({dir_a: [parsed_a], dir_b: [parsed_b]}, config) is True
    assert len(created) == 2
    for directory in (dir_a, dir_b):
        fonts = [p for p in (directory / "fonts").iterdir() if p.is_file()]
        assert len(fonts) == 1


def test_extract_pipeline_flushes_persistent_cache(tmp_path):
    cache_file = tmp_path / "inspect_cache.json"
    cache = PersistentInspectCache(cache_file=cache_file)
    cache._dirty = True  # force the finally-flush to write a file
    runtime = CliRuntime(workers=2, dry_run=True, io_semaphore=None, persistent_cache=cache)

    directory, parsed = _release(tmp_path / "in", "Rel A", "Show A - 01.mkv")
    config = mkv_extract.ExtractRunConfig(runtime=runtime, root_input_dir=tmp_path / "in")

    assert mkv_extract._run_pipeline({directory: [parsed]}, config) is True
    assert cache_file.is_file()


def test_extract_pipeline_visits_releases_in_caller_order(tmp_path, monkeypatch):
    input_dir = tmp_path / "in"
    dir_b, parsed_b = _release(input_dir, "Rel B", "B.mkv")
    dir_a, parsed_a = _release(input_dir, "Rel A", "A.mkv")

    emitted = []
    monkeypatch.setattr(
        mkv_extract, "emit_item_complete",
        lambda item_id, name, outcome, error=None: emitted.append(name),
    )

    config = mkv_extract.ExtractRunConfig(
        runtime=_pipeline_runtime(tmp_path, dry_run=True),
        root_input_dir=input_dir,
    )

    assert mkv_extract._run_pipeline({dir_b: [parsed_b], dir_a: [parsed_a]}, config) is True
    assert emitted == ["B.mkv", "A.mkv"]  # caller-supplied ordering is preserved


def test_extract_pipeline_cancellation_crash_bounded_drain(tmp_path, monkeypatch):
    """Regression test: crash and cancellation stop refill, drain running tasks, and emit all terminal events."""
    import mkv_events
    from mkv_events import set_events_enabled
    from mkv_proc import CancelledError

    set_events_enabled(True)
    input_dir = tmp_path / "in"
    total_files = 8
    grouped = {}
    for i in range(total_files):
        d, p = _release(input_dir, f"Rel_{i:02d}", f"Show_{i:02d}.mkv")
        grouped[d] = [p]

    _fake_inspect_no_assets(monkeypatch)

    emitted_events = []
    monkeypatch.setattr(
        mkv_events, "emit_event",
        lambda ev, **pl: emitted_events.append({"type": ev, **pl}),
    )

    orig_executor = mkv_extract.ThreadPoolExecutor
    submissions = {"count": 0, "max_inflight": 0, "active": 0}
    lock = threading.Lock()

    class InstrumentedExecutor(orig_executor):
        def submit(self, fn, *args, **kwargs):
            with lock:
                submissions["count"] += 1
                submissions["active"] += 1
                if submissions["active"] > submissions["max_inflight"]:
                    submissions["max_inflight"] = submissions["active"]
            fut = super().submit(fn, *args, **kwargs)
            fut.add_done_callback(lambda _: self._on_done())
            return fut

        def _on_done(self):
            with lock:
                submissions["active"] -= 1

    monkeypatch.setattr(mkv_extract, "ThreadPoolExecutor", InstrumentedExecutor)

    runtime = _pipeline_runtime(tmp_path, workers=2)
    config = mkv_extract.ExtractRunConfig(runtime=runtime, root_input_dir=input_dir)
    token = runtime.cancel_token
    running = 0
    drained = 0
    both_running = threading.Barrier(2)
    cleanup_calls = []
    original_cleanup = mkv_extract.FontManager.cleanup_staging

    def cleanup(manager):
        assert running == 0 and drained >= 2
        cleanup_calls.append(manager)
        original_cleanup(manager)

    monkeypatch.setattr(mkv_extract.FontManager, "cleanup_staging", cleanup)

    def fake_process_file(mkv_file, inspected, font_manager, **_kwargs):
        nonlocal running, drained
        with lock:
            running += 1
        try:
            if mkv_file.name == "Show_00.mkv":
                both_running.wait(timeout=5)
                raise RuntimeError("Worker exploded")
            if mkv_file.name == "Show_01.mkv":
                token.request()
                both_running.wait(timeout=5)
                raise CancelledError("Cancellation triggered")
            return mkv_file.path, [f"Processing: {mkv_file.name}"], True
        finally:
            with lock:
                running -= 1
                drained += 1

    monkeypatch.setattr(mkv_extract, "process_file", fake_process_file)

    with pytest.raises(CancelledError):
        mkv_extract._run_pipeline(grouped, config)

    assert submissions["max_inflight"] <= 4  # Window bound never exceeded (2 * workers)
    assert submissions["count"] < total_files  # Refill stopped upon cancellation
    assert running == 0  # All running tasks drained before pipeline exit
    assert drained >= 2
    assert cleanup_calls

    complete_events = [e for e in emitted_events if e["type"] == "item_complete"]
    assert len(complete_events) == total_files
    statuses = {e.get("file"): e.get("status") for e in complete_events}
    assert statuses.get("Show_00.mkv") == "error"
    assert statuses.get("Show_01.mkv") == "cancelled"
    for i in range(2, total_files):
        assert statuses.get(f"Show_{i:02d}.mkv") == "cancelled"
