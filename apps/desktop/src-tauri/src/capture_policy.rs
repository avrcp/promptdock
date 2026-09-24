//! The capture policy is the helper's only persistent authorization input.
//!
//! A missing, malformed, oversized, or pre-v1 document is never interpreted as
//! the permissive default by the Hook helper.  UI callers may use `read_policy`
//! only to render a recoverable draft.
use std::{io::Read, path::Path};

use serde::{Deserialize, Serialize};

use crate::{agent_event_capture::AgentEventCaptureError, model::ResultContentMode};

const POLICY_SCHEMA_VERSION: u16 = 1;
const MAX_POLICY_BYTES: u64 = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapturePolicy {
    pub observe_turns: bool,
    pub notify_started: bool,
    pub notify_ended: bool,
    pub completion_quiet_ms: i64,
    pub result_content_mode: ResultContentMode,
    pub notify_attention: bool,
    /// Task text is separate from terminal result retention and is off by
    /// default, including in status-only mode.
    pub include_task_input: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CapturePolicyState {
    pub schema_version: u16,
    pub policy: CapturePolicy,
    pub revision: i64,
    pub capture_generation: i64,
}

impl Default for CapturePolicy {
    fn default() -> Self {
        Self {
            observe_turns: true,
            notify_started: true,
            notify_ended: true,
            completion_quiet_ms: crate::model::DEFAULT_COMPLETION_QUIET_MS,
            result_content_mode: ResultContentMode::StatusOnly,
            notify_attention: false,
            include_task_input: false,
        }
    }
}

impl CapturePolicy {
    pub(crate) fn validate(&self) -> bool {
        (crate::model::COMPLETION_QUIET_MS_MIN..=crate::model::COMPLETION_QUIET_MS_MAX)
            .contains(&self.completion_quiet_ms)
    }
}

impl Default for CapturePolicyState {
    fn default() -> Self {
        Self {
            schema_version: POLICY_SCHEMA_VERSION,
            policy: CapturePolicy::default(),
            revision: 0,
            capture_generation: 0,
        }
    }
}

fn policy_path(inbox: &Path) -> Result<std::path::PathBuf, AgentEventCaptureError> {
    inbox
        .parent()
        .map(|parent| parent.join("capture-policy.json"))
        .ok_or_else(|| AgentEventCaptureError::InvalidInput("CAPTURE_POLICY_PATH".into()))
}

/// Strict helper read.  Do not replace errors with defaults on the capture path.
pub(crate) fn try_read_policy_state(
    inbox: &Path,
) -> Result<CapturePolicyState, AgentEventCaptureError> {
    let path = policy_path(inbox)?;
    let metadata = path.metadata().map_err(|error| {
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            "CAPTURE_POLICY_MISSING"
        } else {
            "CAPTURE_POLICY_UNREADABLE"
        };
        AgentEventCaptureError::InvalidInput(code.into())
    })?;
    if metadata.len() > MAX_POLICY_BYTES {
        return Err(AgentEventCaptureError::InvalidInput(
            "CAPTURE_POLICY_TOO_LARGE".into(),
        ));
    }
    let file = std::fs::File::open(path)
        .map_err(|_| AgentEventCaptureError::InvalidInput("CAPTURE_POLICY_UNREADABLE".into()))?;
    let mut bytes = Vec::new();
    file.take(MAX_POLICY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AgentEventCaptureError::InvalidInput("CAPTURE_POLICY_UNREADABLE".into()))?;
    if bytes.len() as u64 > MAX_POLICY_BYTES {
        return Err(AgentEventCaptureError::InvalidInput(
            "CAPTURE_POLICY_TOO_LARGE".into(),
        ));
    }
    let raw: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| AgentEventCaptureError::InvalidInput("CAPTURE_POLICY_INVALID".into()))?;
    if raw.get("schemaVersion").and_then(serde_json::Value::as_u64)
        != Some(POLICY_SCHEMA_VERSION.into())
    {
        return Err(AgentEventCaptureError::InvalidInput(
            "CAPTURE_POLICY_SCHEMA_UNSUPPORTED".into(),
        ));
    }
    let state: CapturePolicyState = serde_json::from_value(raw)
        .map_err(|_| AgentEventCaptureError::InvalidInput("CAPTURE_POLICY_INVALID".into()))?;
    if state.revision < 0 || state.capture_generation < 0 || !state.policy.validate() {
        return Err(AgentEventCaptureError::InvalidInput(
            "CAPTURE_POLICY_INVALID".into(),
        ));
    }
    Ok(state)
}

/// Draft display helper. It is deliberately not used by capture or health.
pub fn read_policy(inbox: &Path) -> CapturePolicy {
    try_read_policy_state(inbox)
        .map(|state| state.policy)
        .unwrap_or_default()
}

pub(crate) fn write_policy_state(
    inbox: &Path,
    state: &CapturePolicyState,
) -> Result<(), crate::error::AppError> {
    if state.schema_version != POLICY_SCHEMA_VERSION
        || state.revision < 0
        || state.capture_generation < 0
        || !state.policy.validate()
    {
        return Err(crate::error::AppError::new(
            "CAPTURE_POLICY_WRITE",
            "采集策略无效",
        ));
    }
    let path = inbox
        .parent()
        .ok_or_else(|| crate::error::AppError::new("CAPTURE_POLICY_WRITE", "采集策略路径无效"))?
        .join("capture-policy.json");
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
        crate::error::AppError::internal(
            "CAPTURE_POLICY_WRITE",
            "无法保存采集策略",
            error.to_string(),
        )
    })?;
    crate::atomic_file::write(&path, &bytes, "CAPTURE_POLICY_WRITE", "无法保存采集策略")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_read_rejects_missing_and_legacy_documents() {
        let d = tempfile::tempdir().unwrap();
        let inbox = d.path().join("agent-events.jsonl");
        assert!(matches!(
            try_read_policy_state(&inbox),
            Err(AgentEventCaptureError::InvalidInput(code)) if code == "CAPTURE_POLICY_MISSING"
        ));
        std::fs::write(d.path().join("capture-policy.json"), br#"{"policy":{}}"#).unwrap();
        assert!(matches!(
            try_read_policy_state(&inbox),
            Err(AgentEventCaptureError::InvalidInput(code)) if code == "CAPTURE_POLICY_SCHEMA_UNSUPPORTED"
        ));
    }

    #[test]
    fn default_policy_disables_task_input_and_state_has_revision() {
        let state = CapturePolicyState::default();
        assert!(!state.policy.include_task_input);
        assert_eq!(state.revision, 0);
        assert_eq!(state.schema_version, POLICY_SCHEMA_VERSION);
    }
}
