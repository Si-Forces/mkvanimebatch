"""
Edge case tests for episode parsing and ASS subtitle shifting.
"""
import pytest
import mkv_common


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
        assert mkv_common.parse_episode(filename) == expected


class TestAssShiftEdgeCases:
    def test_shift_dialogue_with_commas_in_text(self):
        content = (
            "[Events]\n"
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
            "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello, world! How, are, you?\n"
        )
        shifted, count = mkv_common.shift_ass_content(content, 2.5)
        assert count == 1
        assert "0:01:02.50,0:01:07.50,Default,,0,0,0,,Hello, world! How, are, you?" in shifted

    def test_shift_minute_and_hour_rollover(self):
        content = (
            "[Events]\n"
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
            "Dialogue: 0,0:59:59.50,1:59:59.90,Default,,0,0,0,,Rollover\n"
        )
        shifted, count = mkv_common.shift_ass_content(content, 1.0)
        assert count == 1
        assert "1:00:00.50,2:00:00.90" in shifted

    def test_malformed_timestamp_leaves_line_intact(self):
        content = (
            "[Events]\n"
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
            "Dialogue: 0,invalid_time,0:01:05.00,Default,,0,0,0,,Bad timestamp\n"
        )
        shifted, count = mkv_common.shift_ass_content(content, 5.0)
        assert count == 0
        assert "Dialogue: 0,invalid_time,0:01:05.00,Default,,0,0,0,,Bad timestamp" in shifted

    def test_nan_or_inf_offset_handled_safely(self):
        content = (
            "[Events]\n"
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
            "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Test\n"
        )
        shifted, count = mkv_common.shift_ass_content(content, float("nan"))
        assert count == 0
        assert shifted == content
