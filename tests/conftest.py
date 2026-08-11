"""
Shared pytest fixtures for the MKV automation test suite.
"""
import importlib
import sys
from pathlib import Path

import pytest

# Make sure the project root is on sys.path so both modules can be imported
PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

COMMON_MODULES = ["mkv_common"]


@pytest.fixture(params=COMMON_MODULES, ids=COMMON_MODULES)
def mod(request):
    """Import the requested common module and clear LRU caches between tests."""
    m = importlib.import_module(request.param)
    if hasattr(m, "parse_episode"):
        m.parse_episode.cache_clear()
    if hasattr(m, "detect_sub_lang"):
        m.detect_sub_lang.cache_clear()
    return m





@pytest.fixture()
def mkv_common():
    return importlib.import_module("mkv_common")
