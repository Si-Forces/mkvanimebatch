"""
mkv_assets.py
Asset discovery, indexing, and resolution for anime releases (subtitles, chapters, covers, fonts).
"""
from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

from mkv_common import (
    _ASS_SCORE_MAP,
    _NON_ASS_SCORE_MAP,
    LANG_LABEL,
    LANG_MKV_TAG,
    SUBTITLE_LANGS_FALLBACK,
    SUBTITLE_LANGS_PRIMARY,
    EpisodeInfo,
    FontInfo,
    ParsedFile,
    ScanResults,
    detect_sub_lang,
    has_chapters,
    has_cover,
    is_chapter_xml,
    is_cover_filename,
    is_dialogue_track,
    logger,
    natural_sort_key,
    parse_episode,
    scan_assets,
    scan_fonts,
    seasons_match,
    select_best_subtitle_track,
)

__all__ = ["AssetResolver", "ChapterIndex", "CoverIndex", "ResolvedAssets", "SubIndex"]


# ─── ASSET INDICES (Subtitles, Chapters, Covers) ─────────────────────────────
def _ep_key(ep_type: str, ep: str) -> str:
    return f"{ep_type}|{ep}"


def _reject_duplicate(
    existing: list[tuple[Path, str]],
    path: Path,
    season: str,
    key: str,
    warning: str,
) -> bool:
    for existing_path, existing_season in existing:
        if existing_season != season:
            continue
        if existing_path.parent == path.parent:
            logger.warning(warning, key, path.parent.name, existing_path.name, path.name)
        return True
    return False


class SubIndex:
    """Index of subtitle files supporting language scoring."""
    def __init__(self) -> None:
        self._index: dict[str, list[ParsedFile]] = {}
        self._unparsed: list[ParsedFile] = []
        self._unparsed_paths: set[Path] = set()

    def add(self, sub_file: ParsedFile) -> None:
        if not sub_file.parsed:
            if sub_file.path not in self._unparsed_paths:
                self._unparsed_paths.add(sub_file.path)
                self._unparsed.append(sub_file)
            return
        _, ep_type, ep = sub_file.parsed
        key = _ep_key(ep_type, ep)
        if any(existing.path == sub_file.path for existing in self._index.get(key, ())):
            return
        self._index.setdefault(key, []).append(sub_file)

    def lookup(self, ep_type: str, ep: str, mkv_season: str) -> tuple[Path, str] | None:
        key = _ep_key(ep_type, ep)
        candidates = []
        for f in self._index.get(key, ()):
            if f.parsed is not None and seasons_match(mkv_season, f.parsed[0]):
                candidates.append(f)

        if not candidates:
            return None

        def _candidate_score(f: ParsedFile) -> tuple[int, int, int]:
            lang = detect_sub_lang(f.stem)
            is_ass = (f.path.suffix.lower() == ".ass")
            is_diag = is_dialogue_track(f.stem)
            scores = _ASS_SCORE_MAP if is_ass else _NON_ASS_SCORE_MAP
            cat_score = scores.get(lang, 7 if is_ass else 1)
            return (cat_score, 1 if is_diag else 0, -len(f.name))

        best = max(candidates, key=_candidate_score)
        return best.path, detect_sub_lang(best.stem)

    @property
    def indexed_count(self) -> int: return sum(len(lst) for lst in self._index.values())

    def _count_by_lang(self, lang_set: frozenset[str]) -> int:
        return sum(
            1
            for lst in self._index.values()
            for f in lst
            if detect_sub_lang(f.stem) in lang_set
        )

    @property
    def primary_count(self) -> int:
        return self._count_by_lang(SUBTITLE_LANGS_PRIMARY)

    @property
    def fallback_count(self) -> int:
        return self._count_by_lang(SUBTITLE_LANGS_FALLBACK)

    @property
    def all_subtitles(self) -> list[Path]:
        """Every discovered subtitle path, including files with no parseable
        episode number (which lookups ignore but manual selection can still use)."""
        discovered = [f.path for f in self._unparsed]
        discovered += [f.path for lst in self._index.values() for f in lst]
        return sorted(dict.fromkeys(discovered), key=natural_sort_key)


