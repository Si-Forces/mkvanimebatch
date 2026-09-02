"""Tests for performance optimizations: PersistentInspectCache, io_semaphore, and probesize flags."""

import importlib
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import cast
from unittest.mock import MagicMock, patch

import pytest

mkv_remux = importlib.import_module("mkv_remux")
mkv_remux_job = importlib.import_module("mkv_remux_job")
mkv_extract = importlib.import_module("mkv_extract")
mkv_assets = importlib.import_module("mkv_assets")

import mkv_proc
import mkv_scan
from mkv_domain import EpisodeInfo
from mkv_fonts import FontInfo
from mkv_scan import InspectionResult, PersistentInspectCache, _create_parsed_file, inspect_videos


class TestPersistentInspectCache:
    def test_cache_hit_and_miss(self, tmp_path):
        cache_file = tmp_path / "cache.json"
        cache = PersistentInspectCache(cache_file=cache_file, enabled=True)

        sample_file = tmp_path / "video.mkv"
        sample_file.write_bytes(b"dummy_mkv_data")

        json_data = {"tracks": [{"id": 0, "type": "video"}]}
        cache.put(sample_file, json_data)
        cache.save()

        # Cache file should now exist on disk
        assert cache_file.exists()

        # Load into new cache instance
        cache2 = PersistentInspectCache(cache_file=cache_file, enabled=True)
        assert cache2.get(sample_file) == json_data

        # Test cache miss on modified size
        sample_file.write_bytes(b"dummy_mkv_data_modified")
        assert cache2.get(sample_file) is None

        # Test clear()
        cache2.clear()
        assert not cache_file.exists()
        assert cache2.get(sample_file) is None

    def test_cache_lazy_eviction_on_missing_file(self, tmp_path):
        """Verify deleted files are lazily invalidated upon get() rather than blocking eager stat loops during save()."""
        cache_file = tmp_path / "cache.json"
        cache = PersistentInspectCache(cache_file=cache_file, enabled=True)

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

        # Reload cache and verify deleted file is lazily invalidated on get()
        cache2 = PersistentInspectCache(cache_file=cache_file, enabled=True)
        assert cache2.get(file1) == {"id": 1}
        assert cache2.get(file2) is None

    def test_inspect_videos_uses_cache(self, tmp_path):
        cache_file = tmp_path / "cache.json"
        cache = PersistentInspectCache(cache_file=cache_file, enabled=True)

        sample_file = tmp_path / "video.mkv"
        sample_file.write_bytes(b"dummy_mkv_data")

        json_data = {"tracks": [{"id": 0, "type": "video"}]}
        cache.put(sample_file, json_data)

        # Mock run_mkvmerge_json to verify it is NOT called for cached video
        with patch.object(mkv_scan, "run_mkvmerge_json") as mock_mkvmerge:
            with ThreadPoolExecutor(max_workers=1) as pool:
                results = inspect_videos([sample_file], pool=pool, persistent_cache=cache)
            assert results[sample_file] == InspectionResult.success(json_data)
            mock_mkvmerge.assert_not_called()

    def test_inspect_videos_without_cache(self, tmp_path):
        sample_file = tmp_path / "video.mkv"
        sample_file.write_bytes(b"dummy_mkv_data")

        json_data = {"tracks": [{"id": 0, "type": "video"}]}
        with patch.object(mkv_scan, "run_mkvmerge_json", return_value=json_data):
            with ThreadPoolExecutor(max_workers=1) as pool:
                results = inspect_videos([sample_file], pool=pool, persistent_cache=None)
            assert results[sample_file] == InspectionResult.success(json_data)

    def test_failed_inspection_is_explicit_not_cached_and_retries(self, tmp_path):
        sample_file = tmp_path / "video.mkv"
        sample_file.write_bytes(b"dummy")
        cache = PersistentInspectCache(cache_file=tmp_path / "cache.json")
        probe = MagicMock(side_effect=[
            mkv_scan.InspectionError("mkvmerge inspection timed out"),
            {"tracks": []},
        ])

        with patch.object(mkv_scan, "run_mkvmerge_json", probe), \
             patch.object(cache, "put", wraps=cache.put) as cache_put:
            with ThreadPoolExecutor(max_workers=1) as pool:
                first = inspect_videos([sample_file], pool=pool, persistent_cache=cache)
                second = inspect_videos([sample_file], pool=pool, persistent_cache=cache)

        assert first[sample_file] == InspectionResult.failure("mkvmerge inspection timed out")
        assert second[sample_file] == InspectionResult.success({"tracks": []})
        assert probe.call_count == 2
        cache_put.assert_called_once_with(sample_file, {"tracks": []})

    def test_probe_timeout_preserves_reason(self, tmp_path):
        sample_file = tmp_path / "video.mkv"
        with patch.object(
            mkv_scan,
            "run_subprocess_cancellable",
            side_effect=mkv_scan.subprocess.TimeoutExpired(["mkvmerge"], 1),
        ):
            with pytest.raises(mkv_scan.InspectionError, match="inspection timed out"):
                mkv_scan.run_mkvmerge_json(sample_file)

    def test_probe_malformed_json_preserves_reason(self, tmp_path):
        sample_file = tmp_path / "video.mkv"
        completed = mkv_scan.subprocess.CompletedProcess(["mkvmerge"], 0, "not json", "")
        with patch.object(mkv_scan, "run_subprocess_cancellable", return_value=completed):
            with pytest.raises(mkv_scan.InspectionError, match="Failed to parse metadata"):
                mkv_scan.run_mkvmerge_json(sample_file)
    def test_cache_save_exception_unlinks_temp_file(self, tmp_path):
        """Verify that any temporary file created during save() is unlinked if serialization or write fails."""
        cache_file = tmp_path / "cache.json"
        cache = PersistentInspectCache(cache_file=cache_file, enabled=True)
        sample_file = tmp_path / "video.mkv"
        sample_file.write_bytes(b"dummy")

        # Put an object that raises a TypeError during json.dump
        cache.put(sample_file, {"bad_object": object()})
        cache.save()

        # Ensure no leftover temporary files in the directory
        temp_files = list(tmp_path.glob("tmp*"))
        assert len(temp_files) == 0
        assert not cache_file.exists()

    def test_inspect_videos_semaphore_bounds_probe_concurrency(self, tmp_path):
        """probe_semaphore of size N never admits more than N concurrent
        probes, and cache hits never invoke the probe at all."""
        limit = 3
        files = [tmp_path / f"video_{i}.mkv" for i in range(12)]
        for f in files:
            f.write_bytes(b"x")
        hit_files, miss_files = files[:4], files[4:]

        class FakeCache:
            def __init__(self, hits):
                self.hits = hits
                self.puts = []

            def get(self, path):
                return self.hits.get(path)

            def put(self, path, data):
                self.puts.append(path)

        cache = FakeCache({f: {"tracks": [{"id": 0}]} for f in hit_files})

        lock = threading.Lock()
        state = {"current": 0, "peak": 0}
        probed = []

        def fake_probe(video, cancel_token=None):
            with lock:
                state["current"] += 1
                state["peak"] = max(state["peak"], state["current"])
                probed.append(video)
            time.sleep(0.02)
            with lock:
                state["current"] -= 1
            return {"tracks": []}

        semaphore = threading.BoundedSemaphore(limit)
        with patch.object(mkv_scan, "run_mkvmerge_json", side_effect=fake_probe):
            with ThreadPoolExecutor(max_workers=len(files)) as pool:
                results = inspect_videos(
                    files,
                    pool=pool,
                    persistent_cache=cast(PersistentInspectCache, cache),
                    probe_semaphore=semaphore,
                )

        assert state["peak"] <= limit
        assert set(probed) == set(miss_files)  # cache hits never spawn a probe
        assert set(cache.puts) == set(miss_files)  # only fresh probes are cached
        for f in hit_files:
            assert results[f] == InspectionResult.success({"tracks": [{"id": 0}]})
        for f in miss_files:
            assert results[f] == InspectionResult.success({"tracks": []})


