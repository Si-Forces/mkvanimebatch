# MKV Anime Batch

Automated batch remuxing and asset extraction pipeline tailored for anime fansub releases.

## Language

**Release Directory**:
A directory containing video files, subtitle files, chapter XMLs, cover images, and fonts belonging to an anime release.
_Avoid_: Folder, package, album

**Asset Resolver**:
The module (`mkv_assets.py`) responsible for discovering, indexing, and matching external and embedded assets (subtitles, chapters, covers, fonts) to video files across ancestor directories. Local assets in the same release directory next to the MKV file take strict precedence over ancestor and fallback assets.
_Avoid_: Asset finder, index manager, track matcher

**Episode Info**:
The canonical parsed identity of an episode comprising season, episode type (Episode, Part, OVA, Special), and episode number.
_Avoid_: Episode tag, episode string

**Sidecar Contract**:
The declared, versioned interface (command-line arguments, inspection manifest JSON via `--plan`, and `@event:` progress streams via `--events`) between the desktop GUI and the Python CLI engine.
_Avoid_: IPC, subprocess pipe, wrapper

**Inspection Manifest**:
The structured JSON document produced by `mkv-remux --plan --input-dir <dir>` summarizing discovered files and metadata for frontend queue hydration.
_Avoid_: Execution plan, dry run report

**Queue Tracker**:
The frontend state machine (`queue-state.js`) managing batch item lifecycles (`queued` → `processing` → `completed` / `skipped` / `planned` / `error`) strictly keyed by item ID.
_Avoid_: Task list, item manager
