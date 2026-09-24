# Media Engine Context

Domain logic for parsing, indexing, scoring, probing, and executing anime fansub batch extraction and remuxing.

## Boundaries & Responsibilities

- **Parsing**: Fast in-memory parsing of Matroska/EBML container headers, track entries, and attachments via native EBML inspection, with transparent fallback to `mkvmerge -J` when container anomalies, malformed headers, or unsupported elements are encountered.
- **Indexing**: Structural inspection and recursive indexing of release directories, mapping video files, external subtitle tracks, XML chapter definitions, front cover artwork, and font attachments across ancestor directory hierarchies.
- **Scoring**: Rule-based heuristic evaluation and ranking of audio and subtitle streams based on language preference (Indonesian dialogue > English dialogue), format types (ASS typesetting > SRT), track roles (dialogue vs signs/songs vs commentary), and audio channel configurations.
- **Font Deduplication**: Transactional extraction, content-hashing (BLAKE2b), and staging of embedded fonts to prevent duplicate attachments and eliminate disk I/O contention during concurrent batch processing.
- **Atomic Muxing**: Coordinated execution of `mkvmerge` command lines with temporary file isolation, rollback protection, atomic file promotion with cross-filesystem (`EXDEV`) fallback, and automatic quarantine of incomplete or interrupted outputs.

## Domain Invariants

- **Precedence Ladder**:
  - *Subtitle resolution*: A human `TrackAssignment` takes precedence over an exact same-directory external subtitle, then an embedded track, then the nearest matching ancestor external subtitle, then an explicit `--assets-dir` fallback. Source precedence strictly beats language score across sources (e.g. an adjacent English subtitle beats an embedded Indonesian track, but a parent English subtitle cannot displace an embedded Indonesian subtitle; nearest matching ancestor outranks explicit `--assets-dir`).
  - *Cover art resolution*: The nearest applicable directory wins before cover kind. Within a directory, candidates rank: episode `_cover` (`<video-stem>_cover.<ext>`) > `.cover` (`<video-stem>.cover.<ext>`) > exact adjacent bare stem (`<video-stem>.<ext>`) > directory `cover` (`cover.<ext>`) > directory `folder` (`folder.<ext>`). Equal tiers break ties using natural filename order then full filename. Inapplicable `*_cover` files are never treated as directory covers. Local and ancestor external covers supersede embedded artwork; explicit `--assets-dir` covers apply only when embedded art is absent.
  - *Audio track scoring*: Japanese dialogue tracks take strict precedence over foreign audio tracks, and dialogue tracks strictly supersede secondary commentary/audio-description (`ad`) tracks.
- **Fail-Fast Preflight Validation**:
  - Every `BatchRequest` is validated synchronously before starting pipeline execution.
  - If any explicit `TrackAssignment` points to a non-existent file on disk, references an unselected or non-existent video, or violates path traversal constraints (`..` or absolute paths outside boundaries), preflight validation fails immediately with an error. No silent fallback to automatic track matching is permitted (ADR-0008).
  - A non-existent input directory, an existing regular file at `out_dir`, worker counts less than 1, or unparseable audio bitrates fail preflight before execution locks are acquired. A non-existent `out_dir` is valid and is created only when execution writes output; planning and dry-run do not create media output directories.
- **Dual-Governor Concurrency**:
  - Worker tasks execute across a unified global `tokio::task::JoinSet` bounded by `workers` semaphore permits.
  - CPU-bound operations (such as FFmpeg Opus audio transcoding) run within the worker slot, while disk-bound remux multiplexing (`mkvmerge`) is separately throttled by `io_semaphore` permits (defaulting to `io_jobs`). This prevents disk I/O thrashing and uncontrolled scratch disk space accumulation (ADR-0011).
- **Transactional Font Deduplication**:
  - Identical fonts shared across episodes are deduplicated via BLAKE2b content hashes.
  - Font extraction follows a two-phase protocol (claim → stage → promote) arbitrated by `FontRegistry`.
  - A persistent `FontHashMemo` caches font file signatures and content hashes to eliminate redundant cryptographic hashing of existing directory fonts across batch runs (ADR-0003).
  - An RAII `FontClaimsGuard` holds uncommitted font claims during task execution. If a task fails or cancellation occurs, uncommitted claims are cleanly defused and released, preventing corrupt destination files or deadlocked claim states (ADR-0003).
- **Mode-Specific Font Handling**:
  - Extract mode always retains extracted fonts as user output in the target destination directory.
  - Remux mode discovers and embeds fonts into output Matroska files as container attachments.
  - Staging directories (`.font_staging/`) are transactionally cleaned up without deleting pre-existing or user-supplied font directories. The inert `keep_fonts` setting has been removed across all request and configuration types.