class ChapterIndex:
    def __init__(self) -> None:
        self._index:     dict[str, list[tuple[Path, str]]] = {}
        self._unmatched: list[Path] = []
        self._rejected = 0

    def add(self, xml_file: ParsedFile) -> None:
        if not is_chapter_xml(xml_file.path):
            self._rejected += 1
            return
        parsed = xml_file.parsed
        if parsed:
            season, ep_type, ep = parsed
            key = _ep_key(ep_type, ep)
            existing = self._index.setdefault(key, [])
            if _reject_duplicate(
                existing,
                xml_file.path,
                season,
                key,
                "Duplicate chapter file for key %s in '%s': keeping '%s', ignoring '%s'",
            ):
                return
            existing.append((xml_file.path, season))
        else:
            self._unmatched.append(xml_file.path)

    def lookup(self, ep_type: str, ep: str, mkv_season: str) -> Path | None:
        key = _ep_key(ep_type, ep)
        if key in self._index:
            for path, season in self._index[key]:
                if seasons_match(mkv_season, season):
                    return path
        return None

    def has_match(self, ep_type: str, ep: str, season: str) -> bool:
        return self.lookup(ep_type, ep, season) is not None

    @property
    def indexed_count(self) -> int:  return sum(len(lst) for lst in self._index.values())
    @property
    def rejected_count(self) -> int: return self._rejected
    @property
    def unmatched(self) -> list[Path]: return list(self._unmatched)


_COVER_FILE_RE = re.compile(r"^(?P<stem>.+)_cover$", re.IGNORECASE)


class CoverIndex:
    def __init__(self) -> None:
        self._index: dict[str, list[tuple[Path, str]]] = {}
        self._global: Path | None = None

    def add(self, image_file: ParsedFile) -> None:
        stem = image_file.stem
        is_global = False
        parsed = None

        if is_cover_filename(image_file.name):
            is_global = True
        else:
            m = _COVER_FILE_RE.match(stem)
            if m:
                parsed = parse_episode(m.group("stem"))
                if not parsed:
                    is_global = True
            else:
                return

        if is_global:
            if self._global is None:
                self._global = image_file.path
            elif self._global.parent == image_file.path.parent:
                logger.warning("Duplicate global cover in '%s': keeping '%s', ignoring '%s'", image_file.path.parent.name, self._global.name, image_file.name)
            return

        if parsed is None:
            return
        season, ep_type, ep = parsed
        key = _ep_key(ep_type, ep)
        existing = self._index.setdefault(key, [])
        if _reject_duplicate(
            existing,
            image_file.path,
            season,
            key,
            "Duplicate cover image for key %s in '%s': keeping '%s', ignoring '%s'",
        ):
            return
        existing.append((image_file.path, season))

    def lookup(self, ep_type: str, ep: str, mkv_season: str) -> Path | None:
        key = _ep_key(ep_type, ep)
        if key in self._index:
            for path, season in self._index[key]:
                if seasons_match(mkv_season, season):
                    return path
        return self._global

    @property
    def indexed_count(self) -> int: return sum(len(lst) for lst in self._index.values())
    @property
    def has_global(self) -> bool:   return self._global is not None


# ─── ASSET RESOLVER (Deep Module) ────────────────────────────────────────────
@dataclass(frozen=True)
class ResolvedAssets:
    subtitle_file: Path | None
    embedded_sub_id: int | None
    subtitle_lang: str
    subtitle_label: str
    subtitle_mkv_lang: str
    matched_subtitle: bool
    chapter_file: Path | None
    cover_file: Path | None
    fonts: FontInfo
    skip_reason: str | None = None


