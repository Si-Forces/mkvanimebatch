"""
Tests for SubIndex matching unrecognized/other languages (like Tsonga)
as external subtitle files.
"""
from pathlib import Path
import importlib
import sys

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

mkv_remux = importlib.import_module("mkv_remux")

REMUX_MODULES = [
    pytest.param(mkv_remux, id="mkv_remux"),
]

@pytest.mark.parametrize("rmod", REMUX_MODULES)
def test_sub_index_other_language_matching(rmod):
    from mkv_common import ParsedFile
    
    sub_index = rmod.SubIndex()
    
    # Simulate a parsed external subtitle file named `episode_01(ts).ass`
    # which is detected as having language 'ts' (Tsonga)
    sub_file = ParsedFile(
        path=Path("D:/My Project/episode_01(ts).ass"),
        name="episode_01(ts).ass",
        stem="episode_01(ts)",
        parsed=("", "E", "01")
    )
    
    sub_index.add(sub_file)
    
    # Lookup for episode 1 should find the external Tsonga subtitle
    result = sub_index.lookup("E", "01", "")
    assert result is not None
    
    path, lang = result
    assert path == Path("D:/My Project/episode_01(ts).ass")
    assert lang == "ts"

@pytest.mark.parametrize("rmod", REMUX_MODULES)
def test_sub_index_different_languages_not_duplicates(rmod):
    from mkv_common import ParsedFile
    
    sub_index = rmod.SubIndex()
    
    # Add English subtitle first
    sub_eng = ParsedFile(
        path=Path("D:/My Project/episode_01.eng.ass"),
        name="episode_01.eng.ass",
        stem="episode_01.eng",
        parsed=("", "E", "01")
    )
    # Add Indonesian subtitle second
    sub_ind = ParsedFile(
        path=Path("D:/My Project/episode_01.ind.ass"),
        name="episode_01.ind.ass",
        stem="episode_01.ind",
        parsed=("", "E", "01")
    )
    
    sub_index.add(sub_eng)
    sub_index.add(sub_ind)
    
    # Lookup should choose the Indonesian one (higher priority score)
    # even though English was added first.
    result = sub_index.lookup("E", "01", "")
    assert result is not None
    
    path, lang = result
    assert path == Path("D:/My Project/episode_01.ind.ass")
    assert lang == "ind"

@pytest.mark.parametrize("rmod", REMUX_MODULES)
def test_sub_index_different_formats_not_duplicates(rmod):
    from mkv_common import ParsedFile
    
    sub_index = rmod.SubIndex()
    
    # Add Indonesian SRT
    sub_srt = ParsedFile(
        path=Path("D:/My Project/episode_01.ind.srt"),
        name="episode_01.ind.srt",
        stem="episode_01.ind",
        parsed=("", "E", "01")
    )
    # Add Indonesian ASS
    sub_ass = ParsedFile(
        path=Path("D:/My Project/episode_01.ind.ass"),
        name="episode_01.ind.ass",
        stem="episode_01.ind",
        parsed=("", "E", "01")
    )
    
    sub_index.add(sub_srt)
    sub_index.add(sub_ass)
    
    # Lookup should choose the ASS one because it scores higher than SRT,
    # proving they were not treated as duplicates of each other.
    result = sub_index.lookup("E", "01", "")
    assert result is not None
    
    path, lang = result
    assert path == Path("D:/My Project/episode_01.ind.ass")
    assert lang == "ind"
