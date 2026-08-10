"""Tests for performance optimizations: PersistentInspectCache, io_semaphore, and probesize flags."""

import importlib
import json
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch, MagicMock

import pytest

mkv_common = importlib.import_module("mkv_common")
mkv_remux = importlib.import_module("mkv_remux")
mkv_extract = importlib.import_module("mkv_extract")


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_common, id="mkv_common"),
])
class TestPersistentInspectCache:
    def test_cache_hit_and_miss(self, tmp_path, mod):
        cache_file = tmp_path / "cache.json"
        cache = mod.PersistentInspectCache(cache_file=cache_file, enabled=True)

        sample_file = tmp_path / "video.mkv"
        sample_file.write_bytes(b"dummy_mkv_data")

        json_data = {"tracks": [{"id": 0, "type": "video"}]}
        cache.put(sample_file, json_data)
        cache.save()

        # Cache file should now exist on disk
        assert cache_file.exists()

        # Load into new cache instance
        cache2 = mod.PersistentInspectCache(cache_file=cache_file, enabled=True)
        assert cache2.get(sample_file) == json_data

        # Test cache miss on modified size
        sample_file.write_bytes(b"dummy_mkv_data_modified")
        assert cache2.get(sample_file) is None

        # Test clear()
        cache2.clear()
        assert not cache_file.exists()
        assert cache2.get(sample_file) is None

    def test_cache_prunes_deleted_files_on_save(self, tmp_path, mod):
        cache_file = tmp_path / "cache.json"
        cache = mod.PersistentInspectCache(cache_file=cache_file, enabled=True)

        file1 = tmp_path / "video1.mkv"
        file2 = tmp_path / "video2.mkv"
        file1.write_bytes(b"data1")
        file2.write_bytes(b"data2")

        cache.put(file1, {"id": 1})
        cache.put(file2, {"id": 2})
        cache.save()

        # Delete file2 from disk and save cache again
        file2.unlink()
        cache.save()

        # Reload cache and verify deleted file was pruned
        cache2 = mod.PersistentInspectCache(cache_file=cache_file, enabled=True)
        assert cache2.get(file1) == {"id": 1}
        assert cache2.get(file2) is None

    def test_inspect_videos_uses_cache(self, tmp_path, mod):
        cache_file = tmp_path / "cache.json"
        cache = mod.PersistentInspectCache(cache_file=cache_file, enabled=True)

        sample_file = tmp_path / "video.mkv"
        sample_file.write_bytes(b"dummy_mkv_data")

        json_data = {"tracks": [{"id": 0, "type": "video"}]}
        cache.put(sample_file, json_data)

        # Mock run_mkvmerge_json to verify it is NOT called for cached video
        with patch.object(mod, "run_mkvmerge_json") as mock_mkvmerge:
            with ThreadPoolExecutor(max_workers=1) as pool:
                results = mod.inspect_videos([sample_file], pool=pool, persistent_cache=cache)
            assert results[sample_file] == json_data
            mock_mkvmerge.assert_not_called()

    def test_inspect_videos_without_cache(self, tmp_path, mod):
        sample_file = tmp_path / "video.mkv"
        sample_file.write_bytes(b"dummy_mkv_data")

        json_data = {"tracks": [{"id": 0, "type": "video"}]}
        with patch.object(mod, "run_mkvmerge_json", return_value=json_data):
            with ThreadPoolExecutor(max_workers=1) as pool:
                results = mod.inspect_videos([sample_file], pool=pool, persistent_cache=None)
            assert results[sample_file] == json_data


def test_cover_index_add_episode_cover_no_nameerror():
    """Verify CoverIndex.add() with per-episode cover does not raise NameError for parse_episode."""
    cover_index = mkv_remux.CoverIndex()
    pf = mkv_common._create_parsed_file(Path("/dummy/Show - 01_cover.jpg"))
    cover_index.add(pf)
    assert cover_index.lookup("E", "01", "") == Path("/dummy/Show - 01_cover.jpg")


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_remux, id="mkv_remux"),
])
def test_ffmpeg_probesize_args(mod):
    """Verify -analyzeduration 0 -probesize 1M flags are included in ffmpeg calls."""
    video = Path("/dummy/video.mkv")
    temp_dir = Path("/dummy/temp")

    mock_res = MagicMock()
    mock_res.returncode = 0
    mock_res.stderr = ""

    with patch.object(mod, "run_subprocess", return_value=mock_res) as mock_run:
        if mod is mkv_remux:
            mod._convert_flac_to_opus(video, 1, 0, temp_dir, "video", "128k", [], 2)
        else:
            mod._convert_flac_to_opus(video, 1, 0, temp_dir, "video", "128k", 10, 2)

        cmd = mock_run.call_args[0][0]
        assert "-analyzeduration" in cmd
        assert cmd[cmd.index("-analyzeduration") + 1] == "0"
        assert "-probesize" in cmd
        assert cmd[cmd.index("-probesize") + 1] == "1M"



