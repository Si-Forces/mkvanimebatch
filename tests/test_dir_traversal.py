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
