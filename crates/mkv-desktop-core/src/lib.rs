//! The toolkit-agnostic Desktop Core (ADR-0016): the Engine Bridge that runs `mkv-core` jobs for
//! a desktop UI, with no dependency on any UI toolkit. A UI adapter supplies the tokio runtime,
//! a `Publisher` and an `Inspector`, spawns the actor, and wraps these calls in its own commands.

pub mod actor;
pub mod control;
pub mod draft;
pub mod inspection;
pub mod job;
pub mod queue;
pub mod snapshot;
pub mod telemetry;
