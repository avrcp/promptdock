use std::io::Read;
use std::path::Path;

use serde::Deserialize;
#[cfg(test)]
use serde_json::Value;

use crate::agent::{
    content_hash, derive_agent_conversation_key, derive_agent_event_id, derive_agent_run_key,
    AgentCorrelationV2, AgentEventEnvelopeV2, AgentEventKind, AgentEventPayloadV2, AttentionKind,
    AttentionRequiredV2, OutputKind, OutputProducedV2, PromptOrigin, PromptSubmittedV2,
    RunSettlingV2, RunStartedV2, SettlingReason, AGENT_EVENT_SCHEMA_VERSION,
};
use crate::agent_event_capture::{
    capture_from_reader as capture_batch_from_reader, ensure_input_size, unix_time_ms,
    AgentEventCaptureDisposition, AgentEventCaptureError,
};
use crate::model::ResultContentMode;
use crate::source::{AgentKind, SourceDescriptor};

pub const CODEX_AGENT_EVENT_SOURCE_ID: &str = "codex.hooks.agent_events";
const CODEX_AGENT_KIND: &str = "codex";
const CODEX_SOURCE_KIND: &str = "codex_hook_inbox";
const CODEX_SOURCE_INSTANCE_ID: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexHookEvent {
    UserPrompt,
    Stop,
    PermissionRequest,
}

impl CodexHookEvent {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex-user-prompt" => Some(Self::UserPrompt),
            "codex-stop" => Some(Self::Stop),
            "codex-permission-request" => Some(Self::PermissionRequest),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct UserPromptSubmitInput {
    session_id: String,
    turn_id: String,
    cwd: String,
    model: String,
    hook_event_name: String,
    prompt: String,
    agent_id: Option<String>,
    agent_type: Option<String>,
}

#[derive(Deserialize)]
struct StopInput {
    session_id: String,
    turn_id: String,
    cwd: String,
    model: String,
    hook_event_name: String,
    last_assistant_message: Option<String>,
    agent_id: Option<String>,
    agent_type: Option<String>,
}

#[derive(Deserialize)]
struct PermissionRequestInput {
    session_id: String,
    turn_id: String,
    cwd: String,
    model: String,
    hook_event_name: String,
    tool_name: String,
    agent_id: Option<String>,
    agent_type: Option<String>,
}

pub fn capture_from_reader(
    event: CodexHookEvent,
    reader: impl Read,
    inbox_path: &Path,
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    capture_from_reader_with_options(event, true, reader, inbox_path)
}

pub fn capture_from_reader_with_options(
    event: CodexHookEvent,
    capture_agent_output: bool,
    reader: impl Read,
    inbox_path: &Path,
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    capture_batch_from_reader(reader, inbox_path, |bytes| {
        events_from_hook_json_with_options(event, capture_agent_output, bytes)
    })
}

pub fn events_from_hook_json(
    event: CodexHookEvent,
    bytes: &[u8],
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    events_from_hook_json_with_options(event, true, bytes)
}

pub fn events_from_hook_json_with_options(
    event: CodexHookEvent,
    capture_agent_output: bool,
    bytes: &[u8],
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    ensure_input_size(bytes)?;
    match event {
        CodexHookEvent::UserPrompt => user_prompt_events(bytes),
        CodexHookEvent::Stop => stop_events(bytes, capture_agent_output),
        CodexHookEvent::PermissionRequest => permission_request_events(bytes),
    }
}

pub fn source_descriptor() -> SourceDescriptor {
    SourceDescriptor::new(
        AgentKind::new(CODEX_AGENT_KIND).expect("fixed Codex agent kind must be valid"),
        CODEX_AGENT_EVENT_SOURCE_ID,
        CODEX_SOURCE_KIND,
        CODEX_SOURCE_INSTANCE_ID,
    )
    .expect("fixed Codex source descriptor must be valid")
}

fn user_prompt_events(
    bytes: &[u8],
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    let input: UserPromptSubmitInput = serde_json::from_slice(bytes)?;
    validate_event_name(&input.hook_event_name, "UserPromptSubmit")?;
    if is_subagent(input.agent_id.as_deref(), input.agent_type.as_deref()) {
        return Ok(AgentEventCaptureDisposition::IgnoredSubagent);
    }
    validate_common(&input.session_id, &input.turn_id, &input.cwd, &input.model)?;
    validate_required("prompt", &input.prompt)?;
    let (conversation_key, run_key) = correlation_keys(&input.session_id, &input.turn_id)?;
    let now = unix_time_ms()?;
    let prompt_hash = content_hash(&input.prompt);
    let correlation = correlation(conversation_key, run_key.clone());
    Ok(AgentEventCaptureDisposition::Recorded(vec![
        envelope(
            derive_agent_event_id(AgentEventKind::PromptSubmitted, &[&run_key, &prompt_hash])?,
            now,
            correlation.clone(),
            AgentEventPayloadV2::PromptSubmitted(PromptSubmittedV2 {
                text: input.prompt,
                workspace_path: Some(input.cwd.clone()),
                model_name: Some(input.model.clone()),
                origin: PromptOrigin::Human,
            }),
        ),
        envelope(
            // Desktop Codex may emit more than one UserPromptSubmit for the
            // same thread/turn identity (for example, an in-flight steer).
            // Keep the start fact replay-stable for the same prompt while
            // preventing a later prompt from reusing an event id with a
            // different payload.
            derive_agent_event_id(AgentEventKind::RunStarted, &[&run_key, &prompt_hash])?,
            now,
            correlation,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Codex".into(),
                workspace_path: Some(input.cwd),
                model_name: Some(input.model),
                task: None,
                reasoning_effort: None,
            }),
        ),
    ]))
}

