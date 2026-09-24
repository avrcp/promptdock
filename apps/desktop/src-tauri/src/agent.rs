mod event_ingestor;
mod event_v2;
mod keys;
mod observation_v1;
mod run_finalizer;
mod run_reducer;

pub(crate) use event_ingestor::AgentEventIngestor;
pub(crate) use run_finalizer::RunFinalizer;

pub use event_v2::{
    source_hash_sha256, AgentCorrelationV2, AgentEventDecodeError, AgentEventEnvelopeV2,
    AgentEventKind, AgentEventPayloadV2, AgentEventV2Error, AgentOutcome, AttentionKind,
    AttentionRequiredV2, CaptureMetadata, CodexUsageSnapshot, CompletionConfidence, OutputKind,
    OutputProducedV2, PromptOrigin, PromptSubmittedV2, RunCancelledV2, RunCompletedV2, RunFailedV2,
    RunInterruptedV2, RunSettlingV2, RunStartedV2, SettlingReason, SourceClassification,
    AGENT_EVENT_SCHEMA_VERSION, MAX_RESULT_CONTENT_BYTES,
};
pub use keys::{
    content_hash, derive_agent_conversation_key, derive_agent_event_id, derive_agent_run_key,
    StableKeyError,
};
pub use observation_v1::{
    reduce_task_projection, AgentObservationDecodeError, AgentObservationEnvelopeV1,
    AgentObservationKind, AgentObservationPayloadV1, AgentObservationV1Error, ArtifactKind,
    ArtifactPublishedV1, RunPhaseChangedV1, RunPhaseState, TaskExecutionChangedV1,
    TaskExecutionState, TaskProjectionV1, TaskReducerError, TaskResolution,
    TaskResolutionChangedV1, TaskUpsertedV1, ToolActivityRecordedV1, ToolActivityStatus,
    ToolCategory, AGENT_OBSERVATION_SCHEMA_VERSION,
};
