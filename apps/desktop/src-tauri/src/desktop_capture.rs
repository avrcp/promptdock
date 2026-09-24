//! Privacy boundary around the pinned Codex adapter, before durable inbox writes.
use std::path::Path;

use crate::adapters::codex::desktop_session::DesktopTurnContext;
use crate::adapters::codex::hooks::{events_from_hook_json_with_options, CodexHookEvent};
use crate::agent::{
    AgentCorrelationV2, AgentEventEnvelopeV2, AgentEventKind, AgentEventPayloadV2, RunStartedV2,
    AGENT_EVENT_SCHEMA_VERSION,
};
use crate::agent_event_capture::{AgentEventCaptureDisposition, AgentEventCaptureError};
pub use crate::capture_policy::read_policy;
pub use crate::capture_policy::CapturePolicy;
#[cfg(test)]
use crate::capture_policy::{write_policy_state, CapturePolicyState};
use crate::model::ResultContentMode;

pub fn capture(
    event: CodexHookEvent,
    bytes: &[u8],
    inbox: &Path,
) -> Result<(), AgentEventCaptureError> {
    capture_inner(event, bytes, inbox, None)
}

pub fn capture_registered(
    event: CodexHookEvent,
    bytes: &[u8],
    inbox: &Path,
    registration: Option<&str>,
) -> Result<(), AgentEventCaptureError> {
    let registration = registration
        .ok_or_else(|| AgentEventCaptureError::InvalidInput("HOOK_REGISTRATION_MISSING".into()))?;
    validate_current_registration(inbox, registration)?;
    capture_inner(event, bytes, inbox, Some(registration))
}

fn capture_inner(
    event: CodexHookEvent,
    bytes: &[u8],
    inbox: &Path,
    registration: Option<&str>,
) -> Result<(), AgentEventCaptureError> {
    // The helper may not turn an unreadable authorization document into the
    // permissive UI default. A policy error is an explicit failed capture.
    crate::agent_event_capture::ensure_input_size(bytes)?;
    let state = crate::capture_policy::try_read_policy_state(inbox)?;
    if !state.policy.observe_turns {
        return Ok(());
    }
    let identity = hook_identity(bytes)?;
    let mut classification = identity.classification(event);
    let verification = if let (Some(dir), Some(registration)) = (inbox.parent(), registration) {
        crate::hook_verification::classify(dir, registration, event, bytes)
    } else {
        crate::hook_verification::CaptureVerification::Normal
    };
    if verification.is_verification() {
        classification = SourceClassification::verification();
    }
    // Rollout files are optional, unstable host data. Reasoning effort is
    // supplemental display metadata and cannot decide whether an event exists.
    let disposition = if identity.explicit_non_target {
        // A declared agent field is not evidence of a user turn. Retain one
        // content-free diagnostic fact so the ingestor can checkpoint it, but
        // never pass the original Hook body to the regular adapter.
        AgentEventCaptureDisposition::Recorded(vec![minimal_non_target_event(&identity)?])
    } else {
        convert_with_context(
            event,
            bytes,
            &state.policy,
            &DesktopTurnContext::default(),
            None,
        )?
    };
    if let AgentEventCaptureDisposition::Recorded(events) = disposition {
        let mut events = events;
        if events.is_empty() {
            // An empty policy disposition has not durably received anything;
            // it must not become a Hook-health observation or verification
            // success signal.
            return Ok(());
        }
        for event in &mut events {
            event.metadata = Some(crate::agent::CaptureMetadata {
                policy_revision: state.revision,
                source_classification: classification.kind,
                source_reason: classification.reason.to_owned(),
                helper_build: crate::hook_verification::build_identity()
                    .unwrap_or_else(|| "promptdock_desktop_unknown".into()),
            });
            if let AgentEventPayloadV2::OutputProduced(output) = &mut event.payload {
                let source_event_id = event.event_id.clone();
                output.capture_generation = Some(state.capture_generation);
                let run_key = event.correlation.run_key.as_deref().ok_or_else(|| {
                    AgentEventCaptureError::InvalidInput(
                        "output event is missing run correlation".into(),
                    )
                })?;
                let generation = state.capture_generation.to_string();
                event.event_id = crate::agent::derive_agent_event_id(
                    crate::agent::AgentEventKind::OutputProduced,
                    &[
                        run_key,
                        &source_event_id,
                        match output.content_mode {
                            ResultContentMode::StatusOnly => "status_only",
                            ResultContentMode::RedactedExcerpt => "redacted_excerpt",
                            ResultContentMode::FullFinal => "full_final",
                        },
                        &generation,
                    ],
                )
                .map_err(|error| AgentEventCaptureError::InvalidInput(error.to_string()))?;
            }
        }
        crate::agent_event_capture::append_events(inbox, &events)?;
        // A durable receive is the only point after which observation or
        // verification may be advanced. Failure here cannot manufacture green
        // health evidence.
        if let (Some(dir), Some(registration)) = (inbox.parent(), registration) {
            crate::hook_verification::record_observation(dir, event, registration);
            crate::hook_verification::commit_classification(
                dir,
                registration,
                event,
                bytes,
                verification,
            );
        }
    }
    Ok(())
}

