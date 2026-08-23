"""
Unit tests for mkv_runner.py — shared CLI scaffolding.
"""

import argparse
import importlib
import sys
import threading
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

mkv_runner = importlib.import_module("mkv_runner")

from mkv_scan import PersistentInspectCache
from mkv_runner import CliRuntime


# ─── default_worker_count ─────────────────────────────────────────────────────
def test_default_worker_count_scaled_and_capped(monkeypatch):
    monkeypatch.setattr(mkv_runner.os, "cpu_count", lambda: 64)
    assert mkv_runner.default_worker_count() == mkv_runner.MAX_DEFAULT_WORKERS

    monkeypatch.setattr(mkv_runner.os, "cpu_count", lambda: 2)
    assert mkv_runner.default_worker_count() == 4


# ─── add_shared_args ──────────────────────────────────────────────────────────
def test_add_shared_args_defaults():
    parser = argparse.ArgumentParser()
    mkv_runner.add_shared_args(parser)
    cfg = parser.parse_args(["--input-dir", "/some/dir"])

    assert cfg.input_dir == "/some/dir"
    assert cfg.workers == mkv_runner.default_worker_count()
    assert cfg.no_cache is False
    assert cfg.clear_cache is False
    assert cfg.events is False


# ─── handle_clear_cache_flag ──────────────────────────────────────────────────
def test_handle_clear_cache_flag_exits_zero(monkeypatch):
    cleared = []

    class StubCache:
        def clear(self):
            cleared.append(True)

    monkeypatch.setattr(mkv_runner, "PersistentInspectCache", StubCache)
    with pytest.raises(SystemExit) as excinfo:
        mkv_runner.handle_clear_cache_flag(True)

    assert excinfo.value.code == 0
    assert cleared == [True]


def test_handle_clear_cache_flag_noop_when_unset():
    mkv_runner.handle_clear_cache_flag(False)


# ─── build_cli_runtime ────────────────────────────────────────────────────────
def test_build_cli_runtime_io_jobs_disabled():
    runtime = mkv_runner.build_cli_runtime(
        root_input_dir=Path("."), out_override=None, workers=4,
        sub_offset=0.0, dry_run=True, no_cache=True, io_jobs=0,
    )
    assert runtime.io_semaphore is None
    assert runtime.persistent_cache.enabled is False


def test_build_cli_runtime_io_jobs_bounded():
    runtime = mkv_runner.build_cli_runtime(
        root_input_dir=Path("."), out_override=None, workers=4,
        sub_offset=0.0, dry_run=True, no_cache=True, io_jobs=3,
    )
    assert isinstance(runtime.io_semaphore, threading.BoundedSemaphore)


# ─── collect_release_dirs ─────────────────────────────────────────────────────
def _touch_mkv(directory: Path, name: str) -> None:
    (directory / name).write_bytes(b"dummy_mkv")


def test_collect_release_dirs_groups_and_sorts_naturally(tmp_path):
    _touch_mkv(tmp_path, "Show - S01E10.mkv")
    _touch_mkv(tmp_path, "Show - S01E02.mkv")
    season2 = tmp_path / "Season 2"
    season2.mkdir()
    _touch_mkv(season2, "Show - S02E01.mkv")

    grouped = mkv_runner.collect_release_dirs(tmp_path)
    dirs = list(grouped.keys())

    assert dirs == [tmp_path, season2]
    assert [p.name for p in grouped[tmp_path]] == ["Show - S01E02.mkv", "Show - S01E10.mkv"]
    assert [p.name for p in grouped[season2]] == ["Show - S02E01.mkv"]


def test_collect_release_dirs_exits_when_no_mkvs(tmp_path):
    with pytest.raises(SystemExit) as excinfo:
        mkv_runner.collect_release_dirs(tmp_path)
    assert excinfo.value.code == 1


# ─── run_batch ────────────────────────────────────────────────────────────────
def _runtime_with_dirty_cache(cache_file: Path) -> CliRuntime:
    cache = PersistentInspectCache(cache_file=cache_file)
    cache._dirty = True  # force the post-loop flush to write a file
    return mkv_runner.CliRuntime(
        root_input_dir=Path("."),
        out_override=None,
        workers=2,
        sub_offset=0.0,
        dry_run=True,
        io_semaphore=None,
        persistent_cache=cache,
    )


def test_run_batch_visits_directories_in_order_and_reports_success(tmp_path):
    runtime = _runtime_with_dirty_cache(tmp_path / "cache.json")
    visited = []

    def process_directory(pool, mkv_dir, dir_mkvs):
        visited.append(mkv_dir.name)
        return True

    grouped = {Path("/src/b"): [], Path("/src/a"): []}
    assert mkv_runner.run_batch(runtime, grouped, process_directory) is True
    assert visited == ["b", "a"]  # caller-supplied ordering is preserved


def test_run_batch_returns_false_when_any_directory_fails(tmp_path):
    runtime = _runtime_with_dirty_cache(tmp_path / "cache.json")

    def process_directory(pool, mkv_dir, dir_mkvs):
        return mkv_dir.name != "b"

    grouped = {Path("/src/a"): [], Path("/src/b"): []}
    assert mkv_runner.run_batch(runtime, grouped, process_directory) is False


def test_run_batch_flushes_persistent_cache_after_loop(tmp_path):
    cache_file = tmp_path / "cache.json"
    runtime = _runtime_with_dirty_cache(cache_file)

    mkv_runner.run_batch(runtime, {Path("/src/a"): []}, lambda pool, d, f: True)

    assert cache_file.is_file()


def test_run_batch_uses_worker_pool_size(tmp_path):
    runtime = _runtime_with_dirty_cache(tmp_path / "cache.json")
    observed = {}

    import concurrent.futures

    real_init = concurrent.futures.ThreadPoolExecutor.__init__

    def spy_init(self, *args, **kwargs):
        observed["max_workers"] = kwargs.get("max_workers")
        real_init(self, *args, **kwargs)

    monkey = pytest.MonkeyPatch()
    monkey.setattr(concurrent.futures.ThreadPoolExecutor, "__init__", spy_init)
    try:
        mkv_runner.run_batch(runtime, {Path("/src/a"): []}, lambda pool, d, f: True)
    finally:
        monkey.undo()

    assert observed["max_workers"] == 2
