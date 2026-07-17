"""Tests for performance optimizations: PersistentInspectCache, io_semaphore, and probesize flags."""

import importlib
import json
import threading
import time
from pathlib import Path
from unittest.mock import patch, MagicMock

import pytest

mkv_common_fast = importlib.import_module("mkv_common_fast")
mkv_common = importlib.import_module("mkv_common")
mkv_remux_fast = importlib.import_module("mkv_remux_fast")
mkv_remux = importlib.import_module("mkv_remux")
mkv_extract_fast = importlib.import_module("mkv_extract_fast")
mkv_extract = importlib.import_module("mkv_extract")


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_common_fast, id="mkv_common_fast"),
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

    def test_inspect_videos_uses_cache(self, tmp_path, mod):
        cache_file = tmp_path / "cache.json"
        cache = mod.PersistentInspectCache(cache_file=cache_file, enabled=True)

        sample_file = tmp_path / "video.mkv"
        sample_file.write_bytes(b"dummy_mkv_data")

        json_data = {"tracks": [{"id": 0, "type": "video"}]}
        cache.put(sample_file, json_data)

        # Mock run_mkvmerge_json to verify it is NOT called for cached video
        with patch.object(mod, "run_mkvmerge_json") as mock_mkvmerge:
            results = mod.inspect_videos([sample_file], pool=MagicMock(), persistent_cache=cache)
            assert results[sample_file] == json_data
            mock_mkvmerge.assert_not_called()


@pytest.mark.parametrize("mod", [
    pytest.param(mkv_remux_fast, id="mkv_remux_fast"),
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


def test_io_semaphore_throttles_remux(tmp_path):
    """Verify io_semaphore is acquired during mkvmerge execution in remux."""
    sem = threading.BoundedSemaphore(1)

    # Acquire semaphore to simulate an active job
    sem.acquire()
    fake_called = []

    def fake_subprocess(cmd, **kwargs):
        print(f"\nDEBUG FAKE SUBPROCESS CALLED: {cmd}", flush=True)
        if len(cmd) > 1 and cmd[0] == "mkvmerge" and cmd[1] == "-o":
            Path(cmd[2]).touch()
        sem_held = not sem.acquire(blocking=False)
        fake_called.append(sem_held)
        res = MagicMock()
        res.returncode = 0
        res.stderr = ""
        return res

    sample_video = tmp_path / "video.mkv"
    sample_video.touch()

    parsed = ("1", "E", "01")
    job = mkv_remux_fast.PlannedJob(
        video=sample_video,
        video_name=sample_video.name,
        base=sample_video.stem,
        output_file=tmp_path / "out.mkv",
        subtitle_file=None,
        subtitle_label="Indonesian",
        subtitle_mkv_lang="ind",
        chapter_file=None,
        cover_file=None,
        embedded_sub_id=0,
    )

    mock_sub_index = MagicMock()
    mock_sub_index.lookup.return_value = (tmp_path / "sub.ass", "ind")
    mock_chap_index = MagicMock()
    mock_chap_index.lookup.return_value = None
    mock_cover_index = MagicMock()
    mock_cover_index.lookup.return_value = None

    with patch.object(mkv_remux_fast, "run_subprocess", side_effect=fake_subprocess), \
         patch.object(mkv_remux_fast, "select_tracks") as mock_tracks:
        tr = MagicMock()
        tr.video_id = 0
        tr.audio_id = None
        tr.audio_codec = "A_AAC"
        tr.source_cover_id = None
        tr.use_source_fonts = False
        tr.source_font_ids = []
        mock_tracks.return_value = tr

        res_holder = []
        errors = []
        def worker():
            try:
                res = mkv_remux_fast.pipeline_worker(
                    video=sample_video,
                    video_parsed=parsed,
                    json_data={"tracks": [{"id": 0, "type": "video"}]},
                    chapter_fallback_count=0,
                    chapter_unmatched_claimed=set(),
                    chapter_unmatched_claimed_lock=threading.Lock(),
                    sub_index=mock_sub_index,
                    chap_index=mock_chap_index,
                    cover_index=mock_cover_index,
                    output_dir=tmp_path,
                    fonts=mkv_remux_fast.FontInfo(args=(), count=0),
                    opus_bitrate="192k",
                    complexity=10,
                    ffmpeg_threads=2,
                    temp_parent_dir=None,
                    dry_run=False,
                    io_semaphore=sem,
                )
                res_holder.append(res)
            except Exception as e:
                import traceback
                errors.append(f"{e}\n{traceback.format_exc()}")

        t = threading.Thread(target=worker)
        t.start()
        time.sleep(0.05)
        sem.release()
        t.join(timeout=2)

        if errors:
            raise errors[0]

        assert res_holder == [True], f"pipeline_worker returned {res_holder}"

        # Confirm pipeline_worker executed run_subprocess under io_semaphore protection
        assert len(fake_called) == 1
        assert fake_called[0] is True
