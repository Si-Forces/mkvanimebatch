"""
Tests for _pick_audio() from both remux modules, and seasons_match().

_pick_audio is a private function but it is the most complex piece of logic
in the audio selection pipeline, so it is tested directly.
"""
import importlib
import sys
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

mkv_remux      = importlib.import_module("mkv_remux")

REMUX_MODULES = [
    pytest.param(mkv_remux,      id="mkv_remux"),
    pytest.param(mkv_remux, id="mkv_remux"),
]


def _audio(lang, channels=2, name="", pos=1, codec="A_AAC"):
    return {
        "id": pos,
        "pos": pos,
        "audio_idx": pos - 1,
        "lang": lang,
        "channels": channels,
        "name": name,
        "codec": codec,
    }


# ─────────────────────────────────────────────────────────────────────────────
# seasons_match  (identical in both modules)
# ─────────────────────────────────────────────────────────────────────────────

class TestSeasonsMatch:

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_both_empty_returns_true(self, rmod):
        assert rmod.seasons_match("", "") is True

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_mkv_empty_returns_true(self, rmod):
        assert rmod.seasons_match("", "2") is True

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_sub_empty_returns_true(self, rmod):
        assert rmod.seasons_match("2", "") is True

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_matching_seasons(self, rmod):
        assert rmod.seasons_match("1", "1") is True

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_different_seasons(self, rmod):
        assert rmod.seasons_match("1", "2") is False


# ─────────────────────────────────────────────────────────────────────────────
# _pick_audio
# ─────────────────────────────────────────────────────────────────────────────

class TestPickAudio:

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_prefers_japanese_stereo(self, rmod):
        tracks = [
            _audio("eng", channels=2, pos=1),
            _audio("jpn", channels=2, pos=2),
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio["lang"] in ("ja", "jpn", "jpn")
        assert audio["pos"] == 2

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_prefers_japanese_stereo_dialogue(self, rmod):
        tracks = [
            _audio("jpn", channels=2, name="Commentary", pos=1),
            _audio("jpn", channels=2, name="Full Dialogue", pos=2),
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio["pos"] == 2

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_skips_commentary_track(self, rmod):
        tracks = [
            _audio("jpn", channels=2, name="Audio Commentary", pos=1),
            _audio("jpn", channels=2, name="Main Audio", pos=2),
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio["pos"] == 2

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_prefers_stereo_over_surround(self, rmod):
        tracks = [
            _audio("jpn", channels=6, pos=1),   # 5.1
            _audio("jpn", channels=2, pos=2),   # stereo
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio["channels"] == 2
        assert audio["pos"] == 2

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_und_stereo_fallback(self, rmod):
        tracks = [
            _audio("und", channels=2, pos=1),
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio is not None
        assert audio["pos"] == 1

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_stereo_fallback_no_japanese(self, rmod):
        tracks = [
            _audio("eng", channels=2, pos=1),
            _audio("eng", channels=6, pos=2),
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio["channels"] == 2

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_empty_tracks_returns_none(self, rmod):
        audio, reason = rmod._pick_audio([], video_pos=0)
        assert audio is None
        assert reason == ""

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_all_commentary_falls_back_to_japanese(self, rmod):
        """When every track is commentary the last-resort Japanese fallback fires."""
        tracks = [
            _audio("jpn", channels=2, name="Director Commentary", pos=1),
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio is not None
        assert "Japanese" in reason or "fallback" in reason.lower() or "commentary" in reason.lower()

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_first_audio_after_video_as_last_resort(self, rmod):
        """Rule 8: first audio whose pos > video_pos when no other rule matches."""
        tracks = [
            _audio("xyz", channels=6, name="Some Audio", pos=5),
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio is not None

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_japanese_preferred_over_undetermined(self, rmod):
        tracks = [
            _audio("und", channels=2, pos=1),
            _audio("jpn", channels=2, pos=2),
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio["pos"] == 2

    @pytest.mark.parametrize("rmod", REMUX_MODULES)
    def test_non_commentary_preferred_over_commentary_even_if_commentary_is_japanese(self, rmod):
        """
        Even if a Japanese track is available, if it is a commentary track,
        and there is a non-commentary track in another language (e.g. English),
        the non-commentary track must be selected.
        """
        tracks = [
            # Japanese audio but with commentary
            {
                "id": 1,
                "audio_idx": 0,
                "codec": "A_AAC",
                "lang": "jpn",
                "channels": 2,
                "name": "Audio Commentary",
                "pos": 1,
            },
            # English audio, normal dialogues
            {
                "id": 2,
                "audio_idx": 1,
                "codec": "A_AC3",
                "lang": "eng",
                "channels": 2,
                "name": "English Stereo",
                "pos": 2,
            }
        ]
        audio, reason = rmod._pick_audio(tracks, video_pos=0)
        assert audio is not None
        assert audio["id"] == 2  # Must pick English normal track instead of Japanese commentary
        assert "Stereo fallback" in reason
