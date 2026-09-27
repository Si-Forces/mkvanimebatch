# Media Engine Context

Parses, indexes, scores, probes, and executes anime fansub batch extraction and remuxing. It owns every decision about what ends up in an output file.

## Ubiquitous Language & Explicit Prohibitions

| Canonical Domain Term | Definition | Prohibited Synonyms (Explicitly Avoid) |
| :--- | :--- | :--- |
| **Release Directory** | A directory containing video files, subtitle files, chapter XMLs, cover images, and fonts belonging to one anime release. | Folder, package, album |
| **Asset Resolver** | The service that discovers, indexes, and matches external and embedded assets — subtitles, chapters, covers, fonts — to video files across a release and its ancestors. Assets beside the video take strict precedence over ancestor and fallback assets. | Asset finder, index manager, track matcher |
| **Episode Info** | The canonical parsed identity of an episode: season, Episode Type, and episode number, with fractional suffixes preserved verbatim. | Episode tag, episode string |
| **Episode Type** | The closed classification of an episode's place in a release: `Episode`, `Part`, `Ova`, or `Special`. | Kind, category, release type |
| **Batch Item Info** | The per-file record inside an Inspection Manifest describing one media file's identity, probe status, metadata, and subtitle candidates. | Manifest entry, file row, item record |
| **Subtitle Source** | The origin of the subtitle used for an item: a human Track Assignment, an external subtitle beside its video, an embedded subtitle track, the nearest matching ancestor external subtitle, or an explicit fallback external subtitle. | Subtitle candidate pool |
| **Episode Cover** | Artwork dedicated to one applicable video: `<video-stem>_cover.<ext>`, `<video-stem>.cover.<ext>`, or an exact adjacent `<video-stem>.<ext>`. An inapplicable `_cover` image is not a Directory Cover. | Series poster |
| **Directory Cover** | Artwork shared by the videos of a release directory or its descendants, named only `cover.<ext>` or `folder.<ext>`. The nearest applicable directory wins, with `cover` preceding `folder` at the same level. | Episode cover |
| **Track Selection Model** | The scoring engine that ranks audio and subtitle streams by language priority, track type (dialogue over signage, songs, or commentary), container flags, and codecs. | Audio picker, stream filter, track sorter |
| **Font Registry** | The transactional component that deduplicates fonts by content hash and promotes unique attachments into batch targets. | Font cache, attachment pool |
| **Extracted Font** | A font recovered from a media file and retained as user output. | Temporary font |
| **Batch Pipeline** | The bounded execution engine orchestrating inspection, asset extraction, audio transcoding, and remuxing. | Batch runner, job worker, task loop |
| **Inspection Manifest** | The structured document produced by preflight inspection summarizing discovered files, track metadata, and candidate external assets. | Execution plan, dry run report |
| **Batch Request** | The canonical serializable specification that configures and initiates one Batch Pipeline execution, submitted across the Engine Bridge or constructed from CLI arguments. | Job spec, pipeline config, run arguments |
| **Preset Profile** | A named bundle of audio transcoding bitrate and encoding complexity, strictly decoupled from hardware worker counts. | Config preset, setting template, encoding profile |
| **Track Assignment** | A human-specified subtitle file attached to exactly one target video, carrying a language tag, track title, and time offset. One video accepts at most one Track Assignment; multi-track stacking is not part of this model. | Sub assignment, manual override, track mapping |
| **Selected Items** | An ordered subset of canonical relative video paths designated in a Batch Request to restrict and sequence execution. When omitted, every discovered media file executes in natural directory order. | Target filter, whitelist, item subset |
| **Item Outcome** | The closed set of terminal statuses an item may reach: `completed`, `skipped`, `error`, `cancelled`, `planned`. | Status string, job state, result code |
| **Job Event** | A strongly-typed lifecycle event describing real-time execution progress: `BatchStart`, `ItemStart`, `ItemProgress`, `ItemComplete`, `BatchComplete`. | Progress message, notification, log line |
| **Item Duration** | The authoritative monotonic duration in seconds for one item, measured by the Batch Pipeline from item start to termination on a monotonic clock. Never derived from a wall clock. | Execution time, run clock, stopwatch |
| **Fallback Inspector** | The secondary probing mechanism that delegates to an external Matroska tool when native container parsing meets unhandled extensions or malformed elements. | Backup scanner, external probe |

Every term above is owned by this context. The Desktop Application consumes them across the Engine Bridge; see [CONTEXT-MAP.md](../../CONTEXT-MAP.md) for that relationship.
