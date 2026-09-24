use crate::source::{derive_stable_key, AgentKind};

use super::AgentEventKind;

const CONVERSATION_KEY_NAMESPACE: &str = "agent-conversation-v1";
const RUN_KEY_NAMESPACE: &str = "agent-run-v1";
const EVENT_ID_NAMESPACE: &str = "agent-event-id-v2";
const CONTENT_HASH_NAMESPACE: &str = "agent-content-v1";
const MAX_STABLE_KEY_COMPONENT_CHARS: usize = 1_024;

pub fn derive_agent_conversation_key(
    agent_kind: &AgentKind,
    instance_id: &str,
    external_conversation_id: &str,
) -> Result<String, StableKeyError> {
    validate_component("instance_id", instance_id)?;
    validate_component("external_conversation_id", external_conversation_id)?;
    Ok(derive_stable_key(
        CONVERSATION_KEY_NAMESPACE,
        &[agent_kind.as_str(), instance_id, external_conversation_id],
    ))
}

pub fn derive_agent_run_key(
    agent_kind: &AgentKind,
    instance_id: &str,
    external_conversation_id: &str,
    external_run_id: &str,
) -> Result<String, StableKeyError> {
    validate_component("instance_id", instance_id)?;
    validate_component("external_conversation_id", external_conversation_id)?;
    validate_component("external_run_id", external_run_id)?;
    Ok(derive_stable_key(
        RUN_KEY_NAMESPACE,
        &[
            agent_kind.as_str(),
            instance_id,
            external_conversation_id,
            external_run_id,
        ],
    ))
}

pub fn derive_agent_event_id(
    kind: AgentEventKind,
    identity_components: &[&str],
) -> Result<String, StableKeyError> {
    if identity_components.is_empty() {
        return Err(StableKeyError::MissingIdentityComponents);
    }
    for component in identity_components {
        validate_component("event_identity_component", component)?;
    }
    let mut components = Vec::with_capacity(identity_components.len() + 1);
    components.push(kind.as_str());
    components.extend_from_slice(identity_components);
    Ok(derive_stable_key(EVENT_ID_NAMESPACE, &components))
}

pub fn content_hash(text: &str) -> String {
    derive_stable_key(CONTENT_HASH_NAMESPACE, &[text])
}

fn validate_component(name: &'static str, value: &str) -> Result<(), StableKeyError> {
    if value.trim().is_empty() {
        return Err(StableKeyError::Empty(name));
    }
    if value.chars().count() > MAX_STABLE_KEY_COMPONENT_CHARS {
        return Err(StableKeyError::TooLong(name));
    }
    if value.chars().any(char::is_control) {
        return Err(StableKeyError::ContainsControl(name));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StableKeyError {
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("{0} exceeds the stable key component length limit")]
    TooLong(&'static str),
    #[error("{0} contains control characters")]
    ContainsControl(&'static str),
    #[error("event identity requires at least one component")]
    MissingIdentityComponents,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_kind() -> AgentKind {
        AgentKind::new("fixture-agent").unwrap()
    }

    #[test]
    fn conversation_and_run_keys_are_stable_and_source_independent() {
        let conversation =
            derive_agent_conversation_key(&agent_kind(), "instance", "conversation").unwrap();
        let run = derive_agent_run_key(&agent_kind(), "instance", "conversation", "turn").unwrap();

        assert_eq!(
            conversation,
            derive_agent_conversation_key(&agent_kind(), "instance", "conversation").unwrap()
        );
        assert_eq!(
            run,
            derive_agent_run_key(&agent_kind(), "instance", "conversation", "turn").unwrap()
        );
        assert_ne!(conversation, run);
        assert_ne!(
            run,
            derive_agent_run_key(&agent_kind(), "other-instance", "conversation", "turn").unwrap()
        );

        let first_source =
            crate::source::SourceDescriptor::new(agent_kind(), "hook-stream", "jsonl", "instance")
                .unwrap();
        let second_source = crate::source::SourceDescriptor::new(
            agent_kind(),
            "observer-stream",
            "api",
            "instance",
        )
        .unwrap();
        assert_ne!(first_source.source_key(), second_source.source_key());
        assert_eq!(
            derive_agent_run_key(
                &first_source.agent_kind,
                &first_source.instance_id,
                "conversation",
                "turn"
            )
            .unwrap(),
            derive_agent_run_key(
                &second_source.agent_kind,
                &second_source.instance_id,
                "conversation",
                "turn"
            )
            .unwrap()
        );
    }

    #[test]
    fn event_ids_are_replay_stable_and_kind_scoped() {
        let output_hash = content_hash("candidate");
        let first =
            derive_agent_event_id(AgentEventKind::OutputProduced, &["run-key", &output_hash])
                .unwrap();
        let replay =
            derive_agent_event_id(AgentEventKind::OutputProduced, &["run-key", &output_hash])
                .unwrap();
        let settling =
            derive_agent_event_id(AgentEventKind::RunSettling, &["run-key", &output_hash]).unwrap();
        let changed = derive_agent_event_id(
            AgentEventKind::OutputProduced,
            &["run-key", &content_hash("new candidate")],
        )
        .unwrap();

        assert_eq!(first, replay);
        assert_ne!(first, settling);
        assert_ne!(first, changed);
    }

    #[test]
    fn stable_key_components_reject_controls_and_oversized_values() {
        assert!(matches!(
            derive_agent_run_key(&agent_kind(), "instance", "bad\nkey", "turn"),
            Err(StableKeyError::ContainsControl("external_conversation_id"))
        ));
        assert!(matches!(
            derive_agent_event_id(
                AgentEventKind::PromptSubmitted,
                &[&"x".repeat(MAX_STABLE_KEY_COMPONENT_CHARS + 1)]
            ),
            Err(StableKeyError::TooLong("event_identity_component"))
        ));
        assert_eq!(content_hash("same"), content_hash("same"));
        assert_ne!(content_hash("same"), content_hash("different"));
    }
}
