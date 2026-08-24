"""
Tests for ancestor directory asset discovery and local-first override precedence.
"""
from pathlib import Path

from mkv_domain import EpisodeInfo, ancestor_dirs
from mkv_scan import ParsedFile


def test_ancestor_dirs_structure(tmp_path):
    root = tmp_path / "Anime"
    s1 = root / "Season 01"
    arc = s1 / "Cour 1"
    arc.mkdir(parents=True)

    ancestors = ancestor_dirs(arc, root)
    assert ancestors == [root, s1, arc]


def test_ancestor_dirs_outside_root(tmp_path):
    other = tmp_path / "Other"
    root = tmp_path / "Root"
    other.mkdir(parents=True)
    root.mkdir(parents=True)

    assert ancestor_dirs(other, root) == [other]


def test_ancestor_asset_override_precedence(tmp_path):
    root_dir = tmp_path / "Anime"
    season_dir = root_dir / "Season 01"
    season_dir.mkdir(parents=True)

    # Root has global cover and global English sub
    (root_dir / "cover.jpg").write_bytes(b"root_cover")
    (root_dir / "Show - 01.eng.ass").write_bytes(b"root_eng_sub")

    # Season dir has specific episode cover and Indonesian sub
    (season_dir / "Show - 01_cover.jpg").write_bytes(b"local_cover")
    (season_dir / "Show - 01.ind.ass").write_bytes(b"local_ind_sub")

    search_dirs = ancestor_dirs(season_dir, root_dir)
    assert search_dirs == [root_dir, season_dir]

    import mkv_assets
    resolver = mkv_assets.AssetResolver.discover(
        search_dirs, season_dir / "fonts"
    )

    # Local Indonesian sub must take precedence over root English sub
    matched_sub = resolver.sub_index.lookup(EpisodeInfo("1", "E", "01"))
    assert matched_sub is not None
    assert matched_sub[0] == season_dir / "Show - 01.ind.ass"
    assert matched_sub[1] == "ind"

    # Local episode cover must take precedence over root global cover
    matched_cover = resolver.cover_index.lookup(EpisodeInfo("1", "E", "01"))
    assert matched_cover == season_dir / "Show - 01_cover.jpg"


def test_local_subtitle_next_to_mkv_always_wins_over_ancestor(tmp_path):
    """A subtitle residing in the same release directory next to the MKV unconditionally
    wins over an ancestor subtitle, even if the ancestor has a shorter filename."""
    root_dir = tmp_path / "Anime"
    season_dir = root_dir / "Season 01"
    season_dir.mkdir(parents=True)

    # Ancestor has short filename
    (root_dir / "01.ass").write_bytes(b"ancestor_short_sub")

    # Local release directory next to MKV has long filename
    local_sub = season_dir / "[SuperLongFansubGroup] Anime Title - 01 [1080p FLAC BD].ass"
    local_sub.write_bytes(b"local_long_sub")

    search_dirs = ancestor_dirs(season_dir, root_dir)
    import mkv_assets
    resolver = mkv_assets.AssetResolver.discover(
        search_dirs, season_dir / "fonts"
    )

    # When lookup is performed with local_dir=season_dir, local sub must win
    matched = resolver.sub_index.lookup(EpisodeInfo("1", "E", "01"), local_dir=season_dir)
    assert matched is not None
    assert matched[0] == local_sub


def test_assets_dir_multi_level_fallback_hierarchy(tmp_path):
    """Fallback assets placed at the root of --assets-dir are discovered when resolving nested folders."""
    root_dir = tmp_path / "Anime"
    season_dir = root_dir / "Season 01"
    season_dir.mkdir(parents=True)

    assets_override = tmp_path / "GlobalAssets"
    (assets_override / "fonts").mkdir(parents=True)
    (assets_override / "fonts" / "FontA.ttf").write_bytes(b"font_data")
    (assets_override / "cover.jpg").write_bytes(b"root_cover_data")

    search_dirs = ancestor_dirs(season_dir, root_dir)
    import mkv_assets
    resolver = mkv_assets.AssetResolver.discover(
        search_dirs,
        season_dir / "fonts",
        assets_dir=assets_override / "Season 01",
    )

    assert resolver.fonts.count == 1
    assert resolver.fallback_cover_index is not None
    assert resolver.fallback_cover_index.has_global


def test_assets_dir_wins_tie_over_its_parent(tmp_path):
    """On an exact score tie, --assets-dir itself must beat its immediate parent,
    mirroring how the nearest ancestor wins ties in the primary search chain."""
    root_dir = tmp_path / "Anime"
    season_dir = root_dir / "Season 01"
    season_dir.mkdir(parents=True)

    assets_dir = tmp_path / "Output" / "Season 01"
    assets_dir.mkdir(parents=True)
    parent_dir = assets_dir.parent

    # Identical basenames force an exact score tie (same lang, format, length)
    (assets_dir / "Show - 01.ass").write_bytes(b"assets_copy")
    (parent_dir / "Show - 01.ass").write_bytes(b"parent_copy")

    search_dirs = ancestor_dirs(season_dir, root_dir)
    import mkv_assets
    resolver = mkv_assets.AssetResolver.discover(
        search_dirs, season_dir / "fonts", assets_dir=assets_dir
    )

    assert resolver.fallback_sub_index is not None
    matched = resolver.fallback_sub_index.lookup(EpisodeInfo("1", "E", "01"))
    assert matched is not None
    assert matched[0] == assets_dir / "Show - 01.ass"


