"""
Tests for parse_episode() and detect_sub_lang().

Both functions must behave consistently across modules.
identically.  The `mod` fixture (from conftest.py) runs every test twice —
once per module — so any divergence is caught immediately.
"""
import pytest


# ─────────────────────────────────────────────────────────────────────────────
# parse_episode
# ─────────────────────────────────────────────────────────────────────────────

class TestParseEpisode:
    """Covers the most common fansub filename conventions."""

    # ── standard dash marker ─────────────────────────────────────────────────
    def test_dash_episode_basic(self, mod):
        assert mod.parse_episode("[Group] Show - 01 [1080p]") == ("", "E", "01")

    def test_dash_episode_two_digits(self, mod):
        assert mod.parse_episode("Show - 12") == ("", "E", "12")

    def test_dash_episode_three_digits(self, mod):
        assert mod.parse_episode("Show - 100") == ("", "E", "100")

    def test_dash_episode_leading_spaces(self, mod):
        assert mod.parse_episode("Show  -  05") == ("", "E", "05")

    # ── SxxExx ───────────────────────────────────────────────────────────────
    def test_sxxexx_season_and_episode(self, mod):
        assert mod.parse_episode("Show S02E10") == ("2", "E", "10")

    def test_sxxexx_season_one(self, mod):
        assert mod.parse_episode("Show S01E03") == ("1", "E", "03")

    def test_sxxexx_large_episode(self, mod):
        assert mod.parse_episode("Show S03E125") == ("3", "E", "125")

    def test_sxxexx_version_suffix(self, mod):
        # vN version suffix must be stripped
        assert mod.parse_episode("Show S01E05v2") == ("1", "E", "05")

    # ── bracket notation ─────────────────────────────────────────────────────
    def test_bracket_episode(self, mod):
        assert mod.parse_episode("Show [07] [720p]") == ("", "E", "07")

    def test_paren_episode(self, mod):
        assert mod.parse_episode("Show (13)") == ("", "E", "13")

    # ── EP/Episode keyword ───────────────────────────────────────────────────
    def test_ep_keyword(self, mod):
        assert mod.parse_episode("Show EP03") == ("", "E", "03")

    def test_episode_keyword_long(self, mod):
        assert mod.parse_episode("Show Episode 07") == ("", "E", "07")

    # ── decimal episodes ─────────────────────────────────────────────────────
    def test_decimal_episode(self, mod):
        result = mod.parse_episode("Show - 12.5")
        assert result is not None
        _, ep_type, ep = result
        assert ep_type == "E"
        assert ep.startswith("12.")

    # ── OVA / OAD / ONA ──────────────────────────────────────────────────────
    def test_ova_numbered(self, mod):
        assert mod.parse_episode("Show OVA 3") == ("", "OVA", "03")

    def test_ova_no_space(self, mod):
        result = mod.parse_episode("Show OVA3")
        assert result is not None
        _, ep_type, ep = result
        assert ep_type == "OVA"
        assert ep == "03"

    def test_oad(self, mod):
        result = mod.parse_episode("Show OAD 01")
        assert result is not None
        _, ep_type, _ = result
        assert ep_type == "OVA"

    # ── Specials / SP ────────────────────────────────────────────────────────
    def test_sp_numbered(self, mod):
        assert mod.parse_episode("Show SP01") == ("", "SP", "01")

    def test_special_keyword(self, mod):
        result = mod.parse_episode("Show Special 02")
        assert result is not None
        _, ep_type, ep = result
        assert ep_type == "SP"
        assert ep == "02"

    # ── end / fin marker ─────────────────────────────────────────────────────
    def test_end_marker(self, mod):
        result = mod.parse_episode("Show - 12 END")
        assert result is not None
        _, ep_type, ep = result
        assert ep_type == "E"
        assert ep == "12"

    # ── underscore → space conversion ────────────────────────────────────────
    def test_underscores_treated_as_spaces(self, mod):
        result = mod.parse_episode("Show_-_01")
        assert result is not None
        _, ep_type, ep = result
        assert ep_type == "E"
        assert ep == "01"

    # ── no episode number ────────────────────────────────────────────────────
    def test_no_episode_returns_none(self, mod):
        assert mod.parse_episode("Random Movie Title") is None

    def test_empty_string_returns_none(self, mod):
        assert mod.parse_episode("") is None

    # ── sub offset stripping ──────────────────────────────────────────────────
    def test_parse_episode_strips_offset_suffix(self, mod):
        assert mod.parse_episode("Show - 01.[+5s]") == ("", "E", "01")
        assert mod.parse_episode("Show - 02.[-3s]") == ("", "E", "02")
        assert mod.parse_episode("Show - 03.[+1.5s]") == ("", "E", "03")
        assert mod.parse_episode("Show - 04.[-0.25s]") == ("", "E", "04")

    # ── zero-padding ─────────────────────────────────────────────────────────
    def test_single_digit_zero_padded(self, mod):
        result = mod.parse_episode("Show - 5")
        assert result is not None
        assert result[2] == "05"

    def test_double_digit_unchanged(self, mod):
        result = mod.parse_episode("Show - 15")
        assert result is not None
        assert result[2] == "15"