def test_cover_index_add_episode_cover_no_nameerror():
    """Verify CoverIndex.add() with per-episode cover does not raise NameError for parse_episode."""
    cover_index = mkv_assets.CoverIndex()
    pf = _create_parsed_file(Path("/dummy/Show - 01_cover.jpg"))
    cover_index.add(pf)
    assert cover_index.lookup(EpisodeInfo("", "E", "01")) == Path("/dummy/Show - 01_cover.jpg")


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_remux_job, id="mkv_remux_job"),
])
def test_ffmpeg_probesize_args(mod):
    """Verify -analyzeduration 0 -probesize 1M flags are included in ffmpeg calls."""
    video = Path("/dummy/video.mkv")
    temp_dir = Path("/dummy/temp")

    mock_res = MagicMock()
    mock_res.returncode = 0
    mock_res.stderr = ""

    with patch.object(mkv_proc, "run_subprocess", return_value=mock_res) as mock_run:
        mod._convert_flac_to_opus(video, 0, temp_dir, "video", "128k", [], 2)

        cmd = mock_run.call_args[0][0]
        assert "-analyzeduration" in cmd
        assert cmd[cmd.index("-analyzeduration") + 1] == "0"
        assert "-probesize" in cmd
        assert cmd[cmd.index("-probesize") + 1] == "1M"