- **Monotonic Item Duration**:
  - Wall-clock item execution duration is authoritatively measured by the media engine using monotonic time (`std::time::Instant`) from task start to termination.
  - The measured elapsed duration in seconds is attached as `duration_sec: Option<f64>` on `JobEvent::ItemComplete` (or `None` if skipped/planned without running). Duration is never guessed or calculated using non-monotonic wall clocks (ADR-0009).
- **Atomic File Promotion with EXDEV Fallback**:
  - Remux and extraction outputs are written to temporary scratch files (`.tmp.*`) in staging or destination directories.
  - Successful outputs are atomically promoted to the final destination filename using standard filesystem renaming (`fs::rename`).
  - If renaming fails due to cross-device link boundaries (`EXDEV`), the engine falls back to an atomic copy-then-remove sequence with cleanup.
  - Interrupted or failed outputs are quarantined and deleted immediately, ensuring no partial or corrupt media files remain in output directories.

- **Asynchronous Reactor Isolation**:
  - Container probing (`probe_video` and `get_or_probe_video`) executes asynchronously across all pipeline stages.
  - Fallback inspection to `mkvmerge -J` uses asynchronous process execution (`tokio::process::Command`), preventing thread starvation on Tokio async runtime worker pools (ADR-0013).
- **Dynamic Transcode Thread Governance**:
  - Audio transcoding algorithmic complexity defaults to `5` for general and balanced remuxing, saving ~40% CPU time per episode compared to maximum complexity 10.
  - Transcode thread concurrency is dynamically derived as `(total_physical_cores / workers).clamp(1, 4)` when unspecified by the caller, preventing CPU core oversubscription across parallel worker slots (ADR-0013).
- **Fine-Grained Font Seeding**:
  - Initial font discovery and BLAKE2b cryptographic hashing in `FontRegistry::seed_existing` are executed outside the registry state lock.
  - The state lock is acquired exclusively during final batch insertion, reducing mutex hold duration from hundreds of milliseconds to under 15 microseconds and eliminating contention against concurrent worker claims (ADR-0013).
- **Zero-Copy Cache Persistence**:
  - `PersistentInspectCache` serializes disk snapshots directly via `std::io::BufWriter` and `serde_json::to_writer`, eliminating temporary heap string allocations and intermediate collection copies (ADR-0013).
## Core Domain Types & Aggregates

- **`ReleaseDirectory`**: Aggregates an anime release folder structure, grouping video files, external subtitle tracks, chapter XML files, cover images, and fonts belonging to a batch.
- **`AssetResolver`**: Domain service responsible for discovering, indexing, and matching candidate external and embedded assets (subtitles, chapters, covers, fonts) to video files according to the Precedence Ladder.
- **`EpisodeInfo`**: Value object capturing the canonical parsed identity of an episode, including season number, episode type (`EpisodeType`), and episode number string with fractional suffix support.
- **`EpisodeType`**: Closed enumeration classifying anime release episodes: `Episode`, `Part`, `Ova`, `Special`.
- **`TrackSelectionModel`**: Heuristic scoring engine evaluating audio and subtitle streams based on language priority, track type (dialogue vs signage/songs/commentary), container flags, and codecs.
- **`FontRegistry`**: Transactional aggregate root managing thread-safe font claims, BLAKE2b deduplication, temporary staging directories, and atomic destination promotion.
- **`FontHashMemo`**: Thread-safe, disk-backed cache storing BLAKE2b-512 content hashes keyed by file path, size, and modification timestamp to bypass redundant font hashing across extraction runs.
- **`BatchPipeline`**: Root execution aggregate coordinating end-to-end batch processing, dual-governor concurrency semaphores, preflight inspection, asset resolution, audio transcoding, and remuxing.
- **`InspectionManifest`**: Structured immutable document produced by preflight inspection summarizing discovered video files, track metadata, container properties, and candidate external assets.
- **`BatchItemInfo`**: Value object within an `InspectionManifest` describing an individual media file, its canonical root-relative ID, probe status, metadata, and subtitle candidate paths.
- **`BatchRequest`**: Canonical serializable specification submitted across the Engine Bridge or constructed from CLI arguments configuring batch operations (input/output paths, preset, assignments, selected items, workers, io_jobs).
- **`PresetProfile`**: Domain value entity defining an audio encoding quality preset (bitrate and Opus complexity), strictly decoupled from hardware worker counts.
- **`TrackAssignment`**: Entity defining an explicit track override for a specific video, carrying relative video path, asset path, track type, custom language tag, custom title, and millisecond time offset (`offset_ms`).
- **`PlannedJob`**: Internal execution aggregate capturing all resolved parameters, source tracks, external asset paths, and audio transcode settings for an individual episode remux or extraction job.
- **`PersistentInspectCache`**: Thread-safe, disk-backed cache storing EBML/mkvmerge probe results keyed by file path, size, and modification time to bypass redundant disk probing across runs.

