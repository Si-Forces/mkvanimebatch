"""Tests for claim_leftover_chapters — the sole-leftover chapter pre-pass."""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import mkv_assets
from mkv_scan import InspectedVideo, ParsedFile


def _make_video(path: Path) -> ParsedFile:
    from mkv_domain import parse_episode

    return ParsedFile(
        path=path,
        name=path.name,
        stem=path.stem,
        parsed=parse_episode(path.stem),
    )


def _resolver_with_unmatched(tmp_path: Path, count: int):
    names = ["chapters.xml"] + [f"extra_chapter_{i}.xml" for i in range(1, count)]
    for name in names:
        (tmp_path / name).write_text(
            "<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
            encoding="utf-8",
        )
    return mkv_assets.AssetResolver.discover([tmp_path], tmp_path / "fonts")


def test_sole_leftover_claimed_by_first_eligible_video(tmp_path):
    resolver = _resolver_with_unmatched(tmp_path, 1)
    ep1 = tmp_path / "Show - 01.mkv"
    ep2 = tmp_path / "Show - 02.mkv"
    videos = [_make_video(ep1), _make_video(ep2)]
    # ep1 already has an external chapter match; ep2 is the sole leftover video.
    (tmp_path / "Show - 01.xml").write_text(
        "<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
        encoding="utf-8",
    )

    resolver.chap_index.add(_make_video(tmp_path / "Show - 01.xml"))
    claims = mkv_assets.claim_leftover_chapters(videos, {ep1: None, ep2: None}, resolver)

    assert claims == {ep2: resolver.chap_index.unmatched[0]}


def test_no_claim_when_multiple_unmatched_files(tmp_path):
    resolver = _resolver_with_unmatched(tmp_path, 2)
    ep1 = tmp_path / "Show - 01.mkv"
    videos = [_make_video(ep1)]

    claims = mkv_assets.claim_leftover_chapters(videos, {ep1: None}, resolver)

    assert claims == {}


def test_no_claim_when_candidate_has_source_chapters(tmp_path):
    resolver = _resolver_with_unmatched(tmp_path, 1)
    ep1 = tmp_path / "Show - 01.mkv"
    videos = [_make_video(ep1)]
    # Sole leftover video has embedded chapters -> rule must not fire.
    json_cache: dict[Path, InspectedVideo | None] = {ep1: InspectedVideo({"chapters": [{"num_entries": 5}]})}

    claims = mkv_assets.claim_leftover_chapters(videos, json_cache, resolver)

    assert claims == {}


def test_resolve_for_consumes_precomputed_claim(tmp_path):
    # "Show - 02" has its own external chapter file; "Show - 01" is the
    # sole leftover video eligible for the unmatched-chapters claim.
    (tmp_path / "Show - 02.xml").write_text(
        "<?xml version='1.0'?><Chapters><EditionEntry></EditionEntry></Chapters>",
        encoding="utf-8",
    )
    resolver = _resolver_with_unmatched(tmp_path, 1)
    leftover = resolver.chap_index.unmatched[0]
    ep1 = tmp_path / "Show - 01.mkv"
    video = _make_video(ep1)
    other = _make_video(tmp_path / "Show - 02.mkv")

    resolved = resolver.resolve_for(video, None, mode=mkv_assets.InteractionMode.NON_INTERACTIVE)
    assert resolved.chapter_file is None

    claims = mkv_assets.claim_leftover_chapters([video, other], {}, resolver)
    resolved = resolver.resolve_for(
        video, None,
        mode=mkv_assets.InteractionMode.NON_INTERACTIVE,
        claimed_chapter=claims.get(ep1),
    )
    assert resolved.chapter_file == leftover

    # Resolver stays stateless: re-resolving either video changes nothing —
    # the leftover is only ever handed out through the explicit claim.
    resolved_again = resolver.resolve_for(video, None, mode=mkv_assets.InteractionMode.NON_INTERACTIVE)
    assert resolved_again.chapter_file is None
    resolved_other = resolver.resolve_for(other, None, mode=mkv_assets.InteractionMode.NON_INTERACTIVE)
    assert resolved_other.chapter_file != leftover
