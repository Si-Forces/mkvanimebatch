"""
Tests for shift_ass_timestamps().

Uses pytest's tmp_path fixture to create real .ass files so the read/write
path is exercised end-to-end.  Parametrized over both common modules.
"""
import pytest


ASS_HEADER = """\
[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize
Style: Default,Arial,20

[Events]
Format: Layer, Start, End, Style, Name, MarginV, MarginR, MarginL, Effect, Text
"""

def _make_ass(tmp_path, lines: list[str], filename: str = "sub.ass"):
    """Write a minimal .ass file and return its Path."""
    from pathlib import Path
    content = ASS_HEADER + "\n".join(lines) + "\n"
    p = tmp_path / filename
    p.write_text(content, encoding="utf-8")
    return p


# ─────────────────────────────────────────────────────────────────────────────
# Zero offset — must not touch the file
# ─────────────────────────────────────────────────────────────────────────────

class TestShiftAssTimestamps:

    def test_zero_offset_returns_zero(self, mod, tmp_path):
        p = _make_ass(tmp_path, [
            "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello",
        ])
        original_mtime = p.stat().st_mtime
        result = mod.shift_ass_timestamps(p, 0.0)
        assert result == 0
        # File must NOT have been rewritten
        assert p.stat().st_mtime == original_mtime

    # ─────────────────────────────────────────────────────────────────────────
    # Basic positive shift
    # ─────────────────────────────────────────────────────────────────────────

    def test_positive_shift_basic(self, mod, tmp_path):
        p = _make_ass(tmp_path, [
            "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello",
        ])
        count = mod.shift_ass_timestamps(p, 5.0)   # +5 seconds
        assert count == 1
        text = p.read_text(encoding="utf-8")
        assert "0:01:05.00" in text   # start shifted
        assert "0:01:10.00" in text   # end shifted

    def test_negative_shift_basic(self, mod, tmp_path):
        p = _make_ass(tmp_path, [
            "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello",
        ])
        count = mod.shift_ass_timestamps(p, -10.0)   # -10 seconds
        assert count == 1
        text = p.read_text(encoding="utf-8")
        assert "0:00:50.00" in text   # start shifted back

    # ─────────────────────────────────────────────────────────────────────────
    # Negative offset clamping — timestamps must never go below 0:00:00.00
    # ─────────────────────────────────────────────────────────────────────────

    def test_negative_clamps_at_zero(self, mod, tmp_path):
        p = _make_ass(tmp_path, [
            "Dialogue: 0,0:00:02.00,0:00:05.00,Default,,0,0,0,,Early line",
        ])
        # Shift back by 60 seconds — both timestamps would go negative
        count = mod.shift_ass_timestamps(p, -60.0)
        assert count == 1
        text = p.read_text(encoding="utf-8")
        # Both times must be clamped to 0:00:00.00
        assert "0:00:00.00,0:00:00.00" in text

    # ─────────────────────────────────────────────────────────────────────────
    # Comment lines are also shifted
    # ─────────────────────────────────────────────────────────────────────────

    def test_comment_lines_shifted(self, mod, tmp_path):
        p = _make_ass(tmp_path, [
            "Comment: 0,0:00:10.00,0:00:15.00,Default,,0,0,0,,Note",
        ])
        count = mod.shift_ass_timestamps(p, 1.0)
        assert count == 1
        text = p.read_text(encoding="utf-8")
        assert "0:00:11.00" in text

    # ─────────────────────────────────────────────────────────────────────────
    # Multiple dialogue lines — all shifted
    # ─────────────────────────────────────────────────────────────────────────

    def test_multiple_lines_all_shifted(self, mod, tmp_path):
        p = _make_ass(tmp_path, [
            "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Line 1",
            "Dialogue: 0,0:00:04.00,0:00:06.00,Default,,0,0,0,,Line 2",
            "Dialogue: 0,0:00:07.00,0:00:09.00,Default,,0,0,0,,Line 3",
        ])
        count = mod.shift_ass_timestamps(p, 2.0)
        assert count == 3
        text = p.read_text(encoding="utf-8")
        assert "0:00:03.00" in text
        assert "0:00:06.00" in text
        assert "0:00:09.00" in text

    # ─────────────────────────────────────────────────────────────────────────
    # Non-Events sections must NOT be modified
    # ─────────────────────────────────────────────────────────────────────────

    def test_non_events_section_untouched(self, mod, tmp_path):
        # Dialogue: line appearing BEFORE [Events] must not be shifted
        content = (
            "[Script Info]\n"
            "Dialogue: fake entry in wrong section\n"
            "\n"
            "[Events]\n"
            "Format: Layer, Start, End, Style, Name, MarginV, MarginR, MarginL, Effect, Text\n"
            "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello\n"
        )
        p = tmp_path / "sub.ass"
        p.write_text(content, encoding="utf-8")
        count = mod.shift_ass_timestamps(p, 5.0)
        assert count == 1   # only the real Dialogue line
        text = p.read_text(encoding="utf-8")
        # The fake entry in [Script Info] must still be unaltered
        assert "Dialogue: fake entry in wrong section" in text

    # ─────────────────────────────────────────────────────────────────────────
    # Fractional seconds (centisecond precision)
    # ─────────────────────────────────────────────────────────────────────────

    def test_fractional_offset(self, mod, tmp_path):
        p = _make_ass(tmp_path, [
            "Dialogue: 0,0:00:10.00,0:00:15.00,Default,,0,0,0,,Text",
        ])
        count = mod.shift_ass_timestamps(p, 0.5)   # +0.5 s = +50 cs
        assert count == 1
        text = p.read_text(encoding="utf-8")
        assert "0:00:10.50" in text
        assert "0:00:15.50" in text

    # ─────────────────────────────────────────────────────────────────────────
    # BOM handling — file written with BOM must still be parsed correctly
    # ─────────────────────────────────────────────────────────────────────────

    def test_bom_file_handled(self, mod, tmp_path):
        content = (
            "\ufeff[Script Info]\n"
            "\n"
            "[Events]\n"
            "Format: Layer, Start, End, Style, Name, MarginV, MarginR, MarginL, Effect, Text\n"
            "Dialogue: 0,0:00:05.00,0:00:10.00,Default,,0,0,0,,BOM test\n"
        )
        p = tmp_path / "bom_sub.ass"
        p.write_bytes(content.encode("utf-8-sig"))
        count = mod.shift_ass_timestamps(p, 1.0)
        assert count == 1
        
        # Verify BOM is preserved in the output file bytes
        bytes_content = p.read_bytes()
        assert bytes_content.startswith(b"\xef\xbb\xbf")
        
        text = p.read_text(encoding="utf-8-sig")
        assert "0:00:06.00" in text

    def test_no_bom_remains_no_bom(self, mod, tmp_path):
        content = (
            "[Script Info]\n"
            "\n"
            "[Events]\n"
            "Format: Layer, Start, End, Style, Name, MarginV, MarginR, MarginL, Effect, Text\n"
            "Dialogue: 0,0:00:05.00,0:00:10.00,Default,,0,0,0,,No BOM test\n"
        )
        p = tmp_path / "nobom_sub.ass"
        p.write_bytes(content.encode("utf-8"))
        count = mod.shift_ass_timestamps(p, 1.0)
        assert count == 1
        
        # Verify BOM is NOT added if not present
        bytes_content = p.read_bytes()
        assert not bytes_content.startswith(b"\xef\xbb\xbf")

    def test_case_variants_of_events_shifted(self, mod, tmp_path):
        p = _make_ass(tmp_path, [
            "dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello",
            "COMMENT: 0,0:02:00.00,0:02:05.00,Default,,0,0,0,,Note",
            "Dialogue  : 0,0:03:00.00,0:03:05.00,Default,,0,0,0,,Spaces",
        ])
        count = mod.shift_ass_timestamps(p, 5.0)
        assert count == 3
        text = p.read_text(encoding="utf-8")
        assert "0:01:05.00" in text
        assert "0:02:05.00" in text
        assert "0:03:05.00" in text
        # Original casing/spacing must be preserved
        assert "dialogue:" in text
        assert "COMMENT:" in text
        assert "Dialogue  :" in text

    # ─────────────────────────────────────────────────────────────────────────
    # Line-ending preservation (regression: CRLF must not become \r\r\n)
    # ─────────────────────────────────────────────────────────────────────────

    def test_crlf_line_endings_preserved(self, mod, tmp_path):
        content = (ASS_HEADER + "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello\n").replace("\n", "\r\n")
        p = tmp_path / "crlf.ass"
        p.write_bytes(content.encode("utf-8"))
        count = mod.shift_ass_timestamps(p, 5.0)
        assert count == 1
        raw = p.read_bytes()
        assert b"\r\r\n" not in raw
        assert raw.endswith(b"0:01:10.00,Default,,0,0,0,,Hello\r\n")

    def test_lf_line_endings_preserved(self, mod, tmp_path):
        content = ASS_HEADER + "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello\n"
        p = tmp_path / "lf.ass"
        p.write_bytes(content.encode("utf-8"))
        count = mod.shift_ass_timestamps(p, 5.0)
        assert count == 1
        raw = p.read_bytes()
        assert b"\r" not in raw
        assert raw.endswith(b"0:01:10.00,Default,,0,0,0,,Hello\n")


