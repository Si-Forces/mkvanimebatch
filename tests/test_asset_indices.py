"""Tests for asset indexing helpers: ChapIndex, CoverIndex, chapter sniffing, and font dir discovery."""

import importlib
from pathlib import Path

import pytest

mkv_common = importlib.import_module("mkv_common")
mkv_remux = importlib.import_module("mkv_remux")


@pytest.mark.parametrize("common_mod", [
    pytest.param(mkv_common, id="mkv_common"),
])
class TestFontDirDiscovery:
    def test_find_fonts_dir_override(self, tmp_path, common_mod):
        override_dir = tmp_path / "custom_fonts"
        result = common_mod.find_fonts_dir(tmp_path, override=override_dir)
        assert result == override_dir

    def test_find_fonts_dir_exact(self, tmp_path, common_mod):
        exact_dir = tmp_path / "fonts"
        exact_dir.mkdir()
        result = common_mod.find_fonts_dir(tmp_path)
        assert result == exact_dir

    def test_find_fonts_dir_variant(self, tmp_path, common_mod):
        variant_dir = tmp_path / "attachments"
        variant_dir.mkdir()
        result = common_mod.find_fonts_dir(tmp_path)
        assert result == variant_dir

    def test_find_fonts_dir_fallback(self, tmp_path, common_mod):
        result = common_mod.find_fonts_dir(tmp_path)
        assert result == tmp_path / "fonts"


@pytest.mark.parametrize("common_mod", [
    pytest.param(mkv_common, id="mkv_common"),
])
class TestChapterSniffing:
    def test_is_chapter_xml(self, tmp_path, common_mod):
        xml_file = tmp_path / "chap.xml"
        xml_file.write_text("<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>", encoding="utf-8")
        assert common_mod.is_chapter_xml(xml_file) is True

        txt_file = tmp_path / "not_chap.txt"
        txt_file.write_text("Just regular text", encoding="utf-8")
        assert common_mod.is_chapter_xml(txt_file) is False

    def test_has_chapters(self, common_mod):
        assert common_mod.has_chapters({"chapters": [{"num_entries": 1}]}) is True
        assert common_mod.has_chapters({"chapters": []}) is False
        assert common_mod.has_chapters(None) is False

    def test_has_cover(self, common_mod):
        assert common_mod.has_cover({"attachments": [{"file_name": "cover.jpg", "content_type": "image/jpeg", "id": 1}]}) is True
        assert common_mod.has_cover({"attachments": [{"file_name": "font.ttf", "content_type": "application/x-truetype-font", "id": 2}]}) is False
        assert common_mod.has_cover({"attachments": []}) is False
        assert common_mod.has_cover(None) is False


@pytest.mark.parametrize("remux_mod, common_mod", [
    pytest.param(mkv_remux, mkv_common, id="mkv_remux"),
])
class TestAssetIndices:
    def test_chap_index_lookup(self, tmp_path, remux_mod, common_mod, monkeypatch):
        xml_path = tmp_path / "Show_S01E01_chapters.xml"
        xml_path.write_text("<?xml version='1.0'?><Chapters></Chapters>", encoding="utf-8")

        parsed_file = common_mod._create_parsed_file(xml_path)

        chap_idx = remux_mod.ChapterIndex()
        chap_idx.add(parsed_file)

        match = chap_idx.lookup("E", "01", "1")
        assert match == xml_path

    def test_cover_index_global_fallback(self, tmp_path, remux_mod, common_mod):
        cover_path = tmp_path / "cover.jpg"
        cover_path.touch()

        parsed_file = common_mod._create_parsed_file(cover_path)

        cover_idx = remux_mod.CoverIndex()
        cover_idx.add(parsed_file)

        match = cover_idx.lookup("E", "05", "")
        assert match == cover_path

    def test_cover_index_single_digit_cover_filename(self, tmp_path, remux_mod, common_mod):
        """Verify 'Show 1_cover.jpg' is indexed as Episode 01 cover, NOT misfiled as a global cover."""
        cover_path = tmp_path / "Show 1_cover.jpg"
        cover_path.touch()

        parsed_file = common_mod._create_parsed_file(cover_path)

        cover_idx = remux_mod.CoverIndex()
        cover_idx.add(parsed_file)

        match = cover_idx.lookup("E", "01", "")
        assert match == cover_path
        assert cover_idx._global is None

    def test_resolve_cover_local_overrides_all(self, tmp_path, remux_mod, common_mod):
        """Local cover in cover_index always takes priority over source cover and fallback assets."""
        local_cover = tmp_path / "Show 01_cover.jpg"
        local_cover.touch()
        fallback_cover = tmp_path / "fallback_cover.jpg"
        fallback_cover.touch()

        cover_idx = remux_mod.CoverIndex()
        cover_idx.add(common_mod._create_parsed_file(local_cover))

        fallback_idx = remux_mod.CoverIndex()
        fallback_idx.add(common_mod._create_parsed_file(fallback_cover))

        parsed = ("1", "E", "01")
        result = remux_mod._resolve_cover(
            parsed, cover_idx,
            has_source_cover=True,
            fallback_cover_index=fallback_idx,
        )
        assert result == local_cover

    def test_resolve_cover_source_cover_gates_fallback(self, tmp_path, remux_mod, common_mod):
        """When no local cover exists and video has source cover, fallback assets-dir cover is ignored."""
        fallback_cover = tmp_path / "cover.jpg"
        fallback_cover.touch()

        cover_idx = remux_mod.CoverIndex()
        fallback_idx = remux_mod.CoverIndex()
        fallback_idx.add(common_mod._create_parsed_file(fallback_cover))

        parsed = ("1", "E", "01")
        result = remux_mod._resolve_cover(
            parsed, cover_idx,
            has_source_cover=True,
            fallback_cover_index=fallback_idx,
        )
        assert result is None

    def test_resolve_cover_fallback_used_when_no_source_cover(self, tmp_path, remux_mod, common_mod):
        """When no local cover exists and video has no source cover, fallback assets-dir cover is used."""
        fallback_cover = tmp_path / "cover.jpg"
        fallback_cover.touch()

        cover_idx = remux_mod.CoverIndex()
        fallback_idx = remux_mod.CoverIndex()
        fallback_idx.add(common_mod._create_parsed_file(fallback_cover))

        parsed = ("1", "E", "01")
        result = remux_mod._resolve_cover(
            parsed, cover_idx,
            has_source_cover=False,
            fallback_cover_index=fallback_idx,
        )
        assert result == fallback_cover
