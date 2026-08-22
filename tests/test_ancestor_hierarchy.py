"""
Tests for ancestor directory asset discovery and local-first override precedence.
"""
from pathlib import Path
import mkv_common
import mkv_remux


def test_ancestor_dirs_structure(tmp_path):
    root = tmp_path / "Anime"
    s1 = root / "Season 01"
    arc = s1 / "Cour 1"
    arc.mkdir(parents=True)

    ancestors = mkv_remux._ancestor_dirs(arc, root)
    assert ancestors == [root, s1, arc]


def test_ancestor_dirs_outside_root(tmp_path):
    other = tmp_path / "Other"
    root = tmp_path / "Root"
    other.mkdir(parents=True)
    root.mkdir(parents=True)

    assert mkv_remux._ancestor_dirs(other, root) == [other]


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

    search_dirs = mkv_remux._ancestor_dirs(season_dir, root_dir)
    assert search_dirs == [root_dir, season_dir]

    import mkv_assets
    resolver = mkv_assets.AssetResolver.discover(
        search_dirs, season_dir / "fonts"
    )

    # Local Indonesian sub must take precedence over root English sub
    matched_sub = resolver.sub_index.lookup("E", "01", "1")
    assert matched_sub is not None
    assert matched_sub[0] == season_dir / "Show - 01.ind.ass"
    assert matched_sub[1] == "ind"

    # Local episode cover must take precedence over root global cover
    matched_cover = resolver.cover_index.lookup("E", "01", "1")
    assert matched_cover == season_dir / "Show - 01_cover.jpg"


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

    search_dirs = mkv_remux._ancestor_dirs(season_dir, root_dir)
    import mkv_assets
    resolver = mkv_assets.AssetResolver.discover(search_dirs, season_dir / "fonts")

    assert set(resolver.all_subtitles) == {unparsed_sub, parsed_sub}
    # Lookup for E01 matches only the parseable candidate; E02 matches nothing.
    matched_e01 = resolver.sub_index.lookup("E", "01", "1")
    assert matched_e01 is not None and matched_e01[0] == parsed_sub
    assert resolver.sub_index.lookup("E", "02", "1") is None


def _make_video(path: Path):
    return mkv_common.ParsedFile(
        path=path, name=path.name, stem=path.stem, parsed=("1", "E", "01")
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
    res_empty = empty_resolver.resolve_for(video, None, interactive=False)
    assert res_empty.matched_subtitle is False
    assert res_empty.skip_reason == "no subtitle files found"

    # Case 2: subtitle files exist but none match this episode
    (season_dir / "Show - 02.ass").write_bytes(b"other_episode")
    (season_dir / "[Group] Unparseable Name.ass").write_bytes(b"unparseable")
    full_resolver = mkv_assets.AssetResolver.discover([season_dir], season_dir / "fonts")
    res_full = full_resolver.resolve_for(
        video, None, all_subs=full_resolver.all_subtitles, interactive=False
    )
    assert res_full.matched_subtitle is False
    assert res_full.skip_reason == "subtitle files found but none matched"