def test_io_semaphore_covers_flac_conversion_and_remux(tmp_path):
    """One heavy-I/O permit covers both conversion and the following mux."""
    mock_sem = MagicMock()
    mock_sem.acquire.return_value = True
    commands = []

    def fake_subprocess(cmd, **kwargs):
        commands.append(cmd[0])
        mock_sem.acquire.assert_called()
        if len(cmd) > 1 and cmd[0] == "mkvmerge" and cmd[1] == "-o":
            from pathlib import Path
            Path(cmd[2]).touch()
        res = MagicMock()
        res.returncode = 0
        res.stderr = ""
        return res

    sample_video = tmp_path / "video.mkv"
    sample_video.touch()

    mock_sub_index = MagicMock()
    mock_sub_index.lookup.return_value = (tmp_path / "sub.ass", "ind")
    mock_chap_index = MagicMock()
    mock_chap_index.lookup.return_value = None
    mock_cover_index = MagicMock()
    mock_cover_index.lookup.return_value = None

    with patch.object(mkv_remux_job, "run_subprocess_cancellable", side_effect=fake_subprocess), \
         patch.object(mkv_remux_job, "select_tracks") as mock_tracks:
        tr = MagicMock()
        tr.video_id = 0
        tr.audio_id = 1
        tr.audio_index = 0
        tr.audio_codec = "A_FLAC"
        tr.audio_reason = "Japanese FLAC"
        tr.audio_channels = 2
        tr.has_source_chapters = False
        tr.source_cover_id = None
        tr.use_source_fonts = False
        tr.source_font_ids = []
        mock_tracks.return_value = tr

        mock_job = MagicMock()
        mock_job.video = sample_video
        mock_job.video_name = "video.mkv"
        mock_job.base = "video"
        mock_job.output_file = tmp_path / "video.mkv"
        mock_job.fonts = FontInfo(args=(), count=0)

        res = mkv_remux_job.process_job(
            job=mock_job,
            opus_bitrate="192k",
            inspection_results={sample_video: InspectionResult.success({"tracks": [{"id": 0, "type": "video"}]})},
            ffmpeg_threads=2,
            io_semaphore=mock_sem,
            complexity=10,
            sub_offset=0.0,
        )

        assert res.skipped_reason is None
        assert commands == ["ffmpeg", "mkvmerge"]
        mock_sem.acquire.assert_called_once()
        mock_sem.release.assert_called_once()
