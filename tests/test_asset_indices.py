"""Tests for asset indexing helpers: ChapIndex, CoverIndex, chapter sniffing, and font dir discovery."""



import mkv_assets
from mkv_domain import EpisodeInfo
from mkv_fonts import FontInfo, find_fonts_dir
from mkv_scan import _create_parsed_file, has_chapters, has_cover, is_chapter_xml


class TestFontDirDiscovery:
    def test_find_fonts_dir_override(self, tmp_path):
        override_dir = tmp_path / "custom_fonts"
        result = find_fonts_dir(tmp_path, override=override_dir)
        assert result == override_dir

    def test_find_fonts_dir_exact(self, tmp_path):
        exact_dir = tmp_path / "fonts"
        exact_dir.mkdir()
        result = find_fonts_dir(tmp_path)
        assert result == exact_dir

    def test_find_fonts_dir_variant(self, tmp_path):
        variant_dir = tmp_path / "attachments"
        variant_dir.mkdir()
        result = find_fonts_dir(tmp_path)
        assert result == variant_dir

    def test_find_fonts_dir_fallback(self, tmp_path):
        result = find_fonts_dir(tmp_path)
        assert result == tmp_path / "fonts"


class TestChapterSniffing:
    def test_is_chapter_xml(self, tmp_path):
        xml_file = tmp_path / "chap.xml"
        xml_file.write_text("<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>", encoding="utf-8")
        assert is_chapter_xml(xml_file) is True

        txt_file = tmp_path / "not_chap.txt"
        txt_file.write_text("Just regular text", encoding="utf-8")
        assert is_chapter_xml(txt_file) is False

    def test_has_chapters(self):
        assert has_chapters({"chapters": [{"num_entries": 1}]}) is True
        assert has_chapters({"chapters": []}) is False
        assert has_chapters(None) is False

    def test_has_cover(self):
        assert has_cover({"attachments": [{"file_name": "cover.jpg", "content_type": "image/jpeg", "id": 1}]}) is True
        assert has_cover({"attachments": [{"file_name": "font.ttf", "content_type": "application/x-truetype-font", "id": 2}]}) is False
        assert has_cover({"attachments": []}) is False
        assert has_cover(None) is False


class TestAssetIndices:
    def test_chap_index_lookup(self, tmp_path):
        xml_path = tmp_path / "Show_S01E01_chapters.xml"
        xml_path.write_text("<?xml version='1.0'?><Chapters></Chapters>", encoding="utf-8")

        parsed_file = _create_parsed_file(xml_path)

        chap_idx = mkv_assets.ChapterIndex()
        chap_idx.add(parsed_file)

        match = chap_idx.lookup(EpisodeInfo("1", "E", "01"))
        assert match == xml_path

    def test_cover_index_global_fallback(self, tmp_path):
        cover_path = tmp_path / "cover.jpg"
        cover_path.touch()

        parsed_file = _create_parsed_file(cover_path)

        cover_idx = mkv_assets.CoverIndex()
        cover_idx.add(parsed_file)

        match = cover_idx.lookup(EpisodeInfo("", "E", "05"))
        assert match == cover_path

    def test_cover_index_single_digit_cover_filename(self, tmp_path):
        """Verify 'Show 1_cover.jpg' is indexed as Episode 01 cover, NOT misfiled as a global cover."""
        cover_path = tmp_path / "Show 1_cover.jpg"
        cover_path.touch()

        parsed_file = _create_parsed_file(cover_path)

        cover_idx = mkv_assets.CoverIndex()
        cover_idx.add(parsed_file)

        match = cover_idx.lookup(EpisodeInfo("", "E", "01"))
        assert match == cover_path
        assert cover_idx._global is None

    def test_resolve_cover_local_overrides_all(self, tmp_path):
        """Local cover in cover_index always takes priority over source cover and fallback assets."""
        local_cover = tmp_path / "Show 01_cover.jpg"
        local_cover.touch()
        fallback_cover = tmp_path / "fallback_cover.jpg"
        fallback_cover.touch()

        cover_idx = mkv_assets.CoverIndex()
        cover_idx.add(_create_parsed_file(local_cover))

        fallback_idx = mkv_assets.CoverIndex()
        fallback_idx.add(_create_parsed_file(fallback_cover))

        parsed = EpisodeInfo("1", "E", "01")
        resolver = mkv_assets.AssetResolver(
            fonts=FontInfo(args=(), count=0),
            sub_index=mkv_assets.SubIndex(),
            chap_index=mkv_assets.ChapterIndex(),
            cover_index=cover_idx,
            fallback_cover_index=fallback_idx,
        )
        result = resolver._resolve_cov(
            parsed,
            has_source_cover=True,
        )
        assert result == local_cover

    def test_resolve_cover_source_cover_gates_fallback(self, tmp_path):
        """When no local cover exists and video has source cover, fallback assets-dir cover is ignored."""
        fallback_cover = tmp_path / "cover.jpg"
        fallback_cover.touch()

        cover_idx = mkv_assets.CoverIndex()
        fallback_idx = mkv_assets.CoverIndex()
        fallback_idx.add(_create_parsed_file(fallback_cover))

        parsed = EpisodeInfo("1", "E", "01")
        resolver = mkv_assets.AssetResolver(
            fonts=FontInfo(args=(), count=0),
            sub_index=mkv_assets.SubIndex(),
            chap_index=mkv_assets.ChapterIndex(),
            cover_index=cover_idx,
            fallback_cover_index=fallback_idx,
        )
        result = resolver._resolve_cov(
            parsed,
            has_source_cover=True,
        )
        assert result is None

    def test_resolve_cover_fallback_used_when_no_source_cover(self, tmp_path):
        """When no local cover exists and video has no source cover, fallback assets-dir cover is used."""
        fallback_cover = tmp_path / "cover.jpg"
        fallback_cover.touch()

        cover_idx = mkv_assets.CoverIndex()
        fallback_idx = mkv_assets.CoverIndex()
        fallback_idx.add(_create_parsed_file(fallback_cover))

        parsed = EpisodeInfo("1", "E", "01")
        resolver = mkv_assets.AssetResolver(
            fonts=FontInfo(args=(), count=0),
            sub_index=mkv_assets.SubIndex(),
            chap_index=mkv_assets.ChapterIndex(),
            cover_index=cover_idx,
            fallback_cover_index=fallback_idx,
        )
        result = resolver._resolve_cov(
            parsed,
            has_source_cover=False,
        )
        assert result == fallback_cover