fn stop_events(
    bytes: &[u8],
    capture_agent_output: bool,
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    let input: StopInput = serde_json::from_slice(bytes)?;
    validate_event_name(&input.hook_event_name, "Stop")?;
    if is_subagent(input.agent_id.as_deref(), input.agent_type.as_deref()) {
        return Ok(AgentEventCaptureDisposition::IgnoredSubagent);
    }
    validate_common(&input.session_id, &input.turn_id, &input.cwd, &input.model)?;
    let (conversation_key, run_key) = correlation_keys(&input.session_id, &input.turn_id)?;
    let now = unix_time_ms()?;
    let occurrence_id = uuid::Uuid::new_v4().to_string();
    let correlation = correlation(conversation_key, run_key.clone());
    let mut events = Vec::new();
    let content_available = input.last_assistant_message.is_some();
    let message = input.last_assistant_message.unwrap_or_default();
    let output_hash = content_available.then(|| crate::agent::source_hash_sha256(&message));
    if capture_agent_output {
        events.push(envelope(
            derive_agent_event_id(AgentEventKind::OutputProduced, &[&run_key, &occurrence_id])?,
            now,
            correlation.clone(),
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: message,
                content_mode: ResultContentMode::RedactedExcerpt,
                content_available,
                source_hash: output_hash.clone(),
                capture_generation: None,
            }),
        ));
    }
    events.push(envelope(
        derive_agent_event_id(AgentEventKind::RunSettling, &[&run_key, &occurrence_id])?,
        now,
        correlation,
        AgentEventPayloadV2::RunSettling(RunSettlingV2 {
            reason: SettlingReason::AgentStopHook,
            codex_usage: None,
        }),
    ));
    Ok(AgentEventCaptureDisposition::Recorded(events))
}

fn permission_request_events(
    bytes: &[u8],
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    let input: PermissionRequestInput = serde_json::from_slice(bytes)?;
    validate_event_name(&input.hook_event_name, "PermissionRequest")?;
    if is_subagent(input.agent_id.as_deref(), input.agent_type.as_deref()) {
        return Ok(AgentEventCaptureDisposition::IgnoredSubagent);
    }
    validate_common(&input.session_id, &input.turn_id, &input.cwd, &input.model)?;
    validate_required("tool_name", &input.tool_name)?;
    let (conversation_key, run_key) = correlation_keys(&input.session_id, &input.turn_id)?;
    // Each helper invocation is an observation, not an upstream request ID.
    // Once persisted, replay uses the envelope's unchanged occurrence identity.
    let occurrence_id = uuid::Uuid::new_v4().to_string();
    let now = unix_time_ms()?;
    let safe_summary = safe_permission_summary(&input.tool_name);
    Ok(AgentEventCaptureDisposition::Recorded(vec![envelope(
        derive_agent_event_id(
            AgentEventKind::AttentionRequired,
            &[&run_key, &occurrence_id],
        )?,
        now,
        correlation(conversation_key, run_key),
        AgentEventPayloadV2::AttentionRequired(AttentionRequiredV2 {
            attention_kind: AttentionKind::Permission,
            safe_summary,
            // The durable outbox applies its 30-minute TTL from the captured
            // occurrence time. Reading the same envelope never renews it.
            expires_at: None,
        }),
    )]))
}

