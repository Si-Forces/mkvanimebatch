"""
Edge case tests for episode parsing.
"""
from mkv_domain import parse_episode

import pytest


class TestEpisodeParserEdgeCases:
    @pytest.mark.parametrize("filename, expected", [
        # Unicode / CJK titles
        ("[Group] 葬送のフリーレン - 05 [1080p].mkv", ("", "E", "05")),
        ("【推しの子】 第01話 (1080p).mkv", ("", "E", "01")),
        ("SPY×FAMILY - 12 [1080p].mkv", ("", "E", "12")),
        # Multi-episode spans (should extract primary starting episode)
        ("[Group] Show - 01-02 [1080p].mkv", ("", "E", "01")),
        ("Show S01E01-E02.mkv", ("1", "E", "01")),
        # 4-digit episode numbers
        ("One Piece - 1089 [1080p].mkv", ("", "E", "1089")),
        ("Detective Conan - 1100.mkv", ("", "E", "1100")),
        # Titles containing digits/resolutions
        ("Mob Psycho 100 II - 03.mkv", ("", "E", "03")),
        ("Demon Lord 2099 - 01.mkv", ("", "E", "01")),
        ("Show 4k 60fps - 02.mkv", ("", "E", "02")),
        # Decimal episodes
        ("Show - 12.5.mkv", ("", "E", "12.5")),
        ("Show - 0.5.mkv", ("", "E", "00.5")),
    ])
    def test_complex_episode_filenames(self, filename, expected):
        assert parse_episode(filename) == expected
