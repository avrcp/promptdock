use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::Emitter;

use crate::agent::{AgentEventEnvelopeV2, AGENT_EVENT_SCHEMA_VERSION};
use crate::db::{now_ms, Db};
use crate::error::AppError;
use crate::inbox::{append_error, InboxPaths};
use crate::model::SourceCheckpointUpdate;
use crate::source::SourceDescriptor;

mod cursor;
mod emitter;
mod errors;
mod lifecycle;
mod maintenance;
mod source;
mod worker;

use cursor::*;
use emitter::*;
use errors::*;
use lifecycle::*;
use maintenance::*;
use source::*;
use worker::*;

pub use lifecycle::{DurableEventRuntime, RuntimeShutdownReport};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AgentEventProcessingOutcome;

/// Durable boundary for the retained Codex event stream. Implementations commit
/// the projection, notification rows and checkpoint in one transaction.
pub(crate) trait AgentEventProcessor: Send + Sync + 'static {
    fn process(
        &self,
        envelope: AgentEventEnvelopeV2,
        checkpoint: &SourceCheckpointUpdate,
    ) -> Result<AgentEventProcessingOutcome, AppError>;

    fn checkpoint_rejection(
        &self,
        rejection_code: &'static str,
        checkpoint: &SourceCheckpointUpdate,
    ) -> Result<(), AppError>;
}
