# 0004. Resolution Precedence and Naming Conventions

> **Status**: Accepted; amended by ADR-0008, revised 2026-09-24 and amended 2026-09-28 and 2026-10-09 (implemented)
> **Date**: 2026-03-15
> **Context**: Media Engine Context

## Context
Several deliberate behaviours in asset resolution, episode naming, and
font/cover conventions keep being flagged as defects by code review. Each is
a settled design decision; recording them prevents re-litigation.

## Decision

### 1. Subtitle sources: manual → adjacent external → embedded → ancestor external → explicit fallback

> [!IMPORTANT]
> **AMENDED BY ADR-0008**: A missing explicit track assignment fails both preflight validation and later resolution; it never falls back to automatic matching.

A human Track Assignment takes precedence over a matching external subtitle
beside the video. An embedded subtitle outranks a matching subtitle in a parent
directory; the nearest matching ancestor outranks an explicit `--assets-dir`
fallback. Source precedence wins before language scoring across sources: an
adjacent English subtitle beats an embedded Indonesian track, but a parent
English subtitle does not. This revises the former ancestor-before-embedded
rule: adjacent assets express a direct choice for that video, whereas an
embedded track is more specific than a directory-wide ancestor asset.

### 2. Episode-number fractional suffixes pass through unvalidated

`mkv_domain.format_ep` zero-pads only the integer part and copies everything
after the dot verbatim (`01.5`, `00.CM`, `02.5`). Fansub numbering schemes
use arbitrary fractional/suffix forms that must survive unchanged into
output filenames and lookups. Only the integer half may be validated.

### 3. Commentary detection matches the standalone word "ad"

`_COMMENTARY_RE` includes `\bad\b` (word-bounded, case-insensitive) to catch
audio-description tracks labelled "AD". Word boundaries prevent matches
inside larger words ("Advent", "Mad"). Known trade-off: non-English track
names using "ad" as an ordinary word would be misclassified; no real-world
case has been observed. Change requires a concrete misclassification.

### 4. Cover names and resolution

Only `cover.<ext>` and `folder.<ext>` are directory-wide covers. An Episode
Cover may use `<video-stem>_cover.<ext>`, `<video-stem>.cover.<ext>`, or
`<video-stem>.<ext>`; the bare-stem form requires an adjacent video with that
exact stem. An inapplicable `*_cover` image is never reclassified as a
directory-wide cover.

The nearest applicable directory wins before cover-name specificity. Within
one directory, episode covers rank `_cover` before `.cover` before an exact
bare stem, then directory covers rank `cover` before `folder`. Equal-tier
candidates use natural filename order and then full filename to break ties.
External covers in the video directory or an ancestor directory supersede
embedded artwork; an explicit `--assets-dir` cover is only a fallback when
there is no embedded cover. This replaces the former `cover`/`_cover`-only
rule while excluding unrelated images from episode matching.

### 5. Font directories resolve first-match-wins

Font selection evaluates candidate directories in most-specific-first order:
an explicit `--fonts-dir` override, then the nearest ancestor fonts
directory, then `--assets-dir` fallbacks. The first candidate containing
fonts wins (guarded by `fonts.count == 0` checks) — consistent with how
asset indices give precedence to earlier-indexed, more specific entries.

## Amendment — 2026-09-24

This section exists because the status line above advertised a revision the body did not carry, and because **Sections 1 and 4 were rewritten in place** to their post-revision text — which predates this repository's convention that an ADR body is never edited after ratification. The superseded rules are recorded here so the reversal is not lost.

**Section 1 — subtitle sources.** The former rule ranked a nearest-ancestor external subtitle *above* an embedded track. Reversed: an embedded subtitle outranks a matching subtitle in a parent directory. Adjacent assets express a direct choice for that video, whereas an embedded track is more specific than a directory-wide ancestor asset. Source precedence still wins before language scoring — a parent English subtitle does not displace an embedded Indonesian track.

**Section 4 — cover names.** The former rule accepted only `cover.<ext>` and `_cover.<ext>`. Replaced by the three episode-cover forms and the two directory-cover forms now in the body, with nearest-directory precedence resolved *before* name specificity, natural-then-full-filename tie-breaking, and an exact-sibling requirement for the bare-stem form. An inapplicable `*_cover` image is never reclassified as a directory-wide cover.

Both revisions are implemented. The ordering stated in the body is shipped behaviour, not an intention.

## Amendment — 2026-09-28

**Section 2 — fractional suffixes are narrower than stated.** The Rust `domain::format_ep` still zero-pads the integer part and copies the fractional part verbatim, but `parse_episode` only captures a fraction of one or two digits (`01.5`, `02.25`), and it first replaces every `.` not between two digits with a space. A non-numeric suffix such as `00.CM` therefore never reaches `format_ep`; `.CM` is lost before parsing. The rule "only the integer half may be validated" holds for numeric fractions only.

**Section 3 — symbol name.** The regex is `domain::COMMENTARY_RE` in Rust, still case-insensitive and word-bounded, still matching a standalone `ad`. It applies to audio selection only; subtitle scoring does not consult it.

## Amendment — 2026-10-09 (exact season before open season)

**Seasons — a known season ranks above an unknown one within a directory.** Subtitles, chapters and episode covers all filter candidates with `seasons_match`, where an unknown season (`""`) on either side matches any season. Nothing ranked an exact match above such an open match, so other keys decided (chapters: index order; subtitles: category score and shorter file name; covers: cover kind): a season-2 video beside `Show - 01.xml` and `Show - S02E01.xml` got the unseasoned file and never used the exact one. Now, within one directory, an **exact** match (both seasons known and equal) beats an **open** one (either season unknown). Section 4's within-directory cover order (`_cover` before `.cover` before an exact bare stem) now applies within each of those two groups. The directory ladder is unchanged and ranks first, so a nearer open candidate still beats a farther exact one, and for subtitles source tier and the embedded track still precede it.

- **Subtitles:** nearest directory, then exact before open, then category score, dialogue keyword, shorter file name and index order. The season is part of the episode's identity, whereas the category score is a preference, so exactness ranks above it.
- **Chapters:** nearest directory, then exact before open, then index order. Every chapter file of an episode is kept in the index; a file of another known season is never a candidate.
- **Episode covers:** nearest directory, then exact before open (a directory cover and an exact-stem cover count as open), then kind, natural file name and full path.

The Log Channel warning for several chapter files of one episode now names the same-directory files that match the video's season but were not chosen, and a duplicate in another directory is no longer logged at all, being the ladder working as designed. A season-2 video can therefore get a different subtitle, chapter file or cover than before.
