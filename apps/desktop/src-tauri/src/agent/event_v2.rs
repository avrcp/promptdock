use serde::{Deserialize, Serialize};

use crate::model::ResultContentMode;
use crate::source::SourceDescriptor;
use sha2::{Digest as _, Sha256};

pub const AGENT_EVENT_SCHEMA_VERSION: u16 = 4;

const MAX_KEY_CHARS: usize = 256;
const MAX_LABEL_CHARS: usize = 256;
const MAX_PATH_CHARS: usize = 32_768;
const MAX_SAFE_SUMMARY_CHARS: usize = 4_096;
pub const MAX_RESULT_CONTENT_BYTES: usize = 256 * 1024;
const MAX_CONTENT_BYTES: usize = MAX_RESULT_CONTENT_BYTES;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentEventEnvelopeV2 {
    pub schema_version: u16,
    pub source: SourceDescriptor,
    pub event_id: String,
    pub occurred_at: i64,
    pub observed_at: i64,
    pub correlation: AgentCorrelationV2,
    pub payload: AgentEventPayloadV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<CaptureMetadata>,
}

/// Small, non-content facts supplied by the registered Desktop capture helper.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureMetadata {
    #[serde(default)]
    pub helper_build: String,
    pub policy_revision: i64,
    pub source_classification: SourceClassification,
    pub source_reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceClassification {
    UserTurn,
    ExplicitNonTarget,
    Unknown,
    Verification,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentCorrelationV2 {
    pub conversation_key: Option<String>,
    pub run_key: Option<String>,
    pub parent_run_key: Option<String>,
    pub agent_id: Option<String>,
    pub agent_role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AgentEventPayloadV2 {
    PromptSubmitted(PromptSubmittedV2),
    RunStarted(RunStartedV2),
    RunSettling(RunSettlingV2),
    RunCompleted(RunCompletedV2),
    RunFailed(RunFailedV2),
    RunInterrupted(RunInterruptedV2),
    RunCancelled(RunCancelledV2),
    OutputProduced(OutputProducedV2),
    AttentionRequired(AttentionRequiredV2),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromptSubmittedV2 {
    pub text: String,
    pub workspace_path: Option<String>,
    pub model_name: Option<String>,
    pub origin: PromptOrigin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptOrigin {
    Human,
    System,
    AgentGenerated,
    Imported,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunStartedV2 {
    pub agent_label: String,
    pub workspace_path: Option<String>,
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunSettlingV2 {
    pub reason: SettlingReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_usage: Option<CodexUsageSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexUsageSnapshot {
    pub used_percent: Option<u8>,
    pub window_duration_mins: Option<i64>,
    pub resets_at: Option<i64>,
}

impl CodexUsageSnapshot {
    pub fn unavailable() -> Self {
        Self {
            used_percent: None,
            window_duration_mins: None,
            resets_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SettlingReason {
    AgentStopHook,
    AdapterSettled,
    WorkflowIdle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunCompletedV2 {
    pub completion_confidence: CompletionConfidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunFailedV2 {
    pub completion_confidence: CompletionConfidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunInterruptedV2 {
    pub completion_confidence: CompletionConfidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunCancelledV2 {
    pub completion_confidence: CompletionConfidence,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionConfidence {
    Authoritative,
    DefinitiveAdapter,
    Inferred,
    Provisional,
}

impl CompletionConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authoritative => "authoritative",
            Self::DefinitiveAdapter => "definitive_adapter",
            Self::Inferred => "inferred",
            Self::Provisional => "provisional",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutcome {
    Success,
    Failure,
    Interrupted,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutputProducedV2 {
    pub output_kind: OutputKind,
    pub text: String,
    pub content_mode: ResultContentMode,
    pub content_available: bool,
    pub source_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_generation: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    AssistantFinal,
    AssistantCandidate,
    ErrorSummary,
    SystemSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttentionRequiredV2 {
    pub attention_kind: AttentionKind,
    pub safe_summary: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    Permission,
    UserInput,
    Confirmation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind {
    PromptSubmitted,
    RunStarted,
    RunSettling,
    RunCompleted,
    RunFailed,
    RunInterrupted,
    RunCancelled,
    OutputProduced,
    AttentionRequired,
}

impl AgentEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PromptSubmitted => "prompt_submitted",
            Self::RunStarted => "run_started",
            Self::RunSettling => "run_settling",
            Self::RunCompleted => "run_completed",
            Self::RunFailed => "run_failed",
            Self::RunInterrupted => "run_interrupted",
            Self::RunCancelled => "run_cancelled",
            Self::OutputProduced => "output_produced",
            Self::AttentionRequired => "attention_required",
        }
    }
}

impl AgentEventPayloadV2 {
    pub fn kind(&self) -> AgentEventKind {
        match self {
            Self::PromptSubmitted(_) => AgentEventKind::PromptSubmitted,
            Self::RunStarted(_) => AgentEventKind::RunStarted,
            Self::RunSettling(_) => AgentEventKind::RunSettling,
            Self::RunCompleted(_) => AgentEventKind::RunCompleted,
            Self::RunFailed(_) => AgentEventKind::RunFailed,
            Self::RunInterrupted(_) => AgentEventKind::RunInterrupted,
            Self::RunCancelled(_) => AgentEventKind::RunCancelled,
            Self::OutputProduced(_) => AgentEventKind::OutputProduced,
            Self::AttentionRequired(_) => AgentEventKind::AttentionRequired,
        }
    }

    pub fn outcome(&self) -> Option<AgentOutcome> {
        match self {
            Self::RunCompleted(_) => Some(AgentOutcome::Success),
            Self::RunFailed(_) => Some(AgentOutcome::Failure),
            Self::RunInterrupted(_) => Some(AgentOutcome::Interrupted),
            Self::RunCancelled(_) => Some(AgentOutcome::Cancelled),
            _ => None,
        }
    }

    pub fn completion_confidence(&self) -> Option<CompletionConfidence> {
        match self {
            Self::RunCompleted(event) => Some(event.completion_confidence),
            Self::RunFailed(event) => Some(event.completion_confidence),
            Self::RunInterrupted(event) => Some(event.completion_confidence),
            Self::RunCancelled(event) => Some(event.completion_confidence),
            _ => None,
        }
    }
}

impl AgentEventEnvelopeV2 {
    pub fn decode_json(bytes: &[u8]) -> Result<Self, AgentEventDecodeError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            schema_version: u16,
        }

        let version = serde_json::from_slice::<VersionProbe>(bytes)
            .map_err(AgentEventDecodeError::Malformed)?
            .schema_version;
        if version != AGENT_EVENT_SCHEMA_VERSION {
            return Err(AgentEventDecodeError::UnsupportedSchema(version));
        }
        let envelope =
            serde_json::from_slice::<Self>(bytes).map_err(AgentEventDecodeError::Malformed)?;
        envelope
            .validate()
            .map_err(AgentEventDecodeError::Invalid)?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<(), AgentEventV2Error> {
        if self.schema_version != AGENT_EVENT_SCHEMA_VERSION {
            return Err(AgentEventV2Error::UnsupportedSchema(self.schema_version));
        }
        self.source.validate()?;
        if let Some(metadata) = &self.metadata {
            if metadata.helper_build.len() > 128
                || !metadata.helper_build.is_ascii()
                || metadata.helper_build.chars().any(char::is_control)
            {
                return Err(AgentEventV2Error::InvalidCaptureMetadata);
            }
            if metadata.policy_revision < 0
                || metadata.source_reason.is_empty()
                || metadata.source_reason.len() > 128
                || !metadata
                    .source_reason
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(AgentEventV2Error::InvalidCaptureMetadata);
            }
        }
        validate_key("event_id", &self.event_id)?;
        if self.occurred_at < 0 || self.observed_at < self.occurred_at {
            return Err(AgentEventV2Error::InvalidTimestamp);
        }
        self.correlation.validate()?;
        if self.correlation.parent_run_key.is_some() && self.correlation.run_key.is_none() {
            return Err(AgentEventV2Error::MissingCorrelation("run_key"));
        }
        if requires_run_key(self.payload.kind()) && self.correlation.run_key.is_none() {
            return Err(AgentEventV2Error::MissingCorrelation("run_key"));
        }
        self.payload.validate()?;
        if let AgentEventPayloadV2::AttentionRequired(event) = &self.payload {
            if event
                .expires_at
                .is_some_and(|value| value <= self.occurred_at)
            {
                return Err(AgentEventV2Error::InvalidExpiry);
            }
        }
        Ok(())
    }

    pub fn source_key(&self) -> String {
        self.source.source_key()
    }

    /// Hashes immutable event meaning while excluding transport identity and capture timestamps.
    /// Including correlation prevents an event id replay from being rebound to another run.
    pub fn payload_hash(&self) -> Result<String, serde_json::Error> {
        let canonical = serde_json::to_string(&(
            self.schema_version,
            &self.correlation,
            &self.payload,
            &self.metadata,
        ))?;
        Ok(crate::source::derive_stable_key(
            "agent-event-payload-v2",
            &[&canonical],
        ))
    }
}

impl AgentCorrelationV2 {
    fn validate(&self) -> Result<(), AgentEventV2Error> {
        for (name, value) in [
            ("conversation_key", self.conversation_key.as_deref()),
            ("run_key", self.run_key.as_deref()),
            ("parent_run_key", self.parent_run_key.as_deref()),
            ("agent_id", self.agent_id.as_deref()),
            ("agent_role", self.agent_role.as_deref()),
        ] {
            if let Some(value) = value {
                validate_key(name, value)?;
            }
        }
        Ok(())
    }
}

impl AgentEventPayloadV2 {
    fn validate(&self) -> Result<(), AgentEventV2Error> {
        match self {
            Self::PromptSubmitted(event) => {
                validate_content("text", &event.text)?;
                validate_optional_path(event.workspace_path.as_deref())?;
                validate_optional_label("model_name", event.model_name.as_deref())
            }
            Self::RunStarted(event) => {
                validate_label("agent_label", &event.agent_label)?;
                validate_optional_path(event.workspace_path.as_deref())?;
                validate_optional_label("model_name", event.model_name.as_deref())?;
                if let Some(task) = event.task.as_deref() {
                    validate_bounded_text("task", task, MAX_SAFE_SUMMARY_CHARS, true)?;
                }
                validate_optional_label("reasoning_effort", event.reasoning_effort.as_deref())
            }
            Self::RunSettling(event) => {
                if let Some(usage) = &event.codex_usage {
                    if usage.used_percent.is_some_and(|value| value > 100)
                        || usage.window_duration_mins.is_some_and(|value| value <= 0)
                        || usage.resets_at.is_some_and(|value| value < 0)
                    {
                        return Err(AgentEventV2Error::InvalidUsage);
                    }
                }
                Ok(())
            }
            Self::RunCompleted(_)
            | Self::RunFailed(_)
            | Self::RunInterrupted(_)
            | Self::RunCancelled(_) => Ok(()),
            Self::OutputProduced(event) => validate_output(event),
            Self::AttentionRequired(event) => {
                validate_summary(&event.safe_summary)?;
                Ok(())
            }
        }
    }
}

fn validate_output(event: &OutputProducedV2) -> Result<(), AgentEventV2Error> {
    match event.content_mode {
        ResultContentMode::StatusOnly => {
            if event.content_available || !event.text.is_empty() || event.source_hash.is_some() {
                return Err(AgentEventV2Error::InvalidResultContent);
            }
        }
        ResultContentMode::RedactedExcerpt | ResultContentMode::FullFinal => {
            if !event.content_available {
                if !event.text.is_empty() || event.source_hash.is_some() {
                    return Err(AgentEventV2Error::InvalidResultContent);
                }
                return Ok(());
            }
            validate_content_bytes("text", &event.text)?;
            let expected = source_hash_sha256(&event.text);
            if event.source_hash.as_deref() != Some(expected.as_str()) {
                return Err(AgentEventV2Error::InvalidResultHash);
            }
        }
    }
    Ok(())
}

fn requires_run_key(kind: AgentEventKind) -> bool {
    matches!(
        kind,
        AgentEventKind::RunStarted
            | AgentEventKind::RunSettling
            | AgentEventKind::RunCompleted
            | AgentEventKind::RunFailed
            | AgentEventKind::RunInterrupted
            | AgentEventKind::RunCancelled
            | AgentEventKind::OutputProduced
    )
}

fn validate_key(name: &'static str, value: &str) -> Result<(), AgentEventV2Error> {
    validate_bounded_text(name, value, MAX_KEY_CHARS, false)
}

fn validate_label(name: &'static str, value: &str) -> Result<(), AgentEventV2Error> {
    validate_bounded_text(name, value, MAX_LABEL_CHARS, false)
}

fn validate_optional_label(
    name: &'static str,
    value: Option<&str>,
) -> Result<(), AgentEventV2Error> {
    value.map_or(Ok(()), |value| validate_label(name, value))
}

fn validate_optional_path(value: Option<&str>) -> Result<(), AgentEventV2Error> {
    value.map_or(Ok(()), |value| {
        validate_bounded_text("workspace_path", value, MAX_PATH_CHARS, false)
    })
}

fn validate_content(name: &'static str, value: &str) -> Result<(), AgentEventV2Error> {
    if value.trim().is_empty() {
        return Err(AgentEventV2Error::MissingField(name));
    }
    if value.len() > MAX_CONTENT_BYTES {
        return Err(AgentEventV2Error::ContentTooLarge(name));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(AgentEventV2Error::ContainsControl(name));
    }
    Ok(())
}

fn validate_content_bytes(name: &'static str, value: &str) -> Result<(), AgentEventV2Error> {
    if value.len() > MAX_CONTENT_BYTES {
        return Err(AgentEventV2Error::ContentTooLarge(name));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(AgentEventV2Error::ContainsControl(name));
    }
    Ok(())
}

pub fn source_hash_sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn validate_summary(value: &str) -> Result<(), AgentEventV2Error> {
    validate_bounded_text("safe_summary", value, MAX_SAFE_SUMMARY_CHARS, false)
}

fn validate_bounded_text(
    name: &'static str,
    value: &str,
    max_chars: usize,
    allow_line_controls: bool,
) -> Result<(), AgentEventV2Error> {
    if value.trim().is_empty() {
        return Err(AgentEventV2Error::MissingField(name));
    }
    if value.chars().count() > max_chars {
        return Err(AgentEventV2Error::FieldTooLong(name));
    }
    if value.chars().any(|character| {
        character.is_control() && !(allow_line_controls && matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(AgentEventV2Error::ContainsControl(name));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum AgentEventV2Error {
    #[error("invalid bounded capture metadata")]
    InvalidCaptureMetadata,
    #[error("unsupported agent event schema version {0}")]
    UnsupportedSchema(u16),
    #[error("invalid agent event source: {0}")]
    InvalidSource(#[from] crate::source::SourceDescriptorError),
    #[error("{0} must not be empty")]
    MissingField(&'static str),
    #[error("{0} exceeds its length limit")]
    FieldTooLong(&'static str),
    #[error("{0} exceeds the content size limit")]
    ContentTooLarge(&'static str),
    #[error("{0} contains control characters")]
    ContainsControl(&'static str),
    #[error("result content fields are inconsistent with the frozen content mode")]
    InvalidResultContent,
    #[error("result source hash does not match the exact UTF-8 body")]
    InvalidResultHash,
    #[error("agent event timestamps are invalid or out of order")]
    InvalidTimestamp,
    #[error("{0} correlation is required for this event kind")]
    MissingCorrelation(&'static str),
    #[error("attention expiry must be later than the event occurrence time")]
    InvalidExpiry,
    #[error("Codex usage fields are outside their accepted ranges")]
    InvalidUsage,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentEventDecodeError {
    #[error("unsupported agent event schema version {0}")]
    UnsupportedSchema(u16),
    #[error("malformed agent event JSON: {0}")]
    Malformed(serde_json::Error),
    #[error("invalid agent event: {0}")]
    Invalid(AgentEventV2Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{content_hash, derive_agent_event_id};
    use crate::source::AgentKind;

    fn source() -> SourceDescriptor {
        SourceDescriptor::new(
            AgentKind::new("fixture-agent").unwrap(),
            "events",
            "fixture_inbox",
            "default",
        )
        .unwrap()
    }

    fn envelope(payload: AgentEventPayloadV2) -> AgentEventEnvelopeV2 {
        let run_key = "run-key";
        AgentEventEnvelopeV2 {
            metadata: None,
            schema_version: AGENT_EVENT_SCHEMA_VERSION,
            source: source(),
            event_id: derive_agent_event_id(payload.kind(), &[run_key]).unwrap(),
            occurred_at: 1,
            observed_at: 2,
            correlation: AgentCorrelationV2 {
                conversation_key: Some("conversation-key".into()),
                run_key: Some(run_key.into()),
                parent_run_key: Some("parent-run-key".into()),
                agent_id: Some("agent-1".into()),
                agent_role: Some("worker".into()),
            },
            payload,
        }
    }

    #[test]
    fn all_event_kinds_validate_and_serialize_with_snake_case_tags() {
        let events = [
            AgentEventPayloadV2::PromptSubmitted(PromptSubmittedV2 {
                text: "prompt".into(),
                workspace_path: None,
                model_name: None,
                origin: PromptOrigin::Human,
            }),
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture Agent".into(),
                workspace_path: None,
                model_name: None,
                task: None,
                reasoning_effort: None,
            }),
            AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                reason: SettlingReason::WorkflowIdle,
                codex_usage: None,
            }),
            AgentEventPayloadV2::RunCompleted(RunCompletedV2 {
                completion_confidence: CompletionConfidence::Authoritative,
            }),
            AgentEventPayloadV2::RunFailed(RunFailedV2 {
                completion_confidence: CompletionConfidence::Authoritative,
            }),
            AgentEventPayloadV2::RunInterrupted(RunInterruptedV2 {
                completion_confidence: CompletionConfidence::Authoritative,
            }),
            AgentEventPayloadV2::RunCancelled(RunCancelledV2 {
                completion_confidence: CompletionConfidence::Authoritative,
            }),
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "candidate".into(),
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(source_hash_sha256("candidate")),
                capture_generation: Some(0),
            }),
            AgentEventPayloadV2::AttentionRequired(AttentionRequiredV2 {
                attention_kind: AttentionKind::Permission,
                safe_summary: "permission required".into(),
                expires_at: Some(10),
            }),
        ];

        for payload in events {
            let event = envelope(payload);
            event.validate().unwrap();
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(value["payload"]["kind"], event.payload.kind().as_str());
            assert_eq!(event.source_key(), event.source.source_key());
            assert_eq!(event.payload_hash().unwrap().len(), 64);
        }
    }

    #[test]
    fn future_schema_invalid_source_controls_and_oversized_components_are_rejected() {
        let mut future = envelope(AgentEventPayloadV2::PromptSubmitted(PromptSubmittedV2 {
            text: "prompt".into(),
            workspace_path: None,
            model_name: None,
            origin: PromptOrigin::Human,
        }));
        future.schema_version += 1;
        assert!(matches!(
            future.validate(),
            Err(AgentEventV2Error::UnsupportedSchema(5))
        ));

        let mut invalid_source = future.clone();
        invalid_source.schema_version = AGENT_EVENT_SCHEMA_VERSION;
        invalid_source.source.source_id = "bad\nsource".into();
        assert!(matches!(
            invalid_source.validate(),
            Err(AgentEventV2Error::InvalidSource(_))
        ));

        let mut invalid_correlation = envelope(AgentEventPayloadV2::RunSettling(RunSettlingV2 {
            reason: SettlingReason::AdapterSettled,
            codex_usage: None,
        }));
        invalid_correlation.correlation.agent_role = Some("bad\nrole".into());
        assert!(matches!(
            invalid_correlation.validate(),
            Err(AgentEventV2Error::ContainsControl("agent_role"))
        ));

        let mut oversized = invalid_correlation;
        oversized.correlation.agent_role = Some("x".repeat(MAX_KEY_CHARS + 1));
        assert!(matches!(
            oversized.validate(),
            Err(AgentEventV2Error::FieldTooLong("agent_role"))
        ));
    }

    #[test]
    fn payload_hash_covers_correlation_and_content() {
        let first = envelope(AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
            output_kind: OutputKind::AssistantCandidate,
            text: "candidate".into(),
            content_mode: ResultContentMode::RedactedExcerpt,
            content_available: true,
            source_hash: Some(source_hash_sha256("candidate")),
            capture_generation: Some(0),
        }));
        let mut replay = first.clone();
        assert_eq!(
            first.payload_hash().unwrap(),
            replay.payload_hash().unwrap()
        );

        let AgentEventPayloadV2::OutputProduced(output) = &mut replay.payload else {
            unreachable!();
        };
        output.text = "different candidate".into();
        assert_ne!(
            first.payload_hash().unwrap(),
            replay.payload_hash().unwrap()
        );

        replay = first.clone();
        replay.correlation.run_key = Some("different-run".into());
        assert_ne!(
            first.payload_hash().unwrap(),
            replay.payload_hash().unwrap()
        );

        replay = first.clone();
        replay.observed_at += 1;
        assert_eq!(
            first.payload_hash().unwrap(),
            replay.payload_hash().unwrap()
        );

        replay.occurred_at += 1;
        assert_eq!(
            first.payload_hash().unwrap(),
            replay.payload_hash().unwrap()
        );

        replay = first.clone();
        let AgentEventPayloadV2::OutputProduced(output) = &mut replay.payload else {
            unreachable!();
        };
        output.output_kind = OutputKind::AssistantFinal;
        assert_eq!(content_hash("candidate"), content_hash(&output.text));
        assert_ne!(
            first.payload_hash().unwrap(),
            replay.payload_hash().unwrap()
        );
    }

    #[test]
    fn run_and_output_events_require_run_correlation() {
        let mut event = envelope(AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
            output_kind: OutputKind::AssistantFinal,
            text: "result".into(),
            content_mode: ResultContentMode::FullFinal,
            content_available: true,
            source_hash: Some(source_hash_sha256("result")),
            capture_generation: Some(0),
        }));
        event.correlation.run_key = None;
        assert!(matches!(
            event.validate(),
            Err(AgentEventV2Error::MissingCorrelation("run_key"))
        ));
    }

    #[test]
    fn decode_reports_future_schema_before_unknown_payload_shape() {
        let bytes = br#"{
            "schema_version": 5,
            "source": {},
            "payload": { "kind": "future_event", "data": { "future": true } }
        }"#;
        assert!(matches!(
            AgentEventEnvelopeV2::decode_json(bytes),
            Err(AgentEventDecodeError::UnsupportedSchema(5))
        ));

        let current_with_unknown_field = br#"{
            "schema_version": 4,
            "source": {
                "agent_kind": "fixture-agent",
                "source_id": "events",
                "source_kind": "fixture_inbox",
                "instance_id": "default"
            },
            "event_id": "event",
            "occurred_at": 1,
            "observed_at": 1,
            "correlation": {
                "conversation_key": null,
                "run_key": null,
                "parent_run_key": null,
                "agent_id": null,
                "agent_role": null
            },
            "payload": {
                "kind": "prompt_submitted",
                "data": {
                    "text": "prompt",
                    "workspace_path": null,
                    "model_name": null,
                    "origin": "human",
                    "unknown": true
                }
            }
        }"#;
        assert!(matches!(
            AgentEventEnvelopeV2::decode_json(current_with_unknown_field),
            Err(AgentEventDecodeError::Malformed(_))
        ));

        let mut nested_source = serde_json::to_value(envelope(
            AgentEventPayloadV2::PromptSubmitted(PromptSubmittedV2 {
                text: "prompt".into(),
                workspace_path: None,
                model_name: None,
                origin: PromptOrigin::Human,
            }),
        ))
        .unwrap();
        nested_source["source"]["unknown"] = true.into();
        assert!(matches!(
            AgentEventEnvelopeV2::decode_json(&serde_json::to_vec(&nested_source).unwrap()),
            Err(AgentEventDecodeError::Malformed(_))
        ));
    }

    #[test]
    fn terminal_confidence_and_attention_expiry_are_semantically_validated() {
        let mut terminal = envelope(AgentEventPayloadV2::AttentionRequired(
            AttentionRequiredV2 {
                attention_kind: AttentionKind::Confirmation,
                safe_summary: "confirm".into(),
                expires_at: Some(1),
            },
        ));
        terminal.correlation.run_key = None;
        terminal.correlation.parent_run_key = None;
        assert!(matches!(
            terminal.validate(),
            Err(AgentEventV2Error::InvalidExpiry)
        ));

        terminal.correlation.parent_run_key = Some("parent".into());
        assert!(matches!(
            terminal.validate(),
            Err(AgentEventV2Error::MissingCorrelation("run_key"))
        ));
    }

    #[test]
    fn stable_hashes_have_golden_values() {
        let event = envelope(AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
            output_kind: OutputKind::AssistantCandidate,
            text: "candidate".into(),
            content_mode: ResultContentMode::RedactedExcerpt,
            content_available: true,
            source_hash: Some(source_hash_sha256("candidate")),
            capture_generation: Some(0),
        }));
        assert_eq!(
            event.source_key(),
            "92e6cadbe2fc04c0acf692a3943ec358f66f37474717d59bb58c8e5402337fb0"
        );
        assert_eq!(
            content_hash("candidate"),
            "d4d388bc97f5bf06b6d3fb45e7b6f5bc433a4e24946b2cf250b3af5404d70054"
        );
        assert_eq!(
            event.payload_hash().unwrap(),
            "c41acab18554a440240c584d31ae2bfbe86e6103588228e48ff26e23e5e94919"
        );
        assert_eq!(
            event.event_id,
            "7787491ebf3238e295611c35d277ee7549f19e23eca2b3019eba94998078e6a3"
        );
    }
}