fn validate_current_registration(
    inbox: &Path,
    registration: &str,
) -> Result<(), AgentEventCaptureError> {
    let dir = inbox
        .parent()
        .ok_or_else(|| AgentEventCaptureError::InvalidInput("HOOK_REGISTRATION_MISSING".into()))?;
    let stored = crate::hook_health::bounded_read(&dir.join("hook-registration.json"))
        .map_err(|_| AgentEventCaptureError::InvalidInput("HOOK_REGISTRATION_UNREADABLE".into()))?;
    let stored = serde_json::from_slice::<String>(&stored)
        .map_err(|_| AgentEventCaptureError::InvalidInput("HOOK_REGISTRATION_INVALID".into()))?;
    if stored != registration {
        return Err(AgentEventCaptureError::InvalidInput(
            "HOOK_REGISTRATION_CHANGED".into(),
        ));
    }
    let target = crate::hook_health::bounded_read(&dir.join("hook-target.json"))
        .map_err(|_| AgentEventCaptureError::InvalidInput("HOOK_TARGET_UNREADABLE".into()))?;
    if target.is_empty() {
        return Err(AgentEventCaptureError::InvalidInput(
            "HOOK_REGISTRATION_INACTIVE".into(),
        ));
    }
    let target = serde_json::from_slice::<std::path::PathBuf>(&target)
        .map_err(|_| AgentEventCaptureError::InvalidInput("HOOK_TARGET_INVALID".into()))?;
    if !target.is_absolute() {
        return Err(AgentEventCaptureError::InvalidInput(
            "HOOK_REGISTRATION_INACTIVE".into(),
        ));
    }
    Ok(())
}

pub fn convert(
    event: CodexHookEvent,
    bytes: &[u8],
    policy: &CapturePolicy,
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    convert_with_context(event, bytes, policy, &DesktopTurnContext::default(), None)
}

fn convert_with_context(
    event: CodexHookEvent,
    bytes: &[u8],
    policy: &CapturePolicy,
    context: &DesktopTurnContext,
    usage: Option<crate::agent::CodexUsageSnapshot>,
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    crate::agent_event_capture::ensure_input_size(bytes)?;
    if !policy.observe_turns
        || (event == CodexHookEvent::PermissionRequest && !policy.notify_attention)
    {
        return Ok(AgentEventCaptureDisposition::Recorded(Vec::new()));
    }
    let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| AgentEventCaptureError::InvalidInput("expected object".into()))?;
    let original_cwd = object
        .get("cwd")
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    let original_model = object
        .get("model")
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    let original_prompt = policy
        .include_task_input
        .then(|| {
            object
                .get("prompt")
                .and_then(|value| value.as_str())
                .and_then(|text| {
                    crate::notification::safe_content::prepare_content(text, 4_000, true)
                })
        })
        .flatten();
    // These donor-required fields are not part of this product's retained data.
    object.insert("cwd".into(), serde_json::json!("withheld"));
    object.insert("model".into(), serde_json::json!("withheld"));
    object.insert("prompt".into(), serde_json::json!("Codex task"));
    let original_message = object
        .get("last_assistant_message")
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    match policy.result_content_mode {
        ResultContentMode::StatusOnly => {
            object.remove("last_assistant_message");
        }
        ResultContentMode::RedactedExcerpt => {
            if let Some(text) = original_message.as_deref() {
                object.insert(
                    "last_assistant_message".into(),
                    serde_json::json!(excerpt(text)),
                );
            }
        }
        ResultContentMode::FullFinal => {
            if original_message
                .as_deref()
                .is_some_and(|text| text.len() > crate::agent::MAX_RESULT_CONTENT_BYTES)
            {
                return Err(AgentEventCaptureError::ResultTooLarge);
            }
        }
    }
    // Tool arguments are neither retained nor included in attention identity.
    object.insert("tool_input".into(), serde_json::json!({}));
    let mut disposition =
        events_from_hook_json_with_options(event, true, &serde_json::to_vec(&value)?)?;
    if let AgentEventCaptureDisposition::Recorded(events) = &mut disposition {
        events.retain(|event| !matches!(event.payload, AgentEventPayloadV2::PromptSubmitted(_)));
        for event in events {
            let mut refresh_started_identity = false;
            if let AgentEventPayloadV2::RunStarted(start) = &mut event.payload {
                start.workspace_path = original_cwd.clone();
                start.model_name = original_model.clone();
                start.task = original_prompt.clone();
                start.reasoning_effort = context.reasoning_effort.clone();
                refresh_started_identity = true;
            }
            if let AgentEventPayloadV2::RunSettling(settling) = &mut event.payload {
                settling.codex_usage = usage.clone();
            }
            if let AgentEventPayloadV2::OutputProduced(output) = &mut event.payload {
                output.content_mode = policy.result_content_mode;
                if matches!(policy.result_content_mode, ResultContentMode::StatusOnly) {
                    output.text.clear();
                    output.content_available = false;
                    output.source_hash = None;
                }
            }
            if refresh_started_identity {
                let run_key = event.correlation.run_key.as_deref().ok_or_else(|| {
                    AgentEventCaptureError::InvalidInput(
                        "run-started event is missing run correlation".into(),
                    )
                })?;
                let occurrence_id = uuid::Uuid::new_v4().to_string();
                event.event_id = crate::agent::derive_agent_event_id(
                    crate::agent::AgentEventKind::RunStarted,
                    // UserPromptSubmit does not carry a Hook-invocation id. Treat
                    // every invocation as an activity occurrence, just as Stop
                    // does, instead of persisting a deterministic fingerprint of
                    // prompt text that lies beyond the retention boundary.
                    &[run_key, &occurrence_id],
                )
                .map_err(|error| AgentEventCaptureError::InvalidInput(error.to_string()))?;
            }
        }
    }
    Ok(disposition)
}

