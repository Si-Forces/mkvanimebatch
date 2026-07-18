"""Tests for multi-threaded concurrency safety in PersistentInspectCache."""

import importlib
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

import pytest

mkv_common = importlib.import_module("mkv_common")
mkv_common_fast = importlib.import_module("mkv_common_fast")


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_common, id="mkv_common"),
    pytest.param(mkv_common_fast, id="mkv_common_fast"),
])
def test_concurrent_put_and_save_safety(tmp_path, mod):
    """Ensure calling .put() repeatedly across 10 threads while calling .save()
    does not cause RuntimeError: dictionary changed size during iteration.
    """
    cache_file = tmp_path / "inspect_cache.json"
    cache = mod.PersistentInspectCache(cache_file=cache_file, enabled=True)

    dummy_files = []
    for i in range(50):
        p = tmp_path / f"video_{i}.mkv"
        p.touch()
        dummy_files.append(p)

    def worker_put(file_path):
        for idx in range(100):
            cache.put(file_path, {"tracks": [{"id": idx}]})

    def worker_save():
        for _ in range(20):
            cache.save()

    with ThreadPoolExecutor(max_workers=12) as pool:
        futures = [pool.submit(worker_put, f) for f in dummy_files]
        futures.append(pool.submit(worker_save))
        for future in as_completed(futures):
            future.result()

    assert cache_file.exists()
