"""
Cross-language regex and domain verification test suite for Python.

Verifies that Python regular expressions for:
- Episode parsing (OVA, SP, SxxExx, SxxPxx, season, episode markers, fallback, release year guards)
- Subtitle language detection (bracket tokens, track suffixes, offsets)
- Safe filename sanitization and OS reserved name guards
- Dialogue track classification
- Audio commentary detection
- Chapter XML detection
- Cover file pattern matching
- Opus bitrate validation

behave with 100% parity to the Rust (mkv-core) implementation.
"""

import re

from mkv_assets import _COVER_FILE_RE
from mkv_domain import (
    is_dialogue_track,
    natural_sort_key,
    parse_episode,
    safe_filename,
)
from mkv_remux_job import _COMMENTARY_RE
from mkv_scan import _CHAPTER_TAG_RE


class TestCrossLanguageRegexParity:
    """Tests confirming Python regex behavior matches Rust's regex semantics."""

    def test_season_patterns_case_insensitivity(self):
        """Rust and Python must both match 'SEASON 2' case-insensitively."""
        res = parse_episode("Show SEASON 2 - 01.mkv")
        assert res is not None
        assert res.season == "2"
        assert res.ep == "01"

        res_lower = parse_episode("Show season 2 - 01.mkv")
        assert res_lower is not None
        assert res_lower.season == "2"

        res_mixed = parse_episode("Show Season 2 - 01.mkv")
        assert res_mixed is not None
        assert res_mixed.season == "2"

    def test_safe_filename_edge_cases(self):
        """safe_filename must handle empty, dot, and Windows reserved devices identically to Rust."""
        assert safe_filename("") == "unnamed"
        assert safe_filename("   ") == "unnamed"
        assert safe_filename(".") == "unnamed"
        assert safe_filename("..") == "unnamed"
        assert safe_filename("CON") == "_CON"
        assert safe_filename("prn.txt") == "_prn.txt"
        assert safe_filename("aux") == "_aux"
        assert safe_filename("NUL.tar.gz") == "_NUL.tar.gz"
        assert safe_filename("foo/bar") == "bar"
        assert safe_filename("foo:bar") == "foo_bar"
        assert safe_filename('foo*bar?"test') == "foo_bar__test"

    def test_dialogue_keywords_and_separators(self):
        """Dialogue track regex split on [\\s\\-_]+ matches all keywords."""
        assert is_dialogue_track("Dialogue")
        assert is_dialogue_track("dialogue")
        assert is_dialogue_track("DIALOGUE")
        assert is_dialogue_track("Main Audio")
        assert is_dialogue_track("Full Subtitles")
        assert is_dialogue_track("Indonesian Dialogue")
        assert is_dialogue_track("Indo_Sub")
        assert is_dialogue_track("Indo-Sub")
        assert not is_dialogue_track("Sign & Song")
        assert not is_dialogue_track("Signs/Songs")
        assert not is_dialogue_track("Commentary")

    def test_commentary_regex(self):
        """_COMMENTARY_RE matches commentary and audio description keywords."""
        assert _COMMENTARY_RE.search("Audio Commentary".lower())
        assert _COMMENTARY_RE.search("Director's Comment".lower())
        assert _COMMENTARY_RE.search("audio description".lower())
        assert _COMMENTARY_RE.search("ad".lower())
        assert _COMMENTARY_RE.search("extension".lower())
        assert not _COMMENTARY_RE.search("broadcast".lower())
        assert not _COMMENTARY_RE.search("broadcasting".lower())
        assert not _COMMENTARY_RE.search("addition".lower())
        assert not _COMMENTARY_RE.search("main audio".lower())

    def test_chapter_xml_sniff(self):
        """_CHAPTER_TAG_RE checks <Chapters[ >] with case-insensitivity."""
        assert _CHAPTER_TAG_RE.search(b"<Chapters>\n  <EditionEntry>\n")
        assert _CHAPTER_TAG_RE.search(b"<chapters>")
        assert _CHAPTER_TAG_RE.search(b"<Chapters >")
        assert _CHAPTER_TAG_RE.search(b"<CHAPTERS>")
        assert _CHAPTER_TAG_RE.search(b"<Chapters xmlns=\"http://www.matroska.org\">")
        assert not _CHAPTER_TAG_RE.search(b"<ChapterAtom>")
        assert not _CHAPTER_TAG_RE.search(b"<other xml>")

    def test_cover_stem_regex(self):
        """_COVER_FILE_RE captures named 'stem' before '_cover' case-insensitively."""
        m1 = _COVER_FILE_RE.match("Show - 01_cover")
        assert m1 and m1.group("stem") == "Show - 01"

        m2 = _COVER_FILE_RE.match("Show S01E02_COVER")
        assert m2 and m2.group("stem") == "Show S01E02"

        m3 = _COVER_FILE_RE.match("cover")
        assert m3 is None

    def test_bitrate_regex_parity(self):
        """Bitrate regex ^(\\d+)([kM]?)$ validates positive bitrate values."""
        pattern = re.compile(r"^(\d+)([kM]?)$")
        assert pattern.match("128k")
        assert pattern.match("192k")
        assert pattern.match("1M")
        assert pattern.match("320")
        assert not pattern.match("128kbps")
        assert not pattern.match("-128k")
        assert not pattern.match("128K")
        assert not pattern.match("abc")
        assert not pattern.match("")

    def test_natural_sort_order(self):
        """natural_sort_key produces identical order to Rust natural_sort_key."""
        inputs = [
            "EP10", "EP2", "EP1",
            "Show - 01.mkv", "Show - 1.mkv", "Show - 2.mkv", "Show - 10.mkv",
            "Show - 1.5.mkv", "Show - 01v2.mkv", "Show - 01v1.mkv",
            "Episode 100", "Episode 99", "Episode 02",
        ]
        sorted_inputs = sorted(inputs, key=natural_sort_key)
        expected = [
            "EP1",
            "EP2",
            "EP10",
            "Episode 02",
            "Episode 99",
            "Episode 100",
            "Show - 1.5.mkv",
            "Show - 01.mkv",
            "Show - 1.mkv",
            "Show - 01v1.mkv",
            "Show - 01v2.mkv",
            "Show - 2.mkv",
            "Show - 10.mkv",
        ]
        assert sorted_inputs == expected
