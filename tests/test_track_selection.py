"""
Tests for select_best_subtitle_track() and is_dialogue_track().
Parametrized over both common modules.
"""
import pytest


def _track(tid, lang, codec="s_text/ass", name=""):
    """Build a minimal track dict matching the mkvmerge JSON structure."""
    return {
        "id": tid,
        "type": "subtitles",
        "properties": {
            "codec_id": codec,
            "language": lang,
            "track_name": name,
        },
    }


def _non_sub_track(tid, lang="jpn"):
    """Audio track — must never be selected."""
    return {
        "id": tid,
        "type": "audio",
        "properties": {"language": lang},
    }


# ─────────────────────────────────────────────────────────────────────────────
# is_dialogue_track
# ─────────────────────────────────────────────────────────────────────────────

class TestIsDialogueTrack:

    def test_dialogue_keyword(self, mod):
        assert mod.is_dialogue_track("Full Dialogue") is True

    def test_main_keyword(self, mod):
        assert mod.is_dialogue_track("Main") is True

    def test_indo_keyword(self, mod):
        assert mod.is_dialogue_track("Indo Sub") is True

    def test_indonesia_keyword(self, mod):
        assert mod.is_dialogue_track("Indonesia") is True

    def test_complete_keyword(self, mod):
        assert mod.is_dialogue_track("Complete") is True

    def test_signs_only_not_dialogue(self, mod):
        assert mod.is_dialogue_track("Signs & Songs") is False

    def test_none_input(self, mod):
        assert mod.is_dialogue_track(None) is False

    def test_empty_string(self, mod):
        assert mod.is_dialogue_track("") is False

    def test_unrelated_name(self, mod):
        assert mod.is_dialogue_track("Subtitles Track 1") is False


# ─────────────────────────────────────────────────────────────────────────────
# select_best_subtitle_track — priority ordering
# ─────────────────────────────────────────────────────────────────────────────

class TestSelectBestSubtitleTrack:

    def test_returns_none_when_no_tracks(self, mod):
        assert mod.select_best_subtitle_track([]) is None

    def test_returns_none_when_only_audio_tracks(self, mod):
        tracks = [_non_sub_track(1)]
        assert mod.select_best_subtitle_track(tracks) is None

    def test_picks_ind_over_eng(self, mod):
        tracks = [_track(1, "eng"), _track(2, "ind")]
        tid, lang, _ = mod.select_best_subtitle_track(tracks)
        assert lang == "ind"
        assert tid == 2

    def test_picks_id_as_primary(self, mod):
        tracks = [_track(1, "eng"), _track(2, "id")]
        tid, lang, _ = mod.select_best_subtitle_track(tracks)
        assert lang == "id"

    def test_picks_und_over_eng(self, mod):
        tracks = [_track(1, "eng"), _track(2, "und")]
        tid, lang, _ = mod.select_best_subtitle_track(tracks)
        assert lang == "und"

    def test_prefers_dialogue_track_within_same_lang(self, mod):
        tracks = [
            _track(1, "ind", name="Signs"),
            _track(2, "ind", name="Full Dialogue"),
        ]
        tid, lang, is_diag = mod.select_best_subtitle_track(tracks)
        assert tid == 2
        assert is_diag is True

    def test_english_fallback_when_no_primary(self, mod):
        tracks = [_track(1, "eng"), _track(2, "jpn")]
        result = mod.select_best_subtitle_track(tracks)
        assert result is not None
        tid, lang, _ = result
        assert lang == "eng"

    def test_last_resort_fallback_unrecognised_lang(self, mod):
        tracks = [_track(1, "zho"), _track(2, "kor")]
        result = mod.select_best_subtitle_track(tracks)
        assert result is not None
        tid, lang, _ = result
        assert tid == 1
        assert lang == "zho"

    def test_last_resort_fallback_prefers_dialogue(self, mod):
        tracks = [
            _track(1, "zho", name="Signs"),
            _track(2, "kor", name="Full Dialogue"),
        ]
        result = mod.select_best_subtitle_track(tracks)
        assert result is not None
        tid, lang, is_diag = result
        assert tid == 2
        assert lang == "kor"
        assert is_diag is True

    def test_last_resort_fallback_prefers_ass(self, mod):
        # Track 1 is Chinese SRT, Track 2 is Tsonga ASS. It should prefer Track 2.
        tracks = [
            _track(1, "zho", codec="s_text/utf8"),
            _track(2, "tso", codec="s_text/ass"),
        ]
        result = mod.select_best_subtitle_track(tracks)
        assert result is not None
        tid, lang, _ = result
        assert tid == 2
        assert lang == "tso"

    def test_codec_filter_excludes_wrong_codec(self, mod):
        tracks = [_track(1, "ind", codec="s_text/utf8")]
        result = mod.select_best_subtitle_track(
            tracks, codec_filter=frozenset({"s_text/ass"})
        )
        assert result is None

    def test_codec_filter_passes_correct_codec(self, mod):
        tracks = [_track(1, "ind", codec="s_text/ass")]
        result = mod.select_best_subtitle_track(
            tracks, codec_filter=frozenset({"s_text/ass"})
        )
        assert result is not None
        assert result[0] == 1

    def test_track_without_id_is_skipped(self, mod):
        bad_track = {
            "id": None,
            "type": "subtitles",
            "properties": {"language": "ind"},
        }
        result = mod.select_best_subtitle_track([bad_track])
        assert result is None

    def test_non_subtitle_type_ignored(self, mod):
        tracks = [_non_sub_track(1, "ind")]
        result = mod.select_best_subtitle_track(tracks)
        assert result is None

    def test_ind_primary_order_before_id(self, mod):
        # ind comes before id in SUBTITLE_LANGS_PRIMARY_ORDER
        tracks = [_track(1, "id"), _track(2, "ind")]
        tid, lang, _ = mod.select_best_subtitle_track(tracks)
        assert lang == "ind"

    def test_eng_before_en_in_fallback(self, mod):
        tracks = [_track(1, "en"), _track(2, "eng")]
        tid, lang, _ = mod.select_best_subtitle_track(tracks)
        assert lang == "eng"