fn safe_permission_summary(tool_name: &str) -> String {
    let normalized = tool_name.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "bash" | "shell" | "exec" | "exec_command" | "command" | "powershell" | "shell_command" => {
            "Shell / command execution".to_owned()
        }
        "edit" | "write" | "write_file" | "apply_patch" | "delete" | "delete_file" | "move"
        | "move_file" => "File modification".to_owned(),
        "read" | "read_file" | "open" | "view" | "view_file" | "view_image" => {
            "File access".to_owned()
        }
        "web" | "fetch" | "webfetch" | "http" | "network" | "search" | "web_search"
        | "search_query" => "Network access".to_owned(),
        _ => "Agent operation".to_owned(),
    }
}

fn envelope(
    event_id: String,
    now: i64,
    correlation: AgentCorrelationV2,
    payload: AgentEventPayloadV2,
) -> AgentEventEnvelopeV2 {
    AgentEventEnvelopeV2 {
        schema_version: AGENT_EVENT_SCHEMA_VERSION,
        source: source_descriptor(),
        event_id,
        occurred_at: now,
        observed_at: now,
        correlation,
        metadata: None,
        payload,
    }
}

fn correlation(conversation_key: String, run_key: String) -> AgentCorrelationV2 {
    AgentCorrelationV2 {
        conversation_key: Some(conversation_key),
        run_key: Some(run_key),
        parent_run_key: None,
        agent_id: None,
        agent_role: None,
    }
}

fn correlation_keys(
    session_id: &str,
    turn_id: &str,
) -> Result<(String, String), AgentEventCaptureError> {
    let kind = AgentKind::new(CODEX_AGENT_KIND).expect("fixed Codex agent kind must be valid");
    Ok((
        derive_agent_conversation_key(&kind, CODEX_SOURCE_INSTANCE_ID, session_id)?,
        derive_agent_run_key(&kind, CODEX_SOURCE_INSTANCE_ID, session_id, turn_id)?,
    ))
}

fn validate_event_name(value: &str, expected: &str) -> Result<(), AgentEventCaptureError> {
    if value == expected {
        Ok(())
    } else {
        Err(AgentEventCaptureError::InvalidInput(format!(
            "hook_event_name must be {expected}"
        )))
    }
}

fn validate_common(
    session_id: &str,
    turn_id: &str,
    cwd: &str,
    model: &str,
) -> Result<(), AgentEventCaptureError> {
    validate_opaque_identifier("session_id", session_id)?;
    validate_opaque_identifier("turn_id", turn_id)?;
    validate_required("cwd", cwd)?;
    validate_required("model", model)
}

/// Hook identifiers are host-owned opaque strings. They are intentionally not
/// parsed as UUIDs, while still being bounded before entering key derivation.
fn validate_opaque_identifier(name: &str, value: &str) -> Result<(), AgentEventCaptureError> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(AgentEventCaptureError::InvalidInput(format!(
            "{name} must be a bounded opaque ID"
        )));
    }
    Ok(())
}

fn validate_required(name: &str, value: &str) -> Result<(), AgentEventCaptureError> {
    if value.trim().is_empty() {
        Err(AgentEventCaptureError::InvalidInput(format!(
            "{name} must not be empty"
        )))
    } else {
        Ok(())
    }
}