def _populate_indices(
    assets: ScanResults,
    sub_index: SubIndex,
    chap_index: ChapterIndex,
    cover_index: CoverIndex,
) -> None:
    for sub_file in assets.subs:
        sub_index.add(sub_file)
    for xml_file in assets.chapters:
        chap_index.add(xml_file)
    for image_file in assets.covers:
        cover_index.add(image_file)


class AssetResolver:
    """Deep module that discovers, indexes, and resolves assets for anime video releases."""

    def __init__(
        self,
        fonts: FontInfo,
        sub_index: SubIndex,
        chap_index: ChapterIndex,
        cover_index: CoverIndex,
        fallback_sub_index: SubIndex | None = None,
        fallback_chap_index: ChapterIndex | None = None,
        fallback_cover_index: CoverIndex | None = None,
    ) -> None:
        self.fonts = fonts
        self.sub_index = sub_index
        self.chap_index = chap_index
        self.cover_index = cover_index
        self.fallback_sub_index = fallback_sub_index
        self.fallback_chap_index = fallback_chap_index
        self.fallback_cover_index = fallback_cover_index
        self._chapter_unmatched_claimed: set[Path] = set()

    @property
    def all_subtitles(self) -> list[Path]:
        return self.sub_index.all_subtitles

    @property
    def use_source_fonts(self) -> bool:
        return self.fonts.count == 0

    def has_local_subtitle(self, ep_type: str, ep: str, season: str) -> bool:
        return self.sub_index.lookup(ep_type, ep, season) is not None

    def count_unmatched_chapter_videos(self, videos: list[ParsedFile]) -> int:
        return sum(
            1 for v in videos
            if v.parsed and not self.chap_index.has_match(v.parsed[1], v.parsed[2], v.parsed[0])
        )

    @classmethod
    def discover(
        cls,
        search_dirs: list[Path],
        fonts_dir: Path,
        assets_dir: Path | None = None,
        scan_cache: dict[Path, ScanResults] | None = None,
    ) -> AssetResolver:
        if fonts_dir.is_dir():
            logger.info("Scanning detected font directory: '%s'...", fonts_dir.name)
            fonts = scan_fonts(fonts_dir)
            logger.info("Fonts found: %d", fonts.count)
            if fonts.count == 0:
                logger.info("  (fonts folder is empty — will use source fonts if available)")
        else:
            logger.info("No font directory found — will use source fonts if available.")
            fonts = FontInfo(args=(), count=0)

        sub_index = SubIndex()
        chap_index = ChapterIndex()
        cover_index = CoverIndex()

        for d in reversed(search_dirs):
            _populate_indices(scan_assets(d, scan_cache), sub_index, chap_index, cover_index)

        fallback_sub_index: SubIndex | None = None
        fallback_chap_index: ChapterIndex | None = None
        fallback_cover_index: CoverIndex | None = None

        if assets_dir is not None:
            if not assets_dir.is_dir():
                logger.warning("Specified --assets-dir '%s' does not exist — fallback asset search disabled.", assets_dir)
            else:
                fallback_sub_index = SubIndex()
                fallback_chap_index = ChapterIndex()
                fallback_cover_index = CoverIndex()
                _populate_indices(scan_assets(assets_dir, scan_cache), fallback_sub_index, fallback_chap_index, fallback_cover_index)

                if fonts.count == 0:
                    fallback_fonts_dir = assets_dir / "fonts" if (assets_dir / "fonts").is_dir() else assets_dir
                    fallback_fonts = scan_fonts(fallback_fonts_dir)
                    if fallback_fonts.count > 0:
                        fonts = fallback_fonts
                        logger.info("Detected %d external font(s) in fallback assets directory: %s", fonts.count, fallback_fonts_dir)

        logger.info("Subtitles indexed: %d primary, %d English fallback", sub_index.primary_count, sub_index.fallback_count)
        logger.info("Chapters indexed: %d by episode number, %d unmatched", chap_index.indexed_count, len(chap_index.unmatched))
        if chap_index.rejected_count:
            logger.info("  (%d .xml file(s) skipped — not chapter data)", chap_index.rejected_count)

        cover_note = " + folder-wide cover.*" if cover_index.has_global else ""
        logger.info("Covers indexed: %d by episode number%s", cover_index.indexed_count, cover_note)

        return cls(
            fonts=fonts,
            sub_index=sub_index,
            chap_index=chap_index,
            cover_index=cover_index,
            fallback_sub_index=fallback_sub_index,
            fallback_chap_index=fallback_chap_index,
            fallback_cover_index=fallback_cover_index,
        )

    def _resolve_sub(
        self,
        video: ParsedFile,
        parsed: EpisodeInfo,
        json_data: dict | None,
        all_subs: list[Path],
        dry_run: bool,
        interactive: bool = True,
    ) -> tuple[Path | None, int | None, str, bool, str | None]:
        season, ep_type, ep = parsed
        result = self.sub_index.lookup(ep_type, ep, season)
        if result is not None:
            sub_path, lang = result
            if lang in SUBTITLE_LANGS_FALLBACK:
                logger.info("  NOTE: No Indonesian sub found; using English fallback (possible mislabelled fansub).")
            logger.info("  Subtitle       : %s [%s] (matched)", sub_path.name, LANG_LABEL.get(lang, lang))
            return sub_path, None, lang, True, None

        logger.info("  No external subtitle match — checking source for embedded subtitles...")
        if json_data:
            embedded_res = select_best_subtitle_track(json_data.get("tracks", []))
            if embedded_res:
                track_id, lang, _ = embedded_res
                if lang in SUBTITLE_LANGS_FALLBACK:
                    logger.info("  NOTE: No Indonesian sub in source; using English fallback (possible mislabelled fansub).")
                logger.info("  Subtitle       : embedded track %d [%s] (from source)", track_id, LANG_LABEL.get(lang, lang))
                return None, track_id, lang, True, None

        if self.fallback_sub_index is not None:
            fb_result = self.fallback_sub_index.lookup(ep_type, ep, season)
            if fb_result is not None:
                sub_path, lang = fb_result
                if lang in SUBTITLE_LANGS_FALLBACK:
                    logger.info("  NOTE: No Indonesian sub in fallback; using English fallback.")
                logger.info("  Subtitle       : %s [%s] (fallback asset)", sub_path.name, LANG_LABEL.get(lang, lang))
                return sub_path, None, lang, True, None

        logger.warning("No subtitle match found for %s%s (external or embedded).", ep_type, ep)
        skip_reason = "no subtitle files found" if not all_subs else "subtitle files found but none matched"
        is_tty = interactive and sys.stdin.isatty()
        if not all_subs or dry_run or not is_tty:
            if not all_subs:
                logger.warning("SKIPPED: %s | Reason: %s", video.name, "No subtitle files exist in the release directory at all.")
            elif dry_run:
                logger.info("  DRY RUN: would prompt for manual subtitle selection here (skipped).")
            elif not is_tty:
                logger.warning("SKIPPED: %s | Reason: %s", video.name, "Non-interactive environment — skipped manual subtitle selection.")
            return None, None, "und", False, skip_reason

        print("  Available subtitles in release directory:")
        for i, sub in enumerate(all_subs):
            print(f"    [{i + 1}] {sub.name}")
        print("    [0] Skip this video")
        choice = input("\n  Type a number to assign a subtitle, or 0 to skip: ").strip()

        if choice.isdigit() and 0 < int(choice) <= len(all_subs):
            chosen_sub = all_subs[int(choice) - 1]
            sub_lang = detect_sub_lang(chosen_sub.stem)
            logger.info("  -> Manually linked: %s", chosen_sub.name)
            return chosen_sub, None, sub_lang, True, None

        logger.warning("SKIPPED: %s | Reason: %s", video.name, "User skipped manual subtitle selection.")
        return None, None, "und", False, "user skipped manual selection"

    def _resolve_chap(
        self,
        parsed: EpisodeInfo,
        chapter_fallback_count: int,
        has_source_chapters: bool,
    ) -> Path | None:
        season, ep_type, ep = parsed
        match = self.chap_index.lookup(ep_type, ep, season)
        if match:
            logger.info("  Chapters       : %s (matched)", match.name)
            return match

        if not has_source_chapters and self.chap_index.unmatched:
            unclaimed = [f for f in self.chap_index.unmatched if f not in self._chapter_unmatched_claimed]
            if len(unclaimed) == 1 and chapter_fallback_count == 1:
                single_match = unclaimed[0]
                self._chapter_unmatched_claimed.add(single_match)
                logger.info("  NOTE: Chapter file name had no episode number — claimed as sole leftover match.")
                logger.info("  Chapters       : %s (matched)", single_match.name)
                return single_match

        if not has_source_chapters and self.fallback_chap_index is not None:
            fb_match = self.fallback_chap_index.lookup(ep_type, ep, season)
            if fb_match:
                logger.info("  Chapters       : %s (fallback asset)", fb_match.name)
                return fb_match

        if has_source_chapters:
            logger.info("  Chapters       : using source's embedded chapters (no external match found)")
        else:
            logger.info("  Chapters       : no matching file — will check source's own chapters")
        return None

    def _resolve_cov(self, parsed: EpisodeInfo, has_source_cover: bool) -> Path | None:
        season, ep_type, ep = parsed
        cov = self.cover_index.lookup(ep_type, ep, season)
        if cov:
            logger.info("  Cover          : %s (matched)", cov.name)
            return cov

        if not has_source_cover and self.fallback_cover_index is not None:
            fb_cov = self.fallback_cover_index.lookup(ep_type, ep, season)
            if fb_cov:
                logger.info("  Cover          : %s (fallback asset)", fb_cov.name)
                return fb_cov

        if has_source_cover:
            logger.info("  Cover          : keeping source's embedded cover (no external match found)")
        else:
            logger.info("  Cover          : no external file — will check source's own cover")
        return None

    def resolve_for(
        self,
        video: ParsedFile,
        json_data: dict | None,
        all_subs: list[Path] | None = None,
        dry_run: bool = False,
        chapter_fallback_count: int = 0,
        interactive: bool = True,
    ) -> ResolvedAssets:
        parsed = video.parsed
        if not parsed:
            return ResolvedAssets(
                subtitle_file=None,
                embedded_sub_id=None,
                subtitle_lang="und",
                subtitle_label=LANG_LABEL.get("und", "und"),
                subtitle_mkv_lang=LANG_MKV_TAG.get("und", "und"),
                matched_subtitle=False,
                chapter_file=None,
                cover_file=None,
                fonts=self.fonts,
            )

        sub_file, embedded_id, sub_lang, matched, skip_reason = self._resolve_sub(
            video, parsed, json_data, all_subs or [], dry_run,
            interactive=interactive
        )
        chapter_file = self._resolve_chap(
            parsed, chapter_fallback_count, has_source_chapters=has_chapters(json_data)
        )
        cover_file = self._resolve_cov(
            parsed, has_source_cover=has_cover(json_data)
        )

        return ResolvedAssets(
            subtitle_file=sub_file,
            embedded_sub_id=embedded_id,
            subtitle_lang=sub_lang,
            subtitle_label=LANG_LABEL.get(sub_lang, sub_lang),
            subtitle_mkv_lang=LANG_MKV_TAG.get(sub_lang, sub_lang),
            matched_subtitle=matched,
            chapter_file=chapter_file,
            cover_file=cover_file,
            fonts=self.fonts,
            skip_reason=skip_reason,
        )
