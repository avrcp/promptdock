use serde::{Deserialize, Serialize};

use crate::agent::AgentCorrelationV2;
use crate::source::{derive_stable_key, SourceDescriptor};

pub const AGENT_OBSERVATION_SCHEMA_VERSION: u16 = 1;

const MAX_KEY_CHARS: usize = 256;
const MAX_DISPLAY_NAME_CHARS: usize = 256;
const MAX_DESCRIPTION_CHARS: usize = 4_096;
const MAX_GROUP_LABEL_CHARS: usize = 256;
const MAX_LOCAL_LOCATOR_CHARS: usize = 2_048;
const MAX_MIME_TYPE_CHARS: usize = 255;
const MAX_ERROR_CODE_CHARS: usize = 128;
const CONTENT_HASH_HEX_CHARS: usize = 64;
const MAX_TOOL_DURATION_MS: u64 = 30 * 24 * 60 * 60 * 1_000;
const MAX_ARTIFACT_SIZE_BYTES: u64 = 1_099_511_627_776;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentObservationEnvelopeV1 {
    pub schema_version: u16,
    pub source: SourceDescriptor,
    pub event_id: String,
    pub occurred_at: i64,
    pub observed_at: i64,
    pub correlation: AgentCorrelationV2,
    pub payload: AgentObservationPayloadV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AgentObservationPayloadV1 {
    RunPhaseChanged(RunPhaseChangedV1),
    TaskUpserted(TaskUpsertedV1),
    TaskResolutionChanged(TaskResolutionChangedV1),
    TaskExecutionChanged(TaskExecutionChangedV1),
    ToolActivityRecorded(ToolActivityRecordedV1),
    ArtifactPublished(ArtifactPublishedV1),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunPhaseChangedV1 {
    pub phase_id: String,
    pub display_name: String,
    pub state: RunPhaseState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunPhaseState {
    Planned,
    Active,
    Blocked,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskUpsertedV1 {
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub description_text: Option<String>,
    pub group_label: Option<String>,
    pub phase_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskResolutionChangedV1 {
    pub task_id: String,
    pub resolution: TaskResolution,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskResolution {
    Open,
    InProgress,
    Resolved,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskExecutionChangedV1 {
    pub task_id: String,
    pub execution_state: TaskExecutionState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskExecutionState {
    Created,
    Queued,
    Running,
    Reported,
    Failed,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolActivityRecordedV1 {
    pub activity_id: String,
    pub category: ToolCategory,
    pub display_name: String,
    pub status: ToolActivityStatus,
    pub duration_ms: Option<u64>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Shell,
    FileSystem,
    Search,
    Network,
    Browser,
    Compute,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolActivityStatus {
    Started,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactPublishedV1 {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    pub display_name: String,
    pub local_locator: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Document,
    Image,
    Audio,
    Video,
    Archive,
    SourceCode,
    Data,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskProjectionV1 {
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub description_text: Option<String>,
    pub group_label: Option<String>,
    pub phase_id: Option<String>,
    pub model_resolution: TaskResolution,
    pub runtime_status: TaskExecutionState,
}

impl AgentObservationEnvelopeV1 {
    pub fn decode_json(bytes: &[u8]) -> Result<Self, AgentObservationDecodeError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            schema_version: u16,
        }

        let version = serde_json::from_slice::<VersionProbe>(bytes)
            .map_err(AgentObservationDecodeError::Malformed)?
            .schema_version;
        if version != AGENT_OBSERVATION_SCHEMA_VERSION {
            return Err(AgentObservationDecodeError::UnsupportedSchema(version));
        }
        let envelope = serde_json::from_slice::<Self>(bytes)
            .map_err(AgentObservationDecodeError::Malformed)?;
        envelope
            .validate()
            .map_err(AgentObservationDecodeError::Invalid)?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<(), AgentObservationV1Error> {
        if self.schema_version != AGENT_OBSERVATION_SCHEMA_VERSION {
            return Err(AgentObservationV1Error::UnsupportedSchema(
                self.schema_version,
            ));
        }
        self.source.validate()?;
        validate_key("event_id", &self.event_id)?;
        if self.occurred_at < 0 || self.observed_at < self.occurred_at {
            return Err(AgentObservationV1Error::InvalidTimestamp);
        }
        validate_correlation(&self.correlation)?;
        if self.correlation.run_key.is_none() {
            return Err(AgentObservationV1Error::MissingCorrelation("run_key"));
        }
        self.payload.validate()
    }

    pub fn source_key(&self) -> String {
        self.source.source_key()
    }

    pub fn payload_hash(&self) -> Result<String, serde_json::Error> {
        let canonical =
            serde_json::to_string(&(self.schema_version, &self.correlation, &self.payload))?;
        Ok(derive_stable_key(
            "agent-observation-payload-v1",
            &[&canonical],
        ))
    }
}

impl AgentObservationPayloadV1 {
    pub fn kind(&self) -> AgentObservationKind {
        match self {
            Self::RunPhaseChanged(_) => AgentObservationKind::RunPhaseChanged,
            Self::TaskUpserted(_) => AgentObservationKind::TaskUpserted,
            Self::TaskResolutionChanged(_) => AgentObservationKind::TaskResolutionChanged,
            Self::TaskExecutionChanged(_) => AgentObservationKind::TaskExecutionChanged,
            Self::ToolActivityRecorded(_) => AgentObservationKind::ToolActivityRecorded,
            Self::ArtifactPublished(_) => AgentObservationKind::ArtifactPublished,
        }
    }

    fn validate(&self) -> Result<(), AgentObservationV1Error> {
        match self {
            Self::RunPhaseChanged(value) => {
                validate_key("phase_id", &value.phase_id)?;
                validate_display_name(&value.display_name)
            }
            Self::TaskUpserted(value) => {
                validate_key("task_id", &value.task_id)?;
                validate_optional_key("parent_task_id", value.parent_task_id.as_deref())?;
                if value.parent_task_id.as_deref() == Some(value.task_id.as_str()) {
                    return Err(AgentObservationV1Error::SelfParentTask);
                }
                validate_optional_text(
                    "description_text",
                    value.description_text.as_deref(),
                    MAX_DESCRIPTION_CHARS,
                    true,
                )?;
                validate_optional_text(
                    "group_label",
                    value.group_label.as_deref(),
                    MAX_GROUP_LABEL_CHARS,
                    false,
                )?;
                validate_optional_key("phase_id", value.phase_id.as_deref())
            }
            Self::TaskResolutionChanged(value) => validate_key("task_id", &value.task_id),
            Self::TaskExecutionChanged(value) => validate_key("task_id", &value.task_id),
            Self::ToolActivityRecorded(value) => {
                validate_key("activity_id", &value.activity_id)?;
                validate_display_name(&value.display_name)?;
                if value
                    .duration_ms
                    .is_some_and(|duration| duration > MAX_TOOL_DURATION_MS)
                {
                    return Err(AgentObservationV1Error::ValueTooLarge("duration_ms"));
                }
                validate_optional_error_code(value.error_code.as_deref())
            }
            Self::ArtifactPublished(value) => {
                validate_key("artifact_id", &value.artifact_id)?;
                validate_display_name(&value.display_name)?;
                validate_text(
                    "local_locator",
                    &value.local_locator,
                    MAX_LOCAL_LOCATOR_CHARS,
                    false,
                )?;
                validate_optional_mime_type(value.mime_type.as_deref())?;
                if value
                    .size_bytes
                    .is_some_and(|size| size > MAX_ARTIFACT_SIZE_BYTES)
                {
                    return Err(AgentObservationV1Error::ValueTooLarge("size_bytes"));
                }
                validate_optional_content_hash(value.content_hash.as_deref())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentObservationKind {
    RunPhaseChanged,
    TaskUpserted,
    TaskResolutionChanged,
    TaskExecutionChanged,
    ToolActivityRecorded,
    ArtifactPublished,
}

impl AgentObservationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunPhaseChanged => "run_phase_changed",
            Self::TaskUpserted => "task_upserted",
            Self::TaskResolutionChanged => "task_resolution_changed",
            Self::TaskExecutionChanged => "task_execution_changed",
            Self::ToolActivityRecorded => "tool_activity_recorded",
            Self::ArtifactPublished => "artifact_published",
        }
    }
}

/// Pure projection contract for R8's durable reducer. Metadata upserts preserve both truth axes;
/// each change payload owns exactly one axis.
pub fn reduce_task_projection(
    current: Option<TaskProjectionV1>,
    payload: &AgentObservationPayloadV1,
) -> Result<Option<TaskProjectionV1>, TaskReducerError> {
    match payload {
        AgentObservationPayloadV1::TaskUpserted(value) => {
            if let Some(existing) = current.as_ref() {
                ensure_same_task(existing, &value.task_id)?;
            }
            Ok(Some(TaskProjectionV1 {
                task_id: value.task_id.clone(),
                parent_task_id: value.parent_task_id.clone(),
                description_text: value.description_text.clone(),
                group_label: value.group_label.clone(),
                phase_id: value.phase_id.clone(),
                model_resolution: current
                    .as_ref()
                    .map_or(TaskResolution::Open, |task| task.model_resolution),
                runtime_status: current
                    .as_ref()
                    .map_or(TaskExecutionState::Created, |task| task.runtime_status),
            }))
        }
        AgentObservationPayloadV1::TaskResolutionChanged(value) => {
            let mut task = require_task(current, &value.task_id)?;
            task.model_resolution = value.resolution;
            Ok(Some(task))
        }
        AgentObservationPayloadV1::TaskExecutionChanged(value) => {
            let mut task = require_task(current, &value.task_id)?;
            task.runtime_status = value.execution_state;
            Ok(Some(task))
        }
        AgentObservationPayloadV1::RunPhaseChanged(_)
        | AgentObservationPayloadV1::ToolActivityRecorded(_)
        | AgentObservationPayloadV1::ArtifactPublished(_) => Ok(current),
    }
}

fn require_task(
    current: Option<TaskProjectionV1>,
    task_id: &str,
) -> Result<TaskProjectionV1, TaskReducerError> {
    let task = current.ok_or(TaskReducerError::TaskMissing)?;
    ensure_same_task(&task, task_id)?;
    Ok(task)
}

fn ensure_same_task(task: &TaskProjectionV1, task_id: &str) -> Result<(), TaskReducerError> {
    if task.task_id == task_id {
        Ok(())
    } else {
        Err(TaskReducerError::TaskMismatch)
    }
}

fn validate_correlation(value: &AgentCorrelationV2) -> Result<(), AgentObservationV1Error> {
    for (name, value) in [
        ("conversation_key", value.conversation_key.as_deref()),
        ("run_key", value.run_key.as_deref()),
        ("parent_run_key", value.parent_run_key.as_deref()),
        ("agent_id", value.agent_id.as_deref()),
        ("agent_role", value.agent_role.as_deref()),
    ] {
        validate_optional_key(name, value)?;
    }
    if value.parent_run_key.is_some() && value.run_key.is_none() {
        return Err(AgentObservationV1Error::MissingCorrelation("run_key"));
    }
    Ok(())
}

fn validate_key(name: &'static str, value: &str) -> Result<(), AgentObservationV1Error> {
    validate_text(name, value, MAX_KEY_CHARS, false)
}

fn validate_optional_key(
    name: &'static str,
    value: Option<&str>,
) -> Result<(), AgentObservationV1Error> {
    value.map_or(Ok(()), |value| validate_key(name, value))
}

fn validate_display_name(value: &str) -> Result<(), AgentObservationV1Error> {
    validate_text("display_name", value, MAX_DISPLAY_NAME_CHARS, false)
}

fn validate_optional_text(
    name: &'static str,
    value: Option<&str>,
    max_chars: usize,
    allow_line_controls: bool,
) -> Result<(), AgentObservationV1Error> {
    value.map_or(Ok(()), |value| {
        validate_text(name, value, max_chars, allow_line_controls)
    })
}

fn validate_text(
    name: &'static str,
    value: &str,
    max_chars: usize,
    allow_line_controls: bool,
) -> Result<(), AgentObservationV1Error> {
    if value.trim().is_empty() {
        return Err(AgentObservationV1Error::MissingField(name));
    }
    if value.chars().count() > max_chars {
        return Err(AgentObservationV1Error::FieldTooLong(name));
    }
    if value.chars().any(|character| {
        character.is_control() && !(allow_line_controls && matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(AgentObservationV1Error::ContainsControl(name));
    }
    Ok(())
}

fn validate_optional_error_code(value: Option<&str>) -> Result<(), AgentObservationV1Error> {
    let Some(value) = value else {
        return Ok(());
    };
    validate_text("error_code", value, MAX_ERROR_CODE_CHARS, false)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
    }) {
        return Err(AgentObservationV1Error::InvalidFormat("error_code"));
    }
    Ok(())
}

fn validate_optional_mime_type(value: Option<&str>) -> Result<(), AgentObservationV1Error> {
    let Some(value) = value else {
        return Ok(());
    };
    validate_text("mime_type", value, MAX_MIME_TYPE_CHARS, false)?;
    let Some((type_name, subtype)) = value.split_once('/') else {
        return Err(AgentObservationV1Error::InvalidFormat("mime_type"));
    };
    if type_name.is_empty()
        || subtype.is_empty()
        || subtype.contains('/')
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'/' | b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
    {
        return Err(AgentObservationV1Error::InvalidFormat("mime_type"));
    }
    Ok(())
}

fn validate_optional_content_hash(value: Option<&str>) -> Result<(), AgentObservationV1Error> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() != CONTENT_HASH_HEX_CHARS
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AgentObservationV1Error::InvalidFormat("content_hash"));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentObservationV1Error {
    #[error("unsupported agent observation schema version {0}")]
    UnsupportedSchema(u16),
    #[error("invalid agent observation source: {0}")]
    InvalidSource(#[from] crate::source::SourceDescriptorError),
    #[error("{0} must not be empty")]
    MissingField(&'static str),
    #[error("{0} exceeds its length limit")]
    FieldTooLong(&'static str),
    #[error("{0} contains control characters")]
    ContainsControl(&'static str),
    #[error("{0} exceeds its numeric limit")]
    ValueTooLarge(&'static str),
    #[error("{0} has an invalid format")]
    InvalidFormat(&'static str),
    #[error("agent observation timestamps are invalid or out of order")]
    InvalidTimestamp,
    #[error("{0} correlation is required for observations")]
    MissingCorrelation(&'static str),
    #[error("a task cannot be its own parent")]
    SelfParentTask,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentObservationDecodeError {
    #[error("unsupported agent observation schema version {0}")]
    UnsupportedSchema(u16),
    #[error("malformed agent observation: {0}")]
    Malformed(#[source] serde_json::Error),
    #[error("invalid agent observation: {0}")]
    Invalid(#[source] AgentObservationV1Error),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskReducerError {
    #[error("task projection must exist before a state change")]
    TaskMissing,
    #[error("task payload does not target the current projection")]
    TaskMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::AgentKind;

    fn source() -> SourceDescriptor {
        SourceDescriptor::new(
            AgentKind::new("neutral-agent").unwrap(),
            "observations",
            "fixture_stream",
            "default",
        )
        .unwrap()
    }

    fn envelope(payload: AgentObservationPayloadV1) -> AgentObservationEnvelopeV1 {
        AgentObservationEnvelopeV1 {
            schema_version: AGENT_OBSERVATION_SCHEMA_VERSION,
            source: source(),
            event_id: format!("event-{}", payload.kind().as_str()),
            occurred_at: 10,
            observed_at: 11,
            correlation: AgentCorrelationV2 {
                conversation_key: Some("conversation-1".into()),
                run_key: Some("run-1".into()),
                parent_run_key: None,
                agent_id: Some("agent-1".into()),
                agent_role: Some("worker".into()),
            },
            payload,
        }
    }

    fn upsert() -> AgentObservationPayloadV1 {
        AgentObservationPayloadV1::TaskUpserted(TaskUpsertedV1 {
            task_id: "task-1".into(),
            parent_task_id: None,
            description_text: Some("Review the change".into()),
            group_label: Some("Review".into()),
            phase_id: Some("phase-1".into()),
        })
    }

    #[test]
    fn all_payload_kinds_validate_and_use_closed_snake_case_tags() {
        let payloads = [
            AgentObservationPayloadV1::RunPhaseChanged(RunPhaseChangedV1 {
                phase_id: "phase-1".into(),
                display_name: "Review".into(),
                state: RunPhaseState::Active,
            }),
            upsert(),
            AgentObservationPayloadV1::TaskResolutionChanged(TaskResolutionChangedV1 {
                task_id: "task-1".into(),
                resolution: TaskResolution::Resolved,
            }),
            AgentObservationPayloadV1::TaskExecutionChanged(TaskExecutionChangedV1 {
                task_id: "task-1".into(),
                execution_state: TaskExecutionState::Reported,
            }),
            AgentObservationPayloadV1::ToolActivityRecorded(ToolActivityRecordedV1 {
                activity_id: "activity-1".into(),
                category: ToolCategory::Search,
                display_name: "Repository search".into(),
                status: ToolActivityStatus::Succeeded,
                duration_ms: Some(25),
                error_code: None,
            }),
            AgentObservationPayloadV1::ArtifactPublished(ArtifactPublishedV1 {
                artifact_id: "artifact-1".into(),
                kind: ArtifactKind::Document,
                display_name: "Review summary".into(),
                local_locator: "artifact-blob-1".into(),
                mime_type: Some("text/markdown".into()),
                size_bytes: Some(100),
                content_hash: Some("a".repeat(CONTENT_HASH_HEX_CHARS)),
            }),
        ];

        for payload in payloads {
            let expected_kind = payload.kind().as_str();
            let observation = envelope(payload);
            observation.validate().unwrap();
            let bytes = serde_json::to_vec(&observation).unwrap();
            assert_eq!(
                AgentObservationEnvelopeV1::decode_json(&bytes).unwrap(),
                observation
            );
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["payload"]["kind"],
                expected_kind
            );
            assert_eq!(observation.payload_hash().unwrap().len(), 64);
        }
    }

    #[test]
    fn strict_shapes_and_bounds_reject_unsafe_or_oversized_data() {
        let mut unknown = serde_json::to_value(envelope(upsert())).unwrap();
        unknown["payload"]["data"]["tool_args"] = serde_json::json!({"secret": true});
        assert!(matches!(
            AgentObservationEnvelopeV1::decode_json(&serde_json::to_vec(&unknown).unwrap()),
            Err(AgentObservationDecodeError::Malformed(_))
        ));

        let mut oversized = envelope(upsert());
        let AgentObservationPayloadV1::TaskUpserted(task) = &mut oversized.payload else {
            unreachable!()
        };
        task.description_text = Some("x".repeat(MAX_DESCRIPTION_CHARS + 1));
        assert_eq!(
            oversized.validate(),
            Err(AgentObservationV1Error::FieldTooLong("description_text"))
        );

        let invalid_hash = envelope(AgentObservationPayloadV1::ArtifactPublished(
            ArtifactPublishedV1 {
                artifact_id: "artifact-1".into(),
                kind: ArtifactKind::Data,
                display_name: "Metrics".into(),
                local_locator: "artifact-blob-1".into(),
                mime_type: Some("application/json".into()),
                size_bytes: None,
                content_hash: Some("NOT-A-HASH".into()),
            },
        ));
        assert_eq!(
            invalid_hash.validate(),
            Err(AgentObservationV1Error::InvalidFormat("content_hash"))
        );
    }

    #[test]
    fn task_reducer_keeps_resolution_and_execution_as_independent_truths() {
        let task = reduce_task_projection(None, &upsert()).unwrap().unwrap();
        assert_eq!(task.model_resolution, TaskResolution::Open);
        assert_eq!(task.runtime_status, TaskExecutionState::Created);

        let task = reduce_task_projection(
            Some(task),
            &AgentObservationPayloadV1::TaskResolutionChanged(TaskResolutionChangedV1 {
                task_id: "task-1".into(),
                resolution: TaskResolution::Resolved,
            }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(task.model_resolution, TaskResolution::Resolved);
        assert_eq!(task.runtime_status, TaskExecutionState::Created);

        let task = reduce_task_projection(
            Some(task),
            &AgentObservationPayloadV1::TaskExecutionChanged(TaskExecutionChangedV1 {
                task_id: "task-1".into(),
                execution_state: TaskExecutionState::Failed,
            }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(task.model_resolution, TaskResolution::Resolved);
        assert_eq!(task.runtime_status, TaskExecutionState::Failed);

        let task = reduce_task_projection(Some(task), &upsert())
            .unwrap()
            .unwrap();
        assert_eq!(task.model_resolution, TaskResolution::Resolved);
        assert_eq!(task.runtime_status, TaskExecutionState::Failed);
    }

    #[test]
    fn future_schema_is_reported_before_unknown_payload_shape() {
        let bytes = br#"{
            "schema_version": 2,
            "source": {},
            "payload": { "kind": "future_observation", "data": { "future": true } }
        }"#;
        assert!(matches!(
            AgentObservationEnvelopeV1::decode_json(bytes),
            Err(AgentObservationDecodeError::UnsupportedSchema(2))
        ));
    }
}
