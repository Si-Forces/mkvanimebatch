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
