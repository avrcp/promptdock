use serde::{Deserialize, Serialize};

const MAX_SOURCE_COMPONENT_CHARS: usize = 128;
const SOURCE_KEY_NAMESPACE: &str = "agent-source-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct AgentKind(String);

impl AgentKind {
    pub fn new(value: impl Into<String>) -> Result<Self, SourceDescriptorError> {
        let value = value.into();
        validate_component("agent_kind", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct SourceDescriptor {
    pub agent_kind: AgentKind,
    pub source_id: String,
    pub source_kind: String,
    pub instance_id: String,
}

impl SourceDescriptor {
    pub fn new(
        agent_kind: AgentKind,
        source_id: impl Into<String>,
        source_kind: impl Into<String>,
        instance_id: impl Into<String>,
    ) -> Result<Self, SourceDescriptorError> {
        let descriptor = Self {
            agent_kind,
            source_id: source_id.into(),
            source_kind: source_kind.into(),
            instance_id: instance_id.into(),
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn validate(&self) -> Result<(), SourceDescriptorError> {
        validate_component("agent_kind", self.agent_kind.as_str())?;
        validate_component("source_id", &self.source_id)?;
        validate_component("source_kind", &self.source_kind)?;
        validate_component("instance_id", &self.instance_id)
    }

    pub fn source_key(&self) -> String {
        derive_stable_key(
            SOURCE_KEY_NAMESPACE,
            &[
                self.agent_kind.as_str(),
                &self.source_id,
                &self.source_kind,
                &self.instance_id,
            ],
        )
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SourceDescriptorError {
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("{0} exceeds the source component length limit")]
    TooLong(&'static str),
    #[error("{0} contains control characters")]
    ContainsControl(&'static str),
}

/// Derives an opaque, deterministic identifier from unambiguously framed inputs.
/// Length-prefixing prevents tuples such as (`a:b`, `c`) and (`a`, `b:c`) from
/// sharing the same byte representation.
pub fn derive_stable_key(namespace: &str, components: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    update_framed(&mut hasher, namespace.as_bytes());
    for component in components {
        update_framed(&mut hasher, component.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn update_framed(hasher: &mut blake3::Hasher, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(&length.to_be_bytes());
    hasher.update(value);
}

fn validate_component(name: &'static str, value: &str) -> Result<(), SourceDescriptorError> {
    if value.trim().is_empty() {
        return Err(SourceDescriptorError::Empty(name));
    }
    if value.chars().count() > MAX_SOURCE_COMPONENT_CHARS {
        return Err(SourceDescriptorError::TooLong(name));
    }
    if value.chars().any(char::is_control) {
        return Err(SourceDescriptorError::ContainsControl(name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_keys_frame_every_component() {
        assert_ne!(
            derive_stable_key("run", &["a:b", "c"]),
            derive_stable_key("run", &["a", "b:c"])
        );
        assert_ne!(
            derive_stable_key("run:a", &["b"]),
            derive_stable_key("run", &["a", "b"])
        );
    }

    #[test]
    fn source_key_covers_the_complete_descriptor() {
        let base = SourceDescriptor::new(
            AgentKind::new("fixture").unwrap(),
            "events",
            "jsonl",
            "default",
        )
        .unwrap();
        let other_instance = SourceDescriptor::new(
            AgentKind::new("fixture").unwrap(),
            "events",
            "jsonl",
            "secondary",
        )
        .unwrap();
        let other_agent = SourceDescriptor::new(
            AgentKind::new("other").unwrap(),
            "events",
            "jsonl",
            "default",
        )
        .unwrap();

        assert_eq!(base.source_key(), base.source_key());
        assert_ne!(base.source_key(), other_instance.source_key());
        assert_ne!(base.source_key(), other_agent.source_key());
    }

    #[test]
    fn source_descriptor_uses_open_validated_values() {
        let source = SourceDescriptor::new(
            AgentKind::new("future-agent").unwrap(),
            "prompt_submissions",
            "jsonl_hook_inbox",
            "default",
        )
        .unwrap();
        assert_eq!(source.agent_kind.as_str(), "future-agent");
        assert!(AgentKind::new("\n").is_err());
    }
}