struct HookIdentity {
    session_id: String,
    turn_id: String,
    explicit_non_target: bool,
}

fn hook_identity(bytes: &[u8]) -> Result<HookIdentity, AgentEventCaptureError> {
    crate::agent_event_capture::ensure_input_size(bytes)?;
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let object = value
        .as_object()
        .ok_or_else(|| AgentEventCaptureError::InvalidInput("expected object".into()))?;
    let mut ids = Vec::with_capacity(2);
    for name in ["session_id", "turn_id"] {
        let value = object
            .get(name)
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                AgentEventCaptureError::InvalidInput(format!("{name} must be an opaque ID"))
            })?;
        validate_opaque_id(name, value)?;
        ids.push(value.to_owned());
    }
    Ok(HookIdentity {
        session_id: ids.remove(0),
        turn_id: ids.remove(0),
        explicit_non_target: ["agent_id", "agent_type"]
            .into_iter()
            .any(|name| object.get(name).is_some_and(|value| !value.is_null())),
    })
}

fn minimal_non_target_event(
    identity: &HookIdentity,
) -> Result<AgentEventEnvelopeV2, AgentEventCaptureError> {
    let source = crate::adapters::codex::hooks::source_descriptor();
    let conversation_key = crate::agent::derive_agent_conversation_key(
        &source.agent_kind,
        &source.instance_id,
        &identity.session_id,
    )?;
    let run_key = crate::agent::derive_agent_run_key(
        &source.agent_kind,
        &source.instance_id,
        &identity.session_id,
        &identity.turn_id,
    )?;
    let now = crate::agent_event_capture::unix_time_ms()?;
    Ok(AgentEventEnvelopeV2 {
        schema_version: AGENT_EVENT_SCHEMA_VERSION,
        source,
        event_id: crate::agent::derive_agent_event_id(
            AgentEventKind::RunStarted,
            &[&run_key, "explicit_non_target"],
        )?,
        occurred_at: now,
        observed_at: now,
        correlation: AgentCorrelationV2 {
            conversation_key: Some(conversation_key),
            run_key: Some(run_key),
            parent_run_key: None,
            agent_id: None,
            agent_role: None,
        },
        payload: AgentEventPayloadV2::RunStarted(RunStartedV2 {
            agent_label: "Codex non-target".into(),
            workspace_path: None,
            model_name: None,
            task: None,
            reasoning_effort: None,
        }),
        metadata: None,
    })
}

