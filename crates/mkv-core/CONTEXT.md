# Media Engine

Domain logic for parsing, indexing, scoring, probing, and executing anime fansub batch extraction and remuxing.

## Language

**Release Directory**:
A directory containing video files, subtitle files, chapter XMLs, cover images, and fonts belonging to an anime release.
_Avoid_: Folder, package, album

**Asset Resolver**:
The domain service responsible for discovering, indexing, and matching external and embedded assets (subtitles, chapters, covers, fonts) to video files across ancestor directories. Local assets in the same release directory next to the MKV file take strict precedence over ancestor and fallback assets.
_Avoid_: Asset finder, index manager, track matcher

**Episode Info**:
The canonical parsed identity of an episode comprising season, episode type (Episode, Part, OVA, Special), and episode number.
_Avoid_: Episode tag, episode string

**Track Selection Model**:
The heuristic scoring engine evaluating audio and subtitle streams based on language priority, track type (dialogue vs signage/commentary), container flags, and codecs.
_Avoid_: Audio picker, stream filter, track sorter

**Font Registry**:
The transactional component managing font deduplication by content hash and promoting unique font attachments into batch targets.
_Avoid_: Font cache, attachment pool

**Batch Pipeline**:
The bounded execution engine orchestrating media inspection, asset extraction, audio transcoding, and MKV remuxing.
_Avoid_: Batch runner, job worker, task loop

**Inspection Manifest**:
The structured document produced during preflight inspection summarizing discovered files, track metadata, and candidate external assets.
_Avoid_: Execution plan, dry run report

**Item Outcome**:
The closed set of terminal statuses a batch item may reach: `completed`, `skipped`, `error`, `cancelled`, `planned`.
_Avoid_: Status string, job state, result code

**Job Event**:
A strongly-typed lifecycle event emitted across an in-process channel describing real-time execution progress (`BatchStart`, `ItemStart`, `ItemProgress`, `ItemComplete`, `BatchComplete`).
_Avoid_: Progress message, notification, log line

**Item Duration**:
The authoritative wall-clock execution duration in seconds measured by the Batch Pipeline from item execution start to completion and attached to `ItemComplete` events.
_Avoid_: Execution time, run clock, process timing

**Fallback Inspector**:
The secondary probing mechanism that delegates to `mkvmerge -J` when native EBML parsing encounters unhandled container extensions or malformed elements.
_Avoid_: Backup scanner, external probe

**Log Channel**:
The dedicated auxiliary channel streaming real-time diagnostic console text from the media engine to frontend log observers.
_Avoid_: Output stream, log buffer, console pipe

**Batch Request**:
The canonical serializable specification submitted across the Engine Bridge or constructed from CLI arguments to configure and initiate a Batch Pipeline execution.
_Avoid_: Job spec, pipeline config, run arguments
**Preset Profile**:
A canonical named bundle of audio transcoding bitrate, encoder complexity, and container retention flags evaluated by the Media Engine for batch remuxing, decoupled from hardware worker concurrency.
_Avoid_: Config preset, setting template, encoding profile

**Track Assignment**:
An explicit mapping attaching an external or embedded asset to a specific target video file with designated track type, language tag, and time offset. While the domain model anticipates multi-track stacking, current pipeline execution enforces 1:1 subtitle overrides (`track_type: "subtitle"`).
_Avoid_: Sub assignment, manual override, track mapping
