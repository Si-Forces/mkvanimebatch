# Desktop Application Context

The desktop GUI host for anime fansub collections: it presents the queue, drives preflight inspection, and orchestrates a single batch execution. It owns presentation and user configuration, and owns no decision about what ends up in an output file.

## Ubiquitous Language & Explicit Prohibitions

| Canonical Domain Term | Definition | Prohibited Synonyms (Explicitly Avoid) |
| :--- | :--- | :--- |
| **Queue Tracker** | The frontend state machine managing batch item lifecycles (`queued` → `processing` → `completed` / `skipped` / `planned` / `error` / `cancelled`), strictly keyed by item ID. | Task list, item manager |
| **Engine Bridge** | The in-process integration layer connecting the desktop to the Media Engine's Batch Pipeline, with task-level panic isolation, dual event and log streaming, and cooperative cancellation. | Wrapper, helper, controller |
| **Job Coordinator** | The coordinator owning execution lifecycle: it takes the exclusive job slot, turns GUI input into Batch Requests, and runs both the batch and its two-phase teardown off the UI thread. | Job runner, execution manager |
| **Inspection Coordinator** | The component that deduplicates preflight directory scans: concurrent scan requests for the same path and cache setting share one in-flight scan, and a forced request always starts a fresh one. | Scan manager, folder inspector |
| **Presentation Heartbeat** | The single timer in the Queue Tracker that advances elapsed-time display across every active card in one frame, and idles when no item is in flight. | UI ticker, card timer, interval loop |
| **System Telemetry Provider** | The host probe reporting system CPU and memory usage and the disk write rate of the desktop process and its media tool child processes. It reports load only and recommends no concurrency settings. | Hardware probe, system monitor |
| **Job Snapshot** | The in-memory projection of one Active Batch — its participating items, progress, and outcomes, and the Effective Settings it runs with — retained across UI reloads. It never contains unselected Inspection Manifest items. | Job state, queue cache, execution status |
| **Card Progress** | The visual state of one active item in the Queue Tracker: indeterminate while starting up or transcoding, determinate once streamed item progress arrives. | Loading bar, queue spinner, progress gauge |
| **Queue Selection** | The user-marked subset and ordering of scanned manifest items chosen for execution. | Checked items, run list |
| **Active Batch** | The ordered Queue Selection submitted for one execution, excluding every unselected manifest item. | Entire scanned queue |
| **Item Settlement** | When a job ends, the Engine Bridge gives a terminal Item Outcome, through an ItemComplete, to every item of the Active Batch that the Batch Pipeline left open: cancelled when the job was stopped, otherwise error with the run's own failure message. Outcomes the Batch Pipeline itself gives (such as a planning failure, or an item it ends as cancelled on seeing a stop) are not Item Settlement. | Finalize, cleanup, close-out |

**Inspection Manifest**, **Preset Profile**, **Effective Settings**, and **Item Duration** are owned by the [Media Engine context](../crates/mkv-core/CONTEXT.md) and consumed here. This context deliberately holds no second definition of them — see [ADR-0008](../docs/adr/0008-typed-engine-bridge-and-track-assignment-model.md) for why one owner, not two, keeps them honest.

**Native Package** and **AppImage** are packaging formats rather than components of this context, so they are deliberately kept out of the table above. They are load-bearing here — the split between them decides which GTK/WebKitGTK stack the process loads, and therefore which rendering workarounds apply — but the vocabulary and its rationale live in [ADR-0015](../docs/adr/0015-per-package-webkitgtk-dmabuf-renderer-scoping.md).