## Ubiquitous Language & Explicit Prohibitions

| Canonical Domain Term | Definition | Prohibited Synonyms (Explicitly Avoid) |
| :--- | :--- | :--- |
| **Release Directory** | A directory containing video files, subtitle files, chapter XMLs, cover images, and fonts belonging to an anime release. | Folder, package, album |
| **Asset Resolver** | The domain service responsible for discovering, indexing, and matching external and embedded assets (subtitles, chapters, covers, fonts) to video files across ancestor directories. Local assets in the same release directory next to the MKV file take strict precedence over ancestor and fallback assets. | Asset finder, index manager, track matcher |
| **Episode Info** | The canonical parsed identity of an episode comprising season, episode type (Episode, Part, OVA, Special), and episode number. | Episode tag, episode string |
| **Subtitle Source** | A human Track Assignment, an external subtitle beside its video, an embedded subtitle track, an ancestor external subtitle, or an explicit fallback external subtitle. | Subtitle candidate pool |
| **Episode Cover** | Artwork dedicated to an applicable video: `<video-stem>_cover.<ext>`, `<video-stem>.cover.<ext>`, or an exact adjacent `<video-stem>.<ext>`. An inapplicable `_cover` image is not a Directory Cover. | Series poster |
| **Directory Cover** | Artwork shared by videos in a release directory or its descendants, named only `cover.<ext>` or `folder.<ext>`. The nearest applicable directory wins, with `cover` preceding `folder` at the same level. | Episode cover |
| **Track Selection Model** | The heuristic scoring engine evaluating audio and subtitle streams based on language priority, track type (dialogue vs signage/commentary), container flags, and codecs. | Audio picker, stream filter, track sorter |
| **Font Registry** | The transactional component managing font deduplication by content hash and promoting unique font attachments into batch targets. | Font cache, attachment pool |
| **Extracted Font** | A font extracted from a media file and retained as user output. | Temporary font |
| **Batch Pipeline** | The bounded execution engine orchestrating media inspection, asset extraction, audio transcoding, and MKV remuxing. | Batch runner, job worker, task loop |
| **Inspection Manifest** | The structured document produced during preflight inspection summarizing discovered files, track metadata, and candidate external assets. | Execution plan, dry run report |
| **Item Outcome** | The closed set of terminal statuses a batch item may reach: `completed`, `skipped`, `error`, `cancelled`, `planned`. | Status string, job state, result code |
| **Job Event** | A strongly-typed lifecycle event emitted across an in-process channel describing real-time execution progress (`BatchStart`, `ItemStart`, `ItemProgress`, `ItemComplete`, `BatchComplete`). | Progress message, notification, log line |
| **Item Duration** | The authoritative wall-clock execution duration in seconds measured by the Batch Pipeline from item execution start to completion and attached to `ItemComplete` events. | Execution time, run clock, process timing |
| **Fallback Inspector** | The secondary probing mechanism that delegates to `mkvmerge -J` when native EBML parsing encounters unhandled container extensions or malformed elements. | Backup scanner, external probe |
| **Log Channel** | The dedicated auxiliary channel streaming real-time diagnostic console text from the media engine to frontend log observers. | Output stream, log buffer, console pipe |
| **Batch Request** | The canonical serializable specification submitted across the Engine Bridge or constructed from CLI arguments to configure and initiate a Batch Pipeline execution. | Job spec, pipeline config, run arguments |
| **Preset Profile** | A canonical named bundle of audio transcoding bitrate and encoder complexity evaluated by the Media Engine for batch remuxing, decoupled from hardware worker concurrency. | Config preset, setting template, encoding profile |
| **Track Assignment** | An explicit mapping attaching an external or embedded asset to a specific target video file with designated track type, language tag, and time offset. While the domain model anticipates multi-track stacking, current pipeline execution enforces 1:1 subtitle overrides (`track_type: "subtitle"`). | Sub assignment, manual override, track mapping |
| **Selected Items** | An ordered subset of canonical relative video paths or item IDs designated in a Batch Request to restrict and sequence pipeline execution. When omitted, all discovered media files execute in natural directory order. | Target filter, whitelist, item subset |
| **Transcode Governor** | The dynamic calculation mechanism deriving FFmpeg worker thread concurrency from physical CPU core topology and active batch workers (`(total_physical_cores / workers).clamp(1, 4)`). | Thread scaler, core allocator |
| **Zero-Copy Cache Persistence** | Direct-to-disk buffered streaming serialization of persistent inspection cache entries bypassing heap string allocations. | JSON dumper, cache writer |
| **Font Hash Memo** | A disk-backed memoization store recording BLAKE2b font hashes and file signatures to eliminate redundant font hashing across batch runs. | Font hash cache, memo table, font db |
