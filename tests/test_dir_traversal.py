"""
Tests for directory scanners (scan_mkv_files and scan_assets).
Ensures robustness against individual files throwing OSError and prevents
recursive symlink traversal.
"""
import os
import sys
from pathlib import Path
from unittest.mock import MagicMock, patch

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

import mkv_runner
from mkv_scan import PersistentInspectCache, scan_assets, scan_mkv_files


def test_scanner_handles_oserror_on_individual_entries():
    """
    If os.scandir contains 3 entries, and the second one raises OSError on checking properties
    (like is_file() or is_dir()), the scanner must catch the error and process the other entries
    instead of skipping the rest of the folder.
    """
    mock_dir = Path("D:/My Project/fake_dir")

    # Mock entries
    entry1 = MagicMock()
    entry1.name = "episode_01.mkv"
    entry1.path = "D:/My Project/fake_dir/episode_01.mkv"
    entry1.is_file.return_value = True
    entry1.is_dir.return_value = False

    entry2 = MagicMock()
    entry2.name = "broken_permissions.mkv"
    entry2.path = "D:/My Project/fake_dir/broken_permissions.mkv"
    # raises OSError
    entry2.is_file.side_effect = OSError("Permission denied")

    entry3 = MagicMock()
    entry3.name = "episode_02.mkv"
    entry3.path = "D:/My Project/fake_dir/episode_02.mkv"
    entry3.is_file.return_value = True
    entry3.is_dir.return_value = False

    mock_iterator = MagicMock()
    mock_iterator.__enter__.return_value = [entry1, entry2, entry3]

    with patch("os.scandir", return_value=mock_iterator):
        results = scan_mkv_files(mock_dir)

        # Verify that episode_01 and episode_02 are still found successfully
        assert len(results) == 2
        stems = {r.stem for r in results}
        assert "episode_01" in stems
        assert "episode_02" in stems


def test_scan_mkv_files_ignores_directory_symlinks():
    """
    Directory symlinks should not be followed to prevent infinite recursive loop.
    """
    mock_dir = Path("D:/My Project/fake_dir")

    entry_symlink = MagicMock()
    entry_symlink.name = "recursive_symlink"
    entry_symlink.path = "D:/My Project/fake_dir/recursive_symlink"
    # is_dir(follow_symlinks=False) should return False for symlinks
    entry_symlink.is_dir.side_effect = lambda follow_symlinks=True: follow_symlinks
    entry_symlink.is_file.return_value = False

    mock_iterator = MagicMock()
    mock_iterator.__enter__.return_value = [entry_symlink]

    # We recursively scan, but since follow_symlinks=False is checked,
    # it should treat entry_symlink as a symlink and not recurse into it.
    # If it recursed, it would call os.scandir again (which would raise
    # or be tracked).
    with patch("os.scandir") as mock_scandir:
        mock_scandir.return_value = mock_iterator
        scan_mkv_files(mock_dir)
        # The root folder was scanned once, but no subfolder scans should have been called
        assert mock_scandir.call_count == 1


def test_scan_mkv_files_follows_file_symlinks():
    """
    File symlinks pointing to MKV files should be followed and included in scan results.
    """
    mock_dir = Path("D:/My Project/fake_dir")

    entry_file_symlink = MagicMock()
    entry_file_symlink.name = "symlinked_episode.mkv"
    entry_file_symlink.path = "D:/My Project/fake_dir/symlinked_episode.mkv"
    # is_file() with default follow_symlinks=True returns True for valid file symlinks
    entry_file_symlink.is_file.return_value = True
    entry_file_symlink.is_dir.return_value = False

    mock_iterator = MagicMock()
    mock_iterator.__enter__.return_value = [entry_file_symlink]

    with patch("os.scandir", return_value=mock_iterator):
        results = scan_mkv_files(mock_dir)
        assert len(results) == 1
        assert results[0].stem == "symlinked_episode"


def test_scan_assets_fresh_scan_fast(tmp_path):
    """
    scan_assets must return fresh ScanResults instances
    to prevent cross-caller list mutation bugs.
    """
    sub_dir = tmp_path / "SubDir"
    sub_dir.mkdir()
    (sub_dir / "episode_01.ass").write_bytes(b"[Script Info]")

    # First call - scans disk
    res1 = scan_assets(sub_dir)
    assert len(res1.subs) == 1

    # Second call - returns fresh un-cached object with identical contents
    res2 = scan_assets(sub_dir)
    assert res1 == res2
    assert res1 is not res2


