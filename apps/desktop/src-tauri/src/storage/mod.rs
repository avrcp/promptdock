//! Transaction-scoped persistence for immutable agent events and their projections.
//!
//! Repositories in this module deliberately accept an existing SQLite transaction.
//! The event ingestor owns the only commit boundary so an event fact, every derived
//! projection, notification changes, and the source checkpoint succeed or roll back
//! together.

pub(crate) mod activity_projection;
pub(crate) mod activity_repository;
mod event_store;
mod output_repository;
pub(crate) mod retention;
mod run_repository;
mod source_repository;

pub(crate) use event_store::{EventAppendResult, EventStore};
pub(crate) use output_repository::{OutputRepository, StoredResultContent};
pub(crate) use run_repository::{AgentRunRecord, RunRepository};
pub(crate) use source_repository::SourceRepository;