fn is_subagent(agent_id: Option<&str>, agent_type: Option<&str>) -> bool {
    agent_id.is_some() || agent_type.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn common(event: &str) -> serde_json::Map<String, Value> {
        let Value::Object(map) = json!({
            "session_id": "session-1",
            "turn_id": "turn-1",
            "cwd": "C:/workspace/example",
            "model": "gpt-5.6",
            "hook_event_name": event
        }) else {
            unreachable!()
        };
        map
    }

    #[test]
    fn user_prompt_creates_prompt_then_run_started_with_shared_correlation() {
        let mut value = common("UserPromptSubmit");
        value.insert("prompt".into(), json!("write the implementation"));
        let result = events_from_hook_json(
            CodexHookEvent::UserPrompt,
            &serde_json::to_vec(&Value::Object(value)).unwrap(),
        )
        .unwrap();
        let AgentEventCaptureDisposition::Recorded(events) = result else {
            panic!("expected recorded events")
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].source.source_id, CODEX_AGENT_EVENT_SOURCE_ID);
        assert!(matches!(
            events[0].payload,
            AgentEventPayloadV2::PromptSubmitted(_)
        ));
        assert!(matches!(
            events[1].payload,
            AgentEventPayloadV2::RunStarted(_)
        ));
        assert_eq!(events[0].correlation, events[1].correlation);
    }

    #[test]
    fn changed_prompt_in_the_same_turn_has_a_distinct_run_started_event_id() {
        let event = |prompt: &str| {
            let mut value = common("UserPromptSubmit");
            value.insert("prompt".into(), json!(prompt));
            let AgentEventCaptureDisposition::Recorded(events) = events_from_hook_json(
                CodexHookEvent::UserPrompt,
                &serde_json::to_vec(&Value::Object(value)).unwrap(),
            )
            .unwrap() else {
                panic!("expected recorded events")
            };
            events[1].clone()
        };

        let first = event("first prompt");
        let replay = event("first prompt");
        let changed = event("second prompt");
        assert_eq!(first.event_id, replay.event_id);
        assert_eq!(
            first.payload_hash().unwrap(),
            replay.payload_hash().unwrap()
        );
        assert_ne!(first.event_id, changed.event_id);
        assert_eq!(
            first.payload_hash().unwrap(),
            changed.payload_hash().unwrap()
        );
    }

    #[test]
    fn accepts_bounded_non_uuid_hook_identifiers_and_rejects_control_or_oversize_ids() {
        let mut valid = common("UserPromptSubmit");
        valid.insert("session_id".into(), json!("desktop/session:opaque"));
        valid.insert("turn_id".into(), json!("turn-opaque_42"));
        valid.insert("prompt".into(), json!("task"));
        assert!(matches!(
            events_from_hook_json(
                CodexHookEvent::UserPrompt,
                &serde_json::to_vec(&valid).unwrap()
            ),
            Ok(AgentEventCaptureDisposition::Recorded(_))
        ));

        for invalid in ["\u{0000}bad".to_owned(), "x".repeat(129)] {
            let mut value = valid.clone();
            value.insert("session_id".into(), json!(invalid));
            assert!(matches!(
                events_from_hook_json(
                    CodexHookEvent::UserPrompt,
                    &serde_json::to_vec(&value).unwrap()
                ),
                Err(AgentEventCaptureError::InvalidInput(_))
            ));
        }
    }

    #[test]
    fn stop_writes_candidate_before_settling_and_missing_output_is_explicit() {
        let mut output = common("Stop");
        output.insert("last_assistant_message".into(), json!("final answer"));
        let AgentEventCaptureDisposition::Recorded(events) = events_from_hook_json(
            CodexHookEvent::Stop,
            &serde_json::to_vec(&Value::Object(output)).unwrap(),
        )
        .unwrap() else {
            panic!("expected recorded events")
        };
        assert!(matches!(
            events[0].payload,
            AgentEventPayloadV2::OutputProduced(_)
        ));
        assert!(matches!(
            events[1].payload,
            AgentEventPayloadV2::RunSettling(_)
        ));

        let mut empty = common("Stop");
        empty.insert("last_assistant_message".into(), Value::Null);
        let AgentEventCaptureDisposition::Recorded(events) = events_from_hook_json(
            CodexHookEvent::Stop,
            &serde_json::to_vec(&Value::Object(empty)).unwrap(),
        )
        .unwrap() else {
            panic!("expected recorded events")
        };
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].payload,
            AgentEventPayloadV2::OutputProduced(ref output) if !output.content_available
        ));
        assert!(matches!(
            events[1].payload,
            AgentEventPayloadV2::RunSettling(_)
        ));
    }

    #[test]
    fn stop_without_output_capture_never_emits_or_serializes_the_raw_output() {
        let secret = "private final answer that must not reach the inbox";
        let mut output = common("Stop");
        output.insert("last_assistant_message".into(), json!(secret));

        let AgentEventCaptureDisposition::Recorded(events) = events_from_hook_json_with_options(
            CodexHookEvent::Stop,
            false,
            &serde_json::to_vec(&Value::Object(output)).unwrap(),
        )
        .unwrap() else {
            panic!("expected recorded settling event")
        };

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].payload,
            AgentEventPayloadV2::RunSettling(_)
        ));
        assert!(!serde_json::to_string(&events).unwrap().contains(secret));
    }

    #[test]
    fn stop_without_output_capture_writes_only_settling_to_the_inbox() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        let secret = "raw answer must never be durable";
        let mut output = common("Stop");
        output.insert("last_assistant_message".into(), json!(secret));
        capture_from_reader_with_options(
            CodexHookEvent::Stop,
            false,
            std::io::Cursor::new(serde_json::to_vec(&Value::Object(output)).unwrap()),
            &inbox,
        )
        .unwrap();

        let contents = std::fs::read_to_string(inbox).unwrap();
        assert_eq!(contents.lines().count(), 1);
        assert!(!contents.contains(secret));
        let plaintext = crate::content_crypto::unprotect_text(
            crate::content_crypto::ContentPurpose::InboxEvent,
            contents.trim(),
        )
        .unwrap();
        let event: AgentEventEnvelopeV2 = serde_json::from_slice(&plaintext).unwrap();
        assert!(matches!(event.payload, AgentEventPayloadV2::RunSettling(_)));
    }

    #[test]
    fn permission_occurrences_never_hash_or_serialize_tool_input() {
        let secret = "Bearer secret-token";
        let mut first = common("PermissionRequest");
        first.insert("tool_name".into(), json!("Bash"));
        first.insert("tool_input".into(), json!({"z": 1, "a": secret}));
        let mut reordered = common("PermissionRequest");
        reordered.insert("tool_name".into(), json!("Bash"));
        reordered.insert("tool_input".into(), json!({"a": secret, "z": 1}));
        let event = |value| {
            let AgentEventCaptureDisposition::Recorded(events) = events_from_hook_json(
                CodexHookEvent::PermissionRequest,
                &serde_json::to_vec(&Value::Object(value)).unwrap(),
            )
            .unwrap() else {
                panic!("expected recorded event")
            };
            events.into_iter().next().unwrap()
        };
        let first = event(first);
        let reordered = event(reordered);
        assert_ne!(first.event_id, reordered.event_id);
        assert_eq!(
            first.payload_hash().unwrap(),
            reordered.payload_hash().unwrap()
        );
        let serialized = serde_json::to_string(&first).unwrap();
        assert!(!serialized.contains(secret));
        assert!(serialized.contains("Shell / command execution"));
        let AgentEventPayloadV2::AttentionRequired(attention) = &first.payload else {
            panic!("expected attention event")
        };
        assert_eq!(attention.expires_at, None);
    }

    #[test]
    fn permission_summary_uses_a_closed_category_and_never_echoes_tool_names() {
        assert_eq!(safe_permission_summary("Bash"), "Shell / command execution");
        assert_eq!(safe_permission_summary("apply_patch"), "File modification");
        assert_eq!(safe_permission_summary("Read"), "File access");
        assert_eq!(safe_permission_summary("WebFetch"), "Network access");
        assert_eq!(
            safe_permission_summary("unknown-Bearer private-token"),
            "Agent operation"
        );
    }

    #[test]
    fn subagents_are_ignored_for_every_hook_event() {
        for event in [
            CodexHookEvent::UserPrompt,
            CodexHookEvent::Stop,
            CodexHookEvent::PermissionRequest,
        ] {
            let event_name = match event {
                CodexHookEvent::UserPrompt => "UserPromptSubmit",
                CodexHookEvent::Stop => "Stop",
                CodexHookEvent::PermissionRequest => "PermissionRequest",
            };
            let mut value = common(event_name);
            value.insert("agent_id".into(), json!("worker-1"));
            match event {
                CodexHookEvent::UserPrompt => {
                    value.insert("prompt".into(), json!("prompt"));
                }
                CodexHookEvent::Stop => {
                    value.insert("last_assistant_message".into(), Value::Null);
                }
                CodexHookEvent::PermissionRequest => {
                    value.insert("tool_name".into(), json!("Bash"));
                    value.insert("tool_input".into(), json!({}));
                }
            }
            assert!(matches!(
                events_from_hook_json(event, &serde_json::to_vec(&Value::Object(value)).unwrap()),
                Ok(AgentEventCaptureDisposition::IgnoredSubagent)
            ));
        }
    }
}