# ─────────────────────────────────────────────────────────────────────────────
# detect_sub_lang
# ─────────────────────────────────────────────────────────────────────────────

class TestDetectSubLang:

    # ── bracket patterns ─────────────────────────────────────────────────────
    def test_bracket_id_uppercase(self, mod):
        assert mod.detect_sub_lang("EP01[ID]") == "ind"

    def test_bracket_indonesian(self, mod):
        assert mod.detect_sub_lang("EP01[Indonesian]") == "ind"

    def test_bracket_indo(self, mod):
        assert mod.detect_sub_lang("Show - 01 [Indo]") == "ind"

    def test_bracket_english(self, mod):
        assert mod.detect_sub_lang("EP01[English]") == "eng"

    def test_bracket_eng_uppercase(self, mod):
        assert mod.detect_sub_lang("EP01[ENG]") == "eng"

    def test_bracket_japanese(self, mod):
        assert mod.detect_sub_lang("EP01[Japanese]") == "jpn"

    # ── track patterns ───────────────────────────────────────────────────────
    def test_lang_before_track(self, mod):
        assert mod.detect_sub_lang("Show_01_ind_track2") == "ind"

    def test_lang_after_track(self, mod):
        assert mod.detect_sub_lang("Show_01_track2.ind") == "ind"

    def test_non_primary_lang_track_patterns_are_preserved(self, mod):
        assert mod.detect_sub_lang("Show_01_tso_track2") == "tso"
        assert mod.detect_sub_lang("Show_01_track2.ts") == "ts"

    # ── suffix patterns ──────────────────────────────────────────────────────
    def test_dot_suffix_known(self, mod):
        assert mod.detect_sub_lang("Show - 01.ind") == "ind"

    def test_underscore_suffix_known(self, mod):
        assert mod.detect_sub_lang("Show - 01_eng") == "eng"

    def test_paren_suffix(self, mod):
        assert mod.detect_sub_lang("Show - 01 (ind)") == "ind"

    # ── unknown → und ────────────────────────────────────────────────────────
    def test_unknown_lang_returns_und(self, mod):
        assert mod.detect_sub_lang("Show - 01") == "und"

    def test_unknown_suffix_not_in_known_tokens(self, mod):
        # "xyz" is not in _KNOWN_LANG_TOKENS
        assert mod.detect_sub_lang("Show - 01.xyz") == "und"

    # ── case insensitivity ───────────────────────────────────────────────────
    def test_bracket_case_insensitive(self, mod):
        assert mod.detect_sub_lang("EP01[INDONESIAN]") == "ind"

    def test_bracket_shortcodes(self, mod):
        assert mod.detect_sub_lang("EP01[ind]") == "ind"
        assert mod.detect_sub_lang("EP01[id]") == "ind"
        assert mod.detect_sub_lang("EP01[en]") == "eng"
        assert mod.detect_sub_lang("EP01[eng]") == "eng"
        assert mod.detect_sub_lang("EP01[ja]") == "jpn"
        assert mod.detect_sub_lang("EP01[jpn]") == "jpn"

    # ── sub offset stripping ──────────────────────────────────────────────────
    def test_detect_sub_lang_strips_offset_suffix(self, mod):
        assert mod.detect_sub_lang("episode_01.ind.[+5s]") == "ind"
        assert mod.detect_sub_lang("episode_01.eng.[-3s]") == "eng"
        assert mod.detect_sub_lang("episode_01.jpn.[+2.5s]") == "jpn"
        assert mod.detect_sub_lang("episode_01.[+5s].ind") == "ind"

    # ── release year guard ───────────────────────────────────────────────────
    def test_release_year_guard_ignores_dash_years(self, mod):
        assert mod.parse_episode("Frieren - 2023 - 05") == ("", "E", "05")
        assert mod.parse_episode("One Piece - 1999 - 12") == ("", "E", "12")
        assert mod.parse_episode("Demon Lord 2099 - 01") == ("", "E", "01")
        assert mod.parse_episode("Demon Lord 2099 - 2024 - 01") == ("", "E", "01")
        assert mod.parse_episode("Show - 1234") == ("", "E", "1234")
        assert mod.parse_episode("Show - 2150") == ("", "E", "2150")
        assert mod.parse_episode("Show - EP2023") == ("", "E", "2023")


# ─────────────────────────────────────────────────────────────────────────────
# ep_type_label
# ─────────────────────────────────────────────────────────────────────────────

class TestEpTypeLabel:

    def test_episode_short(self, mod):
        assert mod.ep_type_label("E") == "EP"

    def test_episode_long(self, mod):
        assert mod.ep_type_label("E", long=True) == "Episode"

    def test_ova(self, mod):
        assert mod.ep_type_label("OVA") == "OVA"
        assert mod.ep_type_label("OVA", long=True) == "OVA"

    def test_sp_short(self, mod):
        assert mod.ep_type_label("SP") == "SP"

    def test_sp_long(self, mod):
        assert mod.ep_type_label("SP", long=True) == "Special"

    def test_unknown_passthrough(self, mod):
        assert mod.ep_type_label("UNKNOWN") == "UNKNOWN"
