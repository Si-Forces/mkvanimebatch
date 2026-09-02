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
        workers=4, dry_run=True, no_cache=True, io_jobs=0,
    )
    assert runtime.io_semaphore is None
    assert runtime.persistent_cache.enabled is False


def test_build_cli_runtime_io_jobs_bounded():
    runtime = mkv_runner.build_cli_runtime(
        workers=4, dry_run=True, no_cache=True, io_jobs=3,
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