fn validate_opaque_id(name: &str, value: &str) -> Result<(), AgentEventCaptureError> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(AgentEventCaptureError::InvalidInput(format!(
            "{name} must be a bounded opaque ID"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct SourceClassification {
    kind: crate::agent::SourceClassification,
    reason: &'static str,
}

impl HookIdentity {
    fn classification(&self, event: CodexHookEvent) -> SourceClassification {
        if self.explicit_non_target {
            return SourceClassification {
                kind: crate::agent::SourceClassification::ExplicitNonTarget,
                reason: "agent_field_present",
            };
        }
        if event == CodexHookEvent::UserPrompt {
            return SourceClassification {
                kind: crate::agent::SourceClassification::UserTurn,
                reason: "supported_user_prompt",
            };
        }
        SourceClassification {
            kind: crate::agent::SourceClassification::Unknown,
            reason: "unsupported_turn_attribution",
        }
    }
}

impl SourceClassification {
    fn verification() -> Self {
        Self {
            kind: crate::agent::SourceClassification::Verification,
            reason: "verification_nonce",
        }
    }
}

pub fn excerpt(text: &str) -> String {
    let redacted = crate::notification::redaction::redact_secrets(text);
    let mut fenced = false;
    let mut lines = Vec::new();
    for line in redacted.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            if fenced {
                lines.push("[代码片段已省略]".to_owned());
            }
            continue;
        }
        if fenced {
            continue;
        }
        let safe = line
            .split_whitespace()
            .map(|word| {
                if word.contains('@')
                    || word.contains(":\\")
                    || word.starts_with('/')
                    || word.starts_with("\\\\")
                    || word.contains("file://")
                {
                    "[已脱敏]"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(safe);
    }
    let content = lines.join("\n");
    if content.chars().count() > 1200 {
        format!(
            "{}\n（结果摘录已截断）",
            content.chars().take(1180).collect::<String>()
        )
    } else {
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::agent::AgentEventEnvelopeV2;
    use crate::agent::AgentEventIngestor;
    use crate::agent::AgentEventPayloadV2;
    use crate::db::Db;
    use crate::event_runtime::AgentEventProcessor;
    use crate::model::SourceCheckpointUpdate;
    use crate::settings::SettingsStore;

    fn integration_checkpoint(event: &AgentEventEnvelopeV2, offset: i64) -> SourceCheckpointUpdate {
        SourceCheckpointUpdate {
            source: event.source.clone(),
            cursor_json: format!(r#"{{"offset":{offset}}}"#),
            source_revision: Some("desktop-capture-integration".into()),
            status: "active".into(),
            last_error_code: None,
            updated_at: event.observed_at,
        }
    }

    #[test]
    fn permission_observations_are_distinct_without_retaining_argument_identity() {
        let policy = CapturePolicy {
            notify_attention: true,
            ..CapturePolicy::default()
        };
        let capture = |command: &str| {
            let bytes = serde_json::to_vec(&serde_json::json!({
                "hook_event_name":"PermissionRequest", "session_id":"s", "turn_id":"t",
                "tool_name":"Bash", "tool_input":{"command":command}
            }))
            .unwrap();
            let AgentEventCaptureDisposition::Recorded(events) =
                convert(CodexHookEvent::PermissionRequest, &bytes, &policy).unwrap()
            else {
                panic!()
            };
            events.into_iter().next().unwrap()
        };
        let first = capture("private-command-A");
        let second = capture("private-command-B");
        assert_ne!(first.event_id, second.event_id);
        let stored = serde_json::to_vec(&first).unwrap();
        let replay: AgentEventEnvelopeV2 = serde_json::from_slice(&stored).unwrap();
        assert_eq!(replay, first);
        let text = String::from_utf8(stored).unwrap();
        assert!(!text.contains("private-command"));
        assert!(!text.contains("tool_input"));
    }

    #[test]
    fn completion_quiet_ms_defaults_and_validates_configured_bounds() {
        let mut policy = CapturePolicy::default();
        assert_eq!(
            policy.completion_quiet_ms,
            crate::model::DEFAULT_COMPLETION_QUIET_MS
        );
        assert!(policy.validate());

        for value in [
            crate::model::COMPLETION_QUIET_MS_MIN,
            crate::model::COMPLETION_QUIET_MS_MAX,
        ] {
            policy.completion_quiet_ms = value;
            assert!(policy.validate(), "quiet value {value} should be valid");
        }
        for value in [
            crate::model::COMPLETION_QUIET_MS_MIN - 1,
            crate::model::COMPLETION_QUIET_MS_MAX + 1,
        ] {
            policy.completion_quiet_ms = value;
            assert!(!policy.validate(), "quiet value {value} should be invalid");
        }
    }

    #[test]
    fn capture_policy_deserialization_rejects_incomplete_authorization() {
        let mut value = serde_json::to_value(CapturePolicy::default()).unwrap();
        value.as_object_mut().unwrap().remove("completion_quiet_ms");
        assert!(serde_json::from_value::<CapturePolicy>(value).is_err());
        assert!(serde_json::from_value::<CapturePolicy>(serde_json::json!({})).is_err());
    }

    #[test]
    fn status_only_does_not_retain_task_input_but_keeps_non_content_context() {
        let bytes = br#"{"hook_event_name":"UserPromptSubmit","session_id":"s","turn_id":"t","prompt":"private","cwd":"C:/private","model":"secret"}"#;
        let result = convert(CodexHookEvent::UserPrompt, bytes, &CapturePolicy::default()).unwrap();
        let AgentEventCaptureDisposition::Recorded(events) = result else {
            panic!()
        };
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].payload,
            AgentEventPayloadV2::RunStarted(ref started)
                if started.task.is_none()
                    && started.workspace_path.as_deref() == Some("C:/private")
                    && started.model_name.as_deref() == Some("secret")
        ));
        let stop = br#"{"hook_event_name":"Stop","session_id":"s","turn_id":"t","last_assistant_message":"private"}"#;
        let AgentEventCaptureDisposition::Recorded(events) =
            convert(CodexHookEvent::Stop, stop, &CapturePolicy::default()).unwrap()
        else {
            panic!()
        };
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].payload,
            AgentEventPayloadV2::OutputProduced(ref output)
                if output.content_mode == ResultContentMode::StatusOnly
                    && !output.content_available
                    && output.text.is_empty()
        ));
        assert!(matches!(
            events[1].payload,
            AgentEventPayloadV2::RunSettling(_)
        ));
    }

    #[test]
    fn task_input_is_an_explicit_independent_policy_choice() {
        let bytes = br#"{"hook_event_name":"UserPromptSubmit","session_id":"s","turn_id":"t","prompt":"private task","cwd":"C:/private","model":"secret"}"#;
        let policy = CapturePolicy {
            include_task_input: true,
            ..CapturePolicy::default()
        };
        let AgentEventCaptureDisposition::Recorded(events) =
            convert(CodexHookEvent::UserPrompt, bytes, &policy).unwrap()
        else {
            panic!()
        };
        assert!(matches!(
            events[0].payload,
            AgentEventPayloadV2::RunStarted(ref started)
                if started.task.as_deref() == Some("private task")
        ));
    }

    #[test]
    fn changed_desktop_prompt_in_the_same_turn_cannot_conflict_with_the_first_start() {
        let policy = CapturePolicy {
            include_task_input: true,
            ..CapturePolicy::default()
        };
        let event = |prompt: &str| {
            let input = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "s",
                "turn_id": "t",
                "prompt": prompt,
                "cwd": "C:/workspace",
                "model": "gpt-5.6"
            }))
            .unwrap();
            let AgentEventCaptureDisposition::Recorded(events) =
                convert(CodexHookEvent::UserPrompt, &input, &policy).unwrap()
            else {
                panic!("expected recorded events")
            };
            events.into_iter().next().unwrap()
        };

        let first = event("first task");
        let repeated = event("first task");
        let changed = event("steered task");
        assert_ne!(first.event_id, repeated.event_id);
        assert_eq!(
            first.payload_hash().unwrap(),
            repeated.payload_hash().unwrap()
        );
        assert_ne!(first.event_id, changed.event_id);
        assert_ne!(
            first.payload_hash().unwrap(),
            changed.payload_hash().unwrap()
        );
    }

    #[test]
    fn task_input_disabled_still_gives_each_prompt_occurrence_a_distinct_event_id() {
        let event = |prompt: &str| {
            let input = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "s",
                "turn_id": "t",
                "prompt": prompt,
                "cwd": "C:/workspace",
                "model": "gpt-5.6"
            }))
            .unwrap();
            let AgentEventCaptureDisposition::Recorded(events) = convert(
                CodexHookEvent::UserPrompt,
                &input,
                &CapturePolicy::default(),
            )
            .unwrap() else {
                panic!("expected recorded events")
            };
            events.into_iter().next().unwrap()
        };
        let first = event("first task");
        let changed = event("steered task");
        assert_ne!(first.event_id, changed.event_id);
        assert_eq!(
            first.payload_hash().unwrap(),
            changed.payload_hash().unwrap()
        );
    }

    #[test]
    fn prompts_that_only_differ_after_the_visible_limit_remain_distinct_activity() {
        let event = |suffix: char| {
            let input = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "s",
                "turn_id": "t",
                "prompt": format!("{}{}", "x".repeat(5_000), suffix),
                "cwd": "C:/workspace",
                "model": "gpt-5.6"
            }))
            .unwrap();
            let AgentEventCaptureDisposition::Recorded(events) = convert(
                CodexHookEvent::UserPrompt,
                &input,
                &CapturePolicy::default(),
            )
            .unwrap() else {
                panic!("expected recorded events")
            };
            events.into_iter().next().unwrap()
        };

        let first = event('a');
        let changed_outside_visible_limit = event('b');
        assert_eq!(
            first.payload_hash().unwrap(),
            changed_outside_visible_limit.payload_hash().unwrap()
        );
        assert_ne!(first.event_id, changed_outside_visible_limit.event_id);
    }

    #[test]
    fn every_hook_invocation_is_distinct_even_when_rollout_context_changes() {
        let input = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "s",
            "turn_id": "t",
            "prompt": "same task",
            "cwd": "C:/workspace",
            "model": "gpt-5.6"
        }))
        .unwrap();
        let capture = |context: &DesktopTurnContext| {
            let AgentEventCaptureDisposition::Recorded(events) = convert_with_context(
                CodexHookEvent::UserPrompt,
                &input,
                &CapturePolicy::default(),
                context,
                None,
            )
            .unwrap() else {
                panic!("expected recorded events")
            };
            events.into_iter().next().unwrap()
        };

        let without_context = capture(&DesktopTurnContext::default());
        let with_context = capture(&DesktopTurnContext {
            reasoning_effort: Some("high".into()),
            turn_confirmed: true,
        });
        assert_ne!(without_context.event_id, with_context.event_id);
        assert_ne!(
            without_context.payload_hash().unwrap(),
            with_context.payload_hash().unwrap()
        );
    }

    #[test]
    fn opaque_user_prompt_without_a_persisted_rollout_is_durably_received() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        write_policy_state(&inbox, &CapturePolicyState::default()).unwrap();
        let input = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "desktop.session/opaque-42",
            "turn_id": "turn:opaque-42",
            "prompt": "user task",
            "cwd":"C:/workspace",
            "model":"gpt-5.6"
        }))
        .unwrap();
        capture(CodexHookEvent::UserPrompt, &input, &inbox).unwrap();
        let events = std::fs::read_to_string(&inbox).unwrap();
        assert_eq!(events.lines().count(), 1);
        let event = crate::agent_event_capture::decode_protected_event_line(events.trim()).unwrap();
        assert!(matches!(
            event.metadata,
            Some(crate::agent::CaptureMetadata {
                source_classification: crate::agent::SourceClassification::UserTurn,
                ref source_reason,
                policy_revision: 0,
                ..
            }) if source_reason == "supported_user_prompt"
        ));
    }

    #[test]
    fn missing_or_corrupt_policy_never_falls_back_to_capture_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        let input = br#"{"hook_event_name":"UserPromptSubmit","session_id":"opaque","turn_id":"opaque","prompt":"task","cwd":"C:/workspace","model":"gpt"}"#;
        assert!(matches!(
            capture(CodexHookEvent::UserPrompt, input, &inbox),
            Err(AgentEventCaptureError::InvalidInput(code)) if code == "CAPTURE_POLICY_MISSING"
        ));
        std::fs::write(directory.path().join("capture-policy.json"), b"not-json").unwrap();
        assert!(matches!(
            capture(CodexHookEvent::UserPrompt, input, &inbox),
            Err(AgentEventCaptureError::InvalidInput(code)) if code == "CAPTURE_POLICY_INVALID"
        ));
        assert!(!inbox.exists());
    }

    #[test]
    fn registered_capture_requires_the_current_active_registration() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        write_policy_state(&inbox, &CapturePolicyState::default()).unwrap();
        let input = br#"{"hook_event_name":"UserPromptSubmit","session_id":"opaque","turn_id":"opaque","prompt":"task","cwd":"C:/workspace","model":"gpt"}"#;
        assert!(matches!(
            capture_registered(CodexHookEvent::UserPrompt, input, &inbox, None),
            Err(AgentEventCaptureError::InvalidInput(code)) if code == "HOOK_REGISTRATION_MISSING"
        ));
        std::fs::write(
            directory.path().join("hook-registration.json"),
            serde_json::to_vec("current-registration").unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("hook-target.json"),
            serde_json::to_vec(directory.path()).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            capture_registered(CodexHookEvent::UserPrompt, input, &inbox, Some("old-registration")),
            Err(AgentEventCaptureError::InvalidInput(code)) if code == "HOOK_REGISTRATION_CHANGED"
        ));
        capture_registered(
            CodexHookEvent::UserPrompt,
            input,
            &inbox,
            Some("current-registration"),
        )
        .unwrap();
        assert!(inbox.exists());
    }

    #[test]
    fn failed_inbox_append_never_records_a_durable_observation() {
        use fs2::FileExt;
        for lock_busy in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let inbox = directory.path().join("agent-events.jsonl");
            write_policy_state(&inbox, &CapturePolicyState::default()).unwrap();
            std::fs::write(
                directory.path().join("hook-registration.json"),
                serde_json::to_vec("registration").unwrap(),
            )
            .unwrap();
            std::fs::write(
                directory.path().join("hook-target.json"),
                serde_json::to_vec(directory.path()).unwrap(),
            )
            .unwrap();
            let lock = if lock_busy {
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(directory.path().join("agent-events.jsonl.lock"))
                    .unwrap();
                file.lock_exclusive().unwrap();
                Some(file)
            } else {
                std::fs::create_dir(&inbox).unwrap();
                None
            };
            let input=br#"{"hook_event_name":"UserPromptSubmit","session_id":"opaque","turn_id":"opaque","prompt":"private task"}"#;
            let error = capture_registered(
                CodexHookEvent::UserPrompt,
                input,
                &inbox,
                Some("registration"),
            )
            .unwrap_err();
            assert_eq!(
                error.code(),
                if lock_busy {
                    "HOOK_LOCK_TIMEOUT"
                } else {
                    "HOOK_DISK_WRITE_FAILED"
                }
            );
            assert!(crate::hook_verification::last_observation(
                directory.path(),
                "UserPromptSubmit"
            )
            .is_none());
            assert!(!directory.path().join("hook-observed-submit.json").exists());
            drop(lock);
        }
    }

    #[test]
    fn empty_policy_disposition_does_not_create_a_health_observation() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        write_policy_state(
            &inbox,
            &CapturePolicyState {
                policy: CapturePolicy {
                    observe_turns: false,
                    ..CapturePolicy::default()
                },
                ..CapturePolicyState::default()
            },
        )
        .unwrap();
        std::fs::write(
            directory.path().join("hook-registration.json"),
            serde_json::to_vec("current-registration").unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("hook-target.json"),
            serde_json::to_vec(directory.path()).unwrap(),
        )
        .unwrap();
        let input = br#"{"hook_event_name":"UserPromptSubmit","session_id":"opaque","turn_id":"opaque","prompt":"task","cwd":"C:/workspace","model":"gpt"}"#;

        capture_registered(
            CodexHookEvent::UserPrompt,
            input,
            &inbox,
            Some("current-registration"),
        )
        .unwrap();

        assert!(!inbox.exists());
        assert!(!directory.path().join("hook-observed-submit.json").exists());
    }
    #[test]
    fn start_notification_switch_does_not_disable_observation() {
        let policy = CapturePolicy {
            notify_started: false,
            ..CapturePolicy::default()
        };
        let input = br#"{"hook_event_name":"UserPromptSubmit","session_id":"s","turn_id":"t"}"#;
        let AgentEventCaptureDisposition::Recorded(events) =
            convert(CodexHookEvent::UserPrompt, input, &policy).unwrap()
        else {
            panic!()
        };
        assert_eq!(events.len(), 1);
    }
    #[test]
    fn excerpt_is_bounded_and_redacts_sensitive_shapes() {
        let safe = excerpt("Bearer synthetic-secret\nuser@example.test C:\\private\\file /home/private\n```rust\nprivate_code\n```\n正常结果");
        for forbidden in [
            "synthetic-secret",
            "user@example",
            "private",
            "private_code",
        ] {
            assert!(!safe.contains(forbidden));
        }
        assert!(safe.contains("正常结果"));
        assert!(excerpt(&"字".repeat(5000)).chars().count() <= 1200);
    }

    #[test]
    fn full_final_preserves_exact_utf8_and_distinguishes_empty_from_missing() {
        let exact = "中文 English 👩🏽‍💻 e\u{301}\r\n\r\n\t```rust\r\n  let p = \\\"C:\\\\fake\\\\x\\\";  \\r\\n```\r\nhttps://example.test/a user@example.test AKIAFAKESYNTHETIC";
        let policy = CapturePolicy {
            result_content_mode: ResultContentMode::FullFinal,
            ..CapturePolicy::default()
        };
        let input = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "s",
            "turn_id": "t",
            "last_assistant_message": exact
        }))
        .unwrap();
        let AgentEventCaptureDisposition::Recorded(events) =
            convert(CodexHookEvent::Stop, &input, &policy).unwrap()
        else {
            panic!()
        };
        let AgentEventPayloadV2::OutputProduced(output) = &events[0].payload else {
            panic!()
        };
        assert_eq!(output.text.as_bytes(), exact.as_bytes());
        assert_eq!(output.content_mode, ResultContentMode::FullFinal);
        assert_eq!(
            output.source_hash.as_deref(),
            Some(crate::agent::source_hash_sha256(exact).as_str())
        );

        for (value, expected_available) in [
            (serde_json::Value::String(String::new()), true),
            (serde_json::Value::Null, false),
        ] {
            let input = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "Stop", "session_id": "s", "turn_id": "t",
                "last_assistant_message": value
            }))
            .unwrap();
            let AgentEventCaptureDisposition::Recorded(events) =
                convert(CodexHookEvent::Stop, &input, &policy).unwrap()
            else {
                panic!()
            };
            let AgentEventPayloadV2::OutputProduced(output) = &events[0].payload else {
                panic!()
            };
            assert_eq!(output.content_available, expected_available);
            assert_eq!(output.source_hash.is_some(), expected_available);
        }
    }

    #[test]
    fn full_final_rejects_over_256_kib_without_truncation() {
        let input = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "Stop", "session_id": "s", "turn_id": "t",
            "last_assistant_message": "界".repeat(crate::agent::MAX_RESULT_CONTENT_BYTES / 3 + 1)
        }))
        .unwrap();
        let policy = CapturePolicy {
            result_content_mode: ResultContentMode::FullFinal,
            ..CapturePolicy::default()
        };
        assert!(matches!(
            convert(CodexHookEvent::Stop, &input, &policy),
            Err(AgentEventCaptureError::ResultTooLarge)
        ));
    }

    #[test]
    fn each_stop_occurrence_is_distinct_across_same_and_new_generations() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        let policy = CapturePolicy {
            result_content_mode: ResultContentMode::RedactedExcerpt,
            ..CapturePolicy::default()
        };
        let input = br#"{"hook_event_name":"Stop","session_id":"s","turn_id":"t","last_assistant_message":"same result"}"#;

        for generation in [7, 7, 8] {
            write_policy_state(
                &inbox,
                &CapturePolicyState {
                    schema_version: 1,
                    policy: policy.clone(),
                    revision: 0,
                    capture_generation: generation,
                },
            )
            .unwrap();
            capture(CodexHookEvent::Stop, input, &inbox).unwrap();
        }

        let events = std::fs::read_to_string(&inbox)
            .unwrap()
            .lines()
            .map(|line| {
                let plaintext = crate::content_crypto::unprotect_text(
                    crate::content_crypto::ContentPurpose::InboxEvent,
                    line,
                )
                .unwrap();
                serde_json::from_slice::<crate::agent::AgentEventEnvelopeV2>(&plaintext).unwrap()
            })
            .filter(|event| matches!(event.payload, AgentEventPayloadV2::OutputProduced(_)))
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 3);
        assert_ne!(events[0].event_id, events[1].event_id);
        assert_ne!(events[1].event_id, events[2].event_id);
        assert!(matches!(
            events[2].payload,
            AgentEventPayloadV2::OutputProduced(ref output)
                if output.capture_generation == Some(8)
        ));
    }

    #[test]
    fn repeated_missing_and_repeated_text_sequences_keep_every_stop_revision_distinct() {
        for (name, messages) in [
            ("missing-present-missing", [None, Some("X"), None]),
            ("a-b-a", [Some("A"), Some("B"), Some("A")]),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let inbox = directory.path().join(format!("{name}.jsonl"));
            write_policy_state(
                &inbox,
                &CapturePolicyState {
                    schema_version: 1,
                    policy: CapturePolicy {
                        result_content_mode: ResultContentMode::FullFinal,
                        ..CapturePolicy::default()
                    },
                    revision: 0,
                    capture_generation: 9,
                },
            )
            .unwrap();

            for message in messages {
                let input = serde_json::to_vec(&serde_json::json!({
                    "hook_event_name": "Stop",
                    "session_id": "s",
                    "turn_id": "t",
                    "last_assistant_message": message,
                }))
                .unwrap();
                capture(CodexHookEvent::Stop, &input, &inbox).unwrap();
            }

            let outputs = std::fs::read_to_string(&inbox)
                .unwrap()
                .lines()
                .map(|line| crate::agent_event_capture::decode_protected_event_line(line).unwrap())
                .filter(|event| matches!(event.payload, AgentEventPayloadV2::OutputProduced(_)))
                .collect::<Vec<_>>();
            assert_eq!(outputs.len(), 3);
            assert_ne!(outputs[0].event_id, outputs[1].event_id);
            assert_ne!(outputs[0].event_id, outputs[2].event_id);
            assert_ne!(outputs[1].event_id, outputs[2].event_id);
        }
    }

    #[test]
    fn converted_root_turns_permission_occurrences_replay_through_real_ingestor() {
        let policy = CapturePolicy {
            notify_attention: true,
            result_content_mode: ResultContentMode::FullFinal,
            ..CapturePolicy::default()
        };
        let prompt = |turn: &str| {
            serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "UserPromptSubmit", "session_id": "session", "turn_id": turn,
                "cwd": "C:/workspace", "model": "gpt", "prompt": "private task"
            }))
            .unwrap()
        };
        let recorded = |event, bytes: Vec<u8>| match convert(event, &bytes, &policy).unwrap() {
            AgentEventCaptureDisposition::Recorded(events) => events,
            _ => panic!("expected recorded Codex events"),
        };
        let mut first = recorded(CodexHookEvent::UserPrompt, prompt("turn-one"));
        let second = recorded(CodexHookEvent::UserPrompt, prompt("turn-two"));
        let permission = || {
            recorded(CodexHookEvent::PermissionRequest, serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "PermissionRequest", "session_id": "session", "turn_id": "turn-one",
            "cwd": "C:/workspace", "model": "gpt", "tool_name": "bash",
            "tool_input": {"command": "private args must not survive"}
        })).unwrap()).remove(0)
        };
        let permission_one = permission();
        let permission_two = permission();
        assert_ne!(permission_one.event_id, permission_two.event_id);
        let replay =
            AgentEventEnvelopeV2::decode_json(&serde_json::to_vec(&permission_one).unwrap())
                .unwrap();
        assert_eq!(replay, permission_one);
        let serialized = serde_json::to_string(&permission_one).unwrap();
        assert!(!serialized.contains("tool_input"));
        assert!(!serialized.contains("private args"));

        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        let db = Arc::new(Db::open_in_memory().unwrap());
        let settings = SettingsStore::new();
        crate::desktop_policy::initialize(&db, &settings, &inbox).unwrap();
        let ingestor =
            AgentEventIngestor::new(db.clone(), settings, Arc::new(|| {}), Arc::new(|| {}));
        let mut offset = 1;
        for event in first
            .drain(..)
            .chain(second)
            .chain([permission_one.clone(), permission_two.clone()])
        {
            ingestor
                .process(event.clone(), &integration_checkpoint(&event, offset))
                .unwrap();
            offset += 1;
        }
        let before = db
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT activity_revision FROM run_presentation WHERE run_key=?",
                    [permission_one.correlation.run_key.as_deref().unwrap()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(crate::error::AppError::from)
            })
            .unwrap();
        let notices_before = db
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM notification_outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(crate::error::AppError::from)
            })
            .unwrap();
        ingestor
            .process(replay.clone(), &integration_checkpoint(&replay, offset))
            .unwrap();
        let (roots, attention_events, revision, outbox) = db.with_connection(|conn| conn.query_row(
            "SELECT (SELECT COUNT(*) FROM agent_runs WHERE parent_run_key IS NULL), (SELECT COUNT(*) FROM attention_events), (SELECT activity_revision FROM run_presentation WHERE run_key=?), (SELECT COUNT(*) FROM notification_outbox)",
            [permission_one.correlation.run_key.as_deref().unwrap()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?)),
        ).map_err(crate::error::AppError::from)).unwrap();
        assert_eq!(
            (roots, attention_events, revision, outbox),
            (2, 2, before, notices_before)
        );

        // RunFinalizer has no synchronous public test entry.  This verifies
        // that Stop conversion keeps the complete Unicode/CRLF body intact;
        // finalizer and encrypted-output persistence are covered in its module.
        let stop = recorded(
            CodexHookEvent::Stop,
            serde_json::to_vec(&serde_json::json!({
                "hook_event_name":"Stop", "session_id":"session", "turn_id":"turn-one",
                "cwd":"C:/workspace", "model":"gpt", "last_assistant_message":"中文\r\nemoji 😀"
            }))
            .unwrap(),
        );
        assert!(stop.iter().any(|event| matches!(&event.payload, AgentEventPayloadV2::OutputProduced(output) if output.text == "中文\r\nemoji 😀")));
    }
}
