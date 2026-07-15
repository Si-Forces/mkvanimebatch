"""
Tests for directory scanners (scan_mkv_files and scan_assets).
Ensures robustness against individual files throwing OSError and prevents
recursive symlink traversal.
"""
from pathlib import Path
import os
import sys
import importlib
import pytest
from unittest.mock import MagicMock, patch

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

mkv_common = importlib.import_module("mkv_common")
mkv_common_fast = importlib.import_module("mkv_common_fast")

COMMON_MODULES = [
    pytest.param(mkv_common, id="mkv_common"),
    pytest.param(mkv_common_fast, id="mkv_common_fast"),
]


@pytest.mark.parametrize("mod", COMMON_MODULES)
def test_scanner_handles_oserror_on_individual_entries(mod):
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
        results = mod.scan_mkv_files(mock_dir)
        
        # Verify that episode_01 and episode_02 are still found successfully
        assert len(results) == 2
        stems = {r.stem for r in results}
        assert "episode_01" in stems
        assert "episode_02" in stems


@pytest.mark.parametrize("mod", COMMON_MODULES)
def test_scan_mkv_files_ignores_directory_symlinks(mod):
    """
    Directory symlinks should not be followed to prevent infinite recursive loop.
    """
    mock_dir = Path("D:/My Project/fake_dir")
    
    entry_symlink = MagicMock()
    entry_symlink.name = "recursive_symlink"
    entry_symlink.path = "D:/My Project/fake_dir/recursive_symlink"
    # is_dir(follow_symlinks=False) should return False for symlinks
    entry_symlink.is_dir.side_effect = lambda follow_symlinks=True: False if not follow_symlinks else True
    entry_symlink.is_file.return_value = False

    mock_iterator = MagicMock()
    mock_iterator.__enter__.return_value = [entry_symlink]

    with patch("os.scandir", return_value=mock_iterator):
        # We recursively scan, but since follow_symlinks=False is checked,
        # it should treat entry_symlink as a symlink and not recurse into it.
        # If it recursed, it would call os.scandir again (which would raise
        # or be tracked).
        with patch("os.scandir") as mock_scandir:
            mock_scandir.return_value = mock_iterator
            results = mod.scan_mkv_files(mock_dir)
            # The root folder was scanned once, but no subfolder scans should have been called
            assert mock_scandir.call_count == 1


def test_scan_assets_lru_cache_fast(tmp_path):
    """
    scan_assets in mkv_common_fast must use lru_cache to prevent redundant disk reads.
    """
    sub_dir = tmp_path / "SubDir"
    sub_dir.mkdir()
    (sub_dir / "episode_01.ass").write_bytes(b"[Script Info]")
    
    # First call - scans disk
    res1 = mkv_common_fast.scan_assets(sub_dir)
    assert len(res1.subs) == 1

    # Second call - returns cached result
    res2 = mkv_common_fast.scan_assets(sub_dir)
    assert res1 is res2


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

    all_mkvs = mkv_common_fast.scan_mkv_files(tmp_path)
    mkvs_by_dir = defaultdict(list)
    for mkv in all_mkvs:
        mkvs_by_dir[mkv.path.parent].append(mkv)

    assert len(mkvs_by_dir[s1]) == 2
    assert len(mkvs_by_dir[s2]) == 1


def test_fast_scripts_run_accepts_inspect_cache(tmp_path):
    """
    Verify _run in both mkv_remux_fast and mkv_extract_fast accepts inspect_cache keyword argument.
    """
    import mkv_remux_fast
    import mkv_extract_fast
    from concurrent.futures import ThreadPoolExecutor

    ep_file = tmp_path / "Show_E01.mkv"
    ep_file.touch()

    all_mkvs = mkv_common_fast.scan_mkv_files(tmp_path)

    with ThreadPoolExecutor(max_workers=1) as pool:
        # Extract script _run invocation with inspect_cache
        mkv_extract_fast._run(
            tmp_path,
            tmp_path / "fonts",
            all_mkvs,
            pool,
            inspect_cache={ep_file: None},
            dry_run=True,
        )

        # Remux script _run invocation with inspect_cache
        mkv_remux_fast._run(
            tmp_path,
            all_mkvs,
            tmp_path,
            tmp_path / "out",
            tmp_path / "fonts",
            [tmp_path],
            pool,
            1,
            "192k",
            10,
            dry_run=True,
            inspect_cache={ep_file: None},
        )


def test_all_script_imports():
    """
    Ensure all entrypoint and common scripts can be imported cleanly without missing dependencies or syntax errors.
    """
    import mkv_common_fast
    import mkv_extract_fast
    import mkv_remux_fast
    import mkv_common
    import mkv_extract
    import mkv_remux
    assert mkv_common_fast is not None
    assert mkv_extract_fast is not None
    assert mkv_remux_fast is not None
    assert mkv_common is not None
    assert mkv_extract is not None
    assert mkv_remux is not None

