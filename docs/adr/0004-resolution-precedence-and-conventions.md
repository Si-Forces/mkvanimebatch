# 0004. Resolution Precedence and Naming Conventions

> **Status**: Amended by ADR-0008
> **Date**: 2026-03-15
> **Context**: Media Engine Context

## Context
Several deliberate behaviours in asset resolution, episode naming, and
font/cover conventions keep being flagged as defects by code review. Each is
a settled design decision; recording them prevents re-litigation.

## Decision

### 1. Subtitle source precedence: local index → embedded track → fallback assets

> [!IMPORTANT]
> **AMENDED BY ADR-0008**: Explicit track assignments that cannot be resolved on disk now fail preflight fast instead of silently falling back to automatic matching.

`AssetResolver._resolve_sub` tries sources in a fixed order: an external
subtitle matched from the release directory / ancestor chain first, then the
best embedded subtitle track in the video itself, then external subtitles
from `--assets-dir` fallbacks last. An explicit human assignment (GUI/CLI)
overrides everything. Rationale: assets shipped alongside a release express
its authors' intent; embedded tracks travel with the file; fallback
directories are a recovery path of last resort. Do not reorder without a
product-level reason.

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

### 4. Stem `cover` means a directory-wide cover

A file named `cover.<ext>` (stem exactly "cover") is by convention the
poster for the whole Release Directory and is offered to every episode via
`CoverIndex.lookup`'s global fallback. Per-episode covers must use the
`<episode-stem>_cover.<ext>` pattern. This split is intentional.

### 5. Font directories resolve first-match-wins

Font selection evaluates candidate directories in most-specific-first order:
an explicit `--fonts-dir` override, then the nearest ancestor fonts
directory, then `--assets-dir` fallbacks. The first candidate containing
fonts wins (guarded by `fonts.count == 0` checks) — consistent with how
asset indices give precedence to earlier-indexed, more specific entries.