def test_mkv_grouping_by_dir(tmp_path):
    """
    Verify grouping MKVs by parent directory produces expected mapping.
    """
    from collections import defaultdict
    s1 = tmp_path / "Season 1"
    s2 = tmp_path / "Season 2"
    s1.mkdir()
    s2.mkdir()
    (s1 / "ep01.mkv").touch()
    (s1 / "ep02.mkv").touch()
    (s2 / "ep01.mkv").touch()

    all_mkvs = scan_mkv_files(tmp_path)
    mkvs_by_dir = defaultdict(list)
    for mkv in all_mkvs:
        mkvs_by_dir[mkv.path.parent].append(mkv)

    assert len(mkvs_by_dir[s1]) == 2
    assert len(mkvs_by_dir[s2]) == 1


def test_scripts_run_accepts_persistent_cache(tmp_path):
    """
    Verify _run in both mkv_remux and mkv_extract accepts persistent_cache keyword argument.
    """
    from concurrent.futures import ThreadPoolExecutor

    import mkv_extract
    import mkv_remux

    ep_file = tmp_path / "Show_E01.mkv"
    ep_file.touch()

    all_mkvs = scan_mkv_files(tmp_path)

    with ThreadPoolExecutor(max_workers=1) as pool:
        cache = PersistentInspectCache(cache_file=tmp_path / ".mkv_inspect_cache.json")
        # Extract script _run invocation with persistent_cache
        mkv_extract._run(
            tmp_path,
            tmp_path / "fonts",
            all_mkvs,
            pool,
            1,
            dry_run=True,
            persistent_cache=cache,
        )

        # Remux script _run invocation with persistent_cache
        release = mkv_remux.prepare_release_directory(tmp_path, tmp_path, all_mkvs)
        config = mkv_remux.RemuxRunConfig(
            runtime=mkv_runner.CliRuntime(
                workers=1,
                sub_offset=0.0,
                dry_run=True,
                io_semaphore=None,
                persistent_cache=cache,
            ),
            root_input_dir=tmp_path,
            output_dir=tmp_path / "out",
            temp_dir=None,
            opus_bitrate="192k",
            complexity=10,
            ffmpeg_threads=None,
            interaction_mode=mkv_remux.InteractionMode.DRY_RUN,
        )
        mkv_remux._run(release, config, pool)


def test_all_script_imports():
    """
    Ensure all entrypoint and common scripts can be imported cleanly without missing dependencies or syntax errors.
    """
    import mkv_extract
    import mkv_remux
    import mkv_scan
    assert mkv_scan is not None
    assert mkv_extract is not None
    assert mkv_remux is not None
    assert mkv_extract is not None
    assert mkv_remux is not None

def test_scan_assets_uses_scan_cache(tmp_path, monkeypatch):

    shared_ancestor = tmp_path / "Anime"
    shared_ancestor.mkdir()
    (shared_ancestor / "op.ass").touch()

    original_scandir = os.scandir
    call_count = 0

    def mock_scandir(path):
        nonlocal call_count
        if str(path) == str(shared_ancestor):
            call_count += 1
        return original_scandir(path)

    monkeypatch.setattr(os, "scandir", mock_scandir)

    cache = {}

    # First call scans the directory
    res1 = scan_assets(shared_ancestor, scan_cache=cache)
    assert call_count == 1
    assert shared_ancestor in cache

    # Second call returns from cache
    res2 = scan_assets(shared_ancestor, scan_cache=cache)
    assert call_count == 1

    # Fresh copies
    assert res1 is not res2
    assert res1.subs is not res2.subs

    assert len(res1.subs) == 1
    assert res1.subs[0].path == res2.subs[0].path

    # Caller mutating a returned list must not corrupt the cached entry
    res1.subs.clear()
    res3 = scan_assets(shared_ancestor, scan_cache=cache)
    assert len(res3.subs) == 1

    # Equivalent path spellings hit the same cache entry
    res4 = scan_assets(shared_ancestor / ".", scan_cache=cache)
    assert call_count == 1
    assert len(res4.subs) == 1

