//! Batch pipeline execution, event streams, and cancellation coordination.

pub use crate::domain::ItemOutcome;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
pub use tokio_util::sync::CancellationToken;

pub const EVENT_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEvent {
    BatchStart {
        version: u64,
        total: usize,
        input_dir: String,
    },
    ItemStart {
        version: u64,
        id: String,
        file: String,
    },
    ItemComplete {
        version: u64,
        id: String,
        file: String,
        status: ItemOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    BatchComplete {
        version: u64,
        success: bool,
        #[serde(default)]
        cancelled: bool,
    },
}

impl JobEvent {
    pub fn to_ndjson(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        Ok(format!("@event: {json}"))
    }
}

pub type EventSender = mpsc::Sender<JobEvent>;
pub type EventReceiver = mpsc::Receiver<JobEvent>;

pub fn create_event_channel(buffer_size: usize) -> (EventSender, EventReceiver) {
    mpsc::channel(buffer_size)
}
