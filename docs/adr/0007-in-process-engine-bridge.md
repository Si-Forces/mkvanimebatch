# 0007. In-Process Engine Bridge and Sidecar Retirement

> **Status**: Accepted (Partially superseded by ADR-0008; amended 2026-09-28, 2026-10-04, 2026-10-09)
> **Date**: 2026-05-01
> **Context**: Desktop Application Bridge

## Context
Following the Rust media engine migration (ADR-0005), the desktop application (`desktop-tauri`) retained legacy sidecar subprocess execution (`mkv-cli`), requiring OS-specific process group signals (`libc::kill`, Windows `taskkill`), NDJSON parsing, and dedicated sidecar compilation scripts. This caused packaging complexity, slow CI runs, and unnecessary inter-process serialization overhead.

## Decision
We decided to cut over `desktop-tauri` entirely to an in-process **Engine Bridge** directly consuming `mkv-core::pipeline::execute_pipeline`:
1. **Compatible Adapter**: The Tauri command `run_job` retains its existing signature (`mode`, `args`, `assignments_json`) to maintain 100% compatibility with frontend state management (`job-controller.js`), but delegates CLI argument mapping directly to `mkv-core::pipeline::PipelineConfig::from_args`.

   > [!NOTE]
   > **AMENDED BY ADR-0008**: The untyped IPC signature `run_job(mode, args, assignments_json)` was superseded by strongly-typed `BatchRequest`.
2. **Dual-Channel Streaming**: Decouple structured state from diagnostic logs. The batch pipeline emits strongly-typed `JobEvent` structs over an MPSC event channel to update the `Queue Tracker`, while emitting diagnostic lines over a dedicated `Log Channel` to stream batched console lines (`"job-log"`) to the UI terminal drawer.
3. **Task-Level Panic Isolation**: Run pipeline execution within Tokio asynchronous task boundaries. Any unexpected engine panic is caught at the join boundary, preventing desktop crashes, cleanly releasing the `JobSlot`, and emitting a terminal error event to the GUI.
4. **Two-Phase Teardown**: Coordination uses `tokio_util::sync::CancellationToken` and `tauri::async_runtime::JoinHandle` (wrapping Tokio's `JoinHandle`). On cancellation or shutdown, cooperative cancellation is triggered first, allowing up to 2.0 seconds for temporary scratch directory cleanup and inspect cache persistence; if unreleased after 2.0 seconds, hard task abortion is invoked as a safety valve.
5. **Full Sidecar Retirement**: Delete `sidecar.rs`, remove `build-sidecars.*` scripts, and eliminate `externalBin` configurations from `tauri.conf.json`. `mkv-cli` is retained strictly as a standalone terminal CLI for headless scripting.

## Considered Options
- **Retain Sidecar Subprocesses**: Keep sidecar execution as a secondary runtime fallback. *Rejected*: Preserves 500+ lines of platform-specific process management code and dual-path testing burden without concrete benefit.
- **Single Channel with Log Events**: Add log message variants directly to `JobEvent`. *Rejected*: Pollutes the clean domain lifecycle transitions of the `Queue Tracker` with high-frequency console strings.
- **Typed Frontend IPC Overhaul**: Change the `run_job` frontend command signature to a typed Rust/TS struct. *Rejected*: Unnecessarily touches 400+ lines of tested JavaScript UI controller logic when an in-process adapter achieves identical engine performance.

## Consequences
- The Tauri desktop release becomes a completely self-contained single binary with zero external sidecars.
- Inter-process serialization, PID tracking, and process tree killing logic in `desktop-tauri` are eliminated.
- Single source of truth for CLI argument validation is established in `mkv-core::pipeline`.
- `mkvmerge`, `mkvextract`, and `ffmpeg` remain external binaries executed directly by `mkv-core`.

## Amendment — 2026-09-28

The entry points and validation home named above no longer exist under those names. The decision itself stands.

- **Engine entry point.** `run_job` receives a typed `BatchRequest` (ADR-0008), validates it, builds `BatchPipeline::from_request`, and awaits `BatchPipeline::run()`. There is no `mkv-core::pipeline::execute_pipeline` and no `PipelineConfig::from_args`; request-to-config conversion is `BatchRequest::into_pipeline_config`.
- **Validation home.** The single source of request validation is `BatchRequest::validate` in `mkv-core::request`, not `mkv-core::pipeline`. The CLI still rejects unknown flags and checks `--preset` and `--bitrate` while parsing, before `validate` runs.
- **Log destination.** `job-log` batches are shown in each view's log console panel; there is no drawer.
- **Panic isolation has two layers.** A panic inside one worker task is caught by the pipeline's worker `JoinSet` and completes that item as `error` (`worker panicked: ...`) while the batch continues. A panic that escapes the pipeline task is still caught at the bridge's join boundary as Decision 3 describes. Both depend on the release profile unwinding: `[profile.release]` must never set `panic = "abort"`.

## Amendment — 2026-10-04

- **Desktop entry point (#27).** `run_job` no longer calls `BatchPipeline::from_request`. It converts the request with `BatchRequest::into_pipeline_config`, sets `PipelineConfig.cache` to the app session's shared inspect cache (unless the request asks for no cache), and builds the pipeline with `BatchPipeline::new`. `from_request` performs the same two steps without a cache and remains the CLI's entry point. The decision itself stands.
- **Teardown thread (#26).** Decision 4's two phases are synchronous waits, and they must run neither on the main thread nor on a Tokio worker. `stop_job` and `close_window` are async commands that run the teardown on Tokio's blocking pool (`run_teardown`); the `ExitRequested` handler runs it on its own thread. A sync command would freeze the window for up to about 3 s. A plain async command would park a Tokio worker that the supervisor it waits for also needs. `close_window` closes starts (`begin_shutdown`) before handing the wait to the pool; unlike the old sync command, it does so one async hop after dispatch, so a `run_job` landing in that hop starts and is then cancelled. The decision itself stands.

  **Coverage.** `test_teardown_leaves_the_calling_runtime_free_while_it_waits` proves the calling runtime keeps running during the wait; `close_during_a_pending_stop_settles_the_job_once` runs a Stop and a Close against one slot. No test covers `close_window` itself, including its `begin_shutdown` ordering: that needs a real window.

## Amendment — 2026-10-09

- **Engine Diagnostics (#79).** Decision 2's two channels are not the only output. **Engine Diagnostics** (see the Media Engine glossary) are a third stream beside Job Events and the Log Channel: the messages `mkv-core` raises through `tracing` about its own operation. `mkv-core` depends only on `tracing`; each binary installs its own subscriber.
- **Where it goes.** `mkv-cli` writes to stderr, so stdout stays reserved for `--plan` JSON, `--events` NDJSON and the summary. The desktop writes to `mkv-anime-batch.log` in Tauri's app log directory and to stderr. The default level is `warn`; `RUST_LOG` overrides it in both. The desktop empties the file at startup when it is over 1 MB and does not rotate it. If the file cannot be opened, the desktop logs to stderr only and still starts. A packaged Windows build has no console, so there the log file is the only output, and losing it loses Engine Diagnostics until the next start.
- **Rule.** An outcome that changes what a run produces is reported on the Log Channel, never only as an Engine Diagnostic. Some current messages break it; follow-up #81 moves them. The decision itself stands.
