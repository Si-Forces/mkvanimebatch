# 0004. Resolution Precedence and Naming Conventions

> **Status**: Accepted; amended by ADR-0008 and revised 2026-09-24 (implemented)
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
