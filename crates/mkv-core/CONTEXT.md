# Media Engine Context

Parses, indexes, scores, probes, and executes anime fansub batch extraction and remuxing. It owns every decision about what ends up in an output file.

## Ubiquitous Language & Explicit Prohibitions

| Canonical Domain Term | Definition | Prohibited Synonyms (Explicitly Avoid) |
| :--- | :--- | :--- |
| **Release Directory** | A directory containing video files, subtitle files, chapter XMLs, cover images, and fonts belonging to one anime release. | Folder, package, album |
| **Asset Resolver** | The service that discovers, indexes, and matches external and embedded assets — subtitles, chapters, covers — to video files across a release and its ancestors. It does not choose a release's fonts directory. Assets beside the video take strict precedence over ancestor and fallback assets. | Asset finder, index manager, track matcher |
| **Episode Info** | The canonical parsed identity of an episode: season, Episode Type, and episode number, with fractional suffixes preserved verbatim. | Episode tag, episode string |
| **Episode Type** | The closed classification of an episode's place in a release: `Episode`, `Part`, `Ova`, or `Special`. | Kind, category, release type |
| **Batch Item Info** | The identity of one item participating in a batch — its canonical item ID and file name — as listed in the `BatchStart` Job Event. | File row, item record |
| **Subtitle Source** | The origin of the subtitle used for an item: a human Track Assignment, an external subtitle beside its video, an embedded subtitle track, the nearest matching ancestor external subtitle, or an explicit fallback external subtitle. | Subtitle candidate pool |
| **Episode Cover** | Artwork dedicated to one applicable video: `<video-stem>_cover.<ext>`, `<video-stem>.cover.<ext>`, or an exact adjacent `<video-stem>.<ext>`. An inapplicable `_cover` image is not a Directory Cover. | Series poster |
| **Directory Cover** | Artwork shared by the videos of a release directory or its descendants, named only `cover.<ext>` or `folder.<ext>`. The nearest applicable directory wins, with `cover` preceding `folder` at the same level. | Episode cover |
| **Track Selection Model** | The scoring engine that ranks embedded tracks. Subtitles rank by language priority and ASS codec, with dialogue-named tracks winning ties; audio ranks by language, channel layout, dialogue naming, and stream position, with commentary tracks considered only when every track is commentary. | Audio picker, stream filter, track sorter |
| **Font Registry** | The transactional component that deduplicates fonts by content hash and promotes unique attachments into batch targets. Fonts already present in a target before the batch count as claimed, so an incoming font matching one by name and size, or by content, is not added again. | Font cache, attachment pool |
| **Extracted Font** | A font recovered from a media file and retained as user output. | Temporary font |
| **Batch Pipeline** | The bounded execution engine orchestrating inspection, asset extraction, audio transcoding, and remuxing. | Batch runner, job worker, task loop |
| **Inspection Manifest** | The structured document produced by preflight inspection: one entry per discovered media file with its identity, release directory, track metadata, and any probe error, plus the external subtitle candidates of each release directory. | Execution plan, dry run report |
| **Batch Request** | The canonical serializable specification that configures and initiates one Batch Pipeline execution, submitted across the Engine Bridge or constructed from CLI arguments. | Job spec, pipeline config, run arguments |
| **Preset Profile** | A named bundle of audio transcoding bitrate and encoding complexity, strictly decoupled from hardware worker counts. | Config preset, setting template, encoding profile |
| **Effective Settings** | The concurrency and audio transcoding values a Batch Pipeline actually runs with — worker count, I/O job limit, decoder threads, bitrate, and complexity — after a Batch Request's explicit values are layered over its Preset Profile and hardware-derived defaults. | Pipeline config, resolved config, runtime options |
| **Track Assignment** | A human-specified subtitle file attached to exactly one target video, optionally carrying a language tag, track title, and time offset. One video accepts at most one Track Assignment; multi-track stacking is not part of this model. | Sub assignment, manual override, track mapping |
| **Selected Items** | An ordered subset of canonical relative video paths designated in a Batch Request to restrict and sequence execution. When omitted, every discovered media file executes in natural directory order. | Target filter, whitelist, item subset |
| **Item Outcome** | The closed set of terminal statuses an item may reach: `completed`, `skipped`, `error`, `cancelled`, `planned`. | Status string, job state, result code |
| **Job Event** | A strongly-typed lifecycle event describing real-time execution progress: `BatchStart`, `ItemStart`, `ItemProgress`, `ItemComplete`, `BatchComplete`. | Progress message, notification, log line |
| **Item Duration** | The authoritative monotonic duration in seconds for one item, measured by the Batch Pipeline from item start to termination on a monotonic clock. Never derived from a wall clock. | Execution time, run clock, stopwatch |
| **Fallback Inspector** | The secondary probing mechanism that delegates to an external Matroska tool (`mkvmerge -J`) whenever native container parsing fails. | Backup scanner, external probe |

Every term above is owned by this context. The Desktop Application consumes them across the Engine Bridge; see [CONTEXT-MAP.md](../../CONTEXT-MAP.md) for that relationship.