def test_assets_dir_fonts_take_precedence_over_parent_fonts(tmp_path):
    """When both --assets-dir and its parent provide fonts, the more specific one wins."""
    root_dir = tmp_path / "Anime"
    season_dir = root_dir / "Season 01"
    season_dir.mkdir(parents=True)

    assets_dir = tmp_path / "Output" / "Season 01"
    assets_dir.mkdir(parents=True)
    (assets_dir / "Specific.ttf").write_bytes(b"specific_font")
    (assets_dir.parent / "Shared.ttf").write_bytes(b"shared_font")

    search_dirs = ancestor_dirs(season_dir, root_dir)
    import mkv_assets
    resolver = mkv_assets.AssetResolver.discover(
        search_dirs, season_dir / "fonts", assets_dir=assets_dir
    )

    assert resolver.fonts.count == 1


def test_unparsed_subtitles_listed_for_manual_selection_but_never_matched(tmp_path):
    """Subtitles without a parseable episode number stay visible in the manual
    picker (all_subtitles) but are ignored by automatic lookups."""
    root_dir = tmp_path / "Anime"
    season_dir = root_dir / "Season 01"
    season_dir.mkdir(parents=True)

    unparsed_sub = season_dir / "[Group] Show Special.ass"
    unparsed_sub.write_bytes(b"unparseable_name")
    parsed_sub = season_dir / "Show - 01.ass"
    parsed_sub.write_bytes(b"parsed")

    search_dirs = ancestor_dirs(season_dir, root_dir)
    import mkv_assets
    resolver = mkv_assets.AssetResolver.discover(search_dirs, season_dir / "fonts")

    assert set(resolver.all_subtitles) == {unparsed_sub, parsed_sub}
    # Lookup for E01 matches only the parseable candidate; E02 matches nothing.
    matched_e01 = resolver.sub_index.lookup(EpisodeInfo("1", "E", "01"))
    assert matched_e01 is not None and matched_e01[0] == parsed_sub
    assert resolver.sub_index.lookup(EpisodeInfo("1", "E", "02")) is None


def _make_video(path: Path):
    return ParsedFile(
        path=path, name=path.name, stem=path.stem, parsed=EpisodeInfo("1", "E", "01")
    )


def test_skip_reason_distinguishes_missing_vs_present_but_unmatched(tmp_path):
    """resolve_for reports WHY no subtitle matched, so runners can emit honest
    skip reasons to the frontend."""
    root_dir = tmp_path / "Anime"
    season_dir = root_dir / "Season 01"
    season_dir.mkdir(parents=True)

    import mkv_assets

    video = _make_video(season_dir / "Show - 01.mkv")

    # Case 1: release directory has no subtitle files at all
    empty_resolver = mkv_assets.AssetResolver.discover([season_dir], season_dir / "fonts")
    res_empty = empty_resolver.resolve_for(video, None, mode=mkv_assets.InteractionMode.NON_INTERACTIVE)
    assert res_empty.matched_subtitle is False
    assert res_empty.skip_reason == "no subtitle files found"

    # Case 2: subtitle files exist but none match this episode
    (season_dir / "Show - 02.ass").write_bytes(b"other_episode")
    (season_dir / "[Group] Unparseable Name.ass").write_bytes(b"unparseable")
    full_resolver = mkv_assets.AssetResolver.discover([season_dir], season_dir / "fonts")
    res_full = full_resolver.resolve_for(
        video, None, all_subs=full_resolver.all_subtitles,
        mode=mkv_assets.InteractionMode.NON_INTERACTIVE
    )
    assert res_full.matched_subtitle is False
    assert res_full.skip_reason == "subtitle files found but none matched"


def test_assigned_subtitle_overrides_every_automatic_source(tmp_path):
    """A GUI/CLI-assigned subtitle wins over index matches, embedded tracks,
    and fallback assets — explicit human intent has highest precedence."""
    root_dir = tmp_path / "Anime"
    season_dir = root_dir / "Season 01"
    season_dir.mkdir(parents=True)

    auto_sub = season_dir / "Show - 01.ass"
    auto_sub.write_bytes(b"auto_match")
    manual_sub = tmp_path / "elsewhere" / "Better Rip.ass"
    manual_sub.parent.mkdir()
    manual_sub.write_bytes(b"manual_pick")

    import mkv_assets
    resolver = mkv_assets.AssetResolver.discover(
        ancestor_dirs(season_dir, root_dir), season_dir / "fonts"
    )

    video = _make_video(season_dir / "Show - 01.mkv")
    res = resolver.resolve_for(video, None, all_subs=resolver.all_subtitles,
                               assigned_subtitle=manual_sub)

    assert res.matched_subtitle is True
    assert res.subtitle_file == manual_sub
    assert res.embedded_sub_id is None