# ─────────────────────────────────────────────────────────────────────────────
# In-Memory shift_ass_content direct tests
# ─────────────────────────────────────────────────────────────────────────────

class TestShiftAssContent:

    def test_zero_offset_returns_unchanged(self, mod):
        raw = ASS_HEADER + "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello\n"
        content, count = mod.shift_ass_content(raw, 0.0)
        assert count == 0
        assert content == raw

    def test_in_memory_positive_shift(self, mod):
        raw = ASS_HEADER + "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello\n"
        content, count = mod.shift_ass_content(raw, 5.0)
        assert count == 1
        assert "0:01:05.00,0:01:10.00" in content

    def test_in_memory_negative_clamping(self, mod):
        raw = ASS_HEADER + "Dialogue: 0,0:00:02.00,0:00:05.00,Default,,0,0,0,,Hello\n"
        content, count = mod.shift_ass_content(raw, -10.0)
        assert count == 1
        assert "0:00:00.00,0:00:00.00" in content

    def test_in_memory_crlf_preserved(self, mod):
        raw = (ASS_HEADER + "Dialogue: 0,0:01:00.00,0:01:05.00,Default,,0,0,0,,Hello\n").replace("\n", "\r\n")
        content, count = mod.shift_ass_content(raw, 2.5)
        assert count == 1
        assert "\r\n" in content
        assert "\r\r\n" not in content
        assert "0:01:02.50,0:01:07.50" in content

