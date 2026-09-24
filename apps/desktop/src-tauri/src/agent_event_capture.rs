use std::io::{self, Read};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent::AgentEventEnvelopeV2;

pub const MAX_AGENT_EVENT_HOOK_INPUT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum AgentEventCaptureError {
    #[error("hook input exceeds the 2 MiB agent event capture limit")]
    InputTooLarge,
    #[error("final result exceeds the 256 KiB UTF-8 content limit")]
    ResultTooLarge,
    #[error("invalid agent event hook input: {0}")]
    InvalidInput(String),
    #[error("failed to serialize agent event envelope: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("agent event inbox I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("content protection is temporarily unavailable")]
    ProtectionUnavailable,
    #[error("system clock is before the Unix epoch")]
    InvalidSystemTime,
    #[error("invalid stable Hook correlation: {0}")]
    InvalidCorrelation(#[from] crate::agent::StableKeyError),
    #[error("invalid agent event envelope: {0}")]
    InvalidEnvelope(String),
}

impl AgentEventCaptureError {
    /// Stable operational code; never stringify an error containing input text.
    pub fn code(&self) -> &str {
        match self {
            Self::InputTooLarge => "HOOK_INPUT_TOO_LARGE",
            Self::ResultTooLarge => "HOOK_RESULT_TOO_LARGE",
            Self::ProtectionUnavailable => "HOOK_PROTECTION_UNAVAILABLE",
            Self::InvalidInput(value)
                if !value.is_empty()
                    && value.len() <= 128
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') =>
            {
                value
            }
            Self::InvalidInput(_) | Self::Serialize(_) | Self::InvalidEnvelope(_) => {
                "HOOK_INPUT_INVALID"
            }
            Self::Io(error)
                if error.kind() == io::ErrorKind::TimedOut
                    || error.kind() == io::ErrorKind::WouldBlock =>
            {
                "HOOK_LOCK_TIMEOUT"
            }
            Self::Io(error) if error.to_string() == "inbox capacity reached" => {
                "HOOK_CAPACITY_EXCEEDED"
            }
            Self::Io(_) => "HOOK_DISK_WRITE_FAILED",
            Self::InvalidSystemTime => "HOOK_CLOCK_INVALID",
            Self::InvalidCorrelation(_) => "HOOK_ID_INVALID",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEventCaptureDisposition {
    Recorded(Vec<AgentEventEnvelopeV2>),
    IgnoredSubagent,
}

pub fn capture_from_reader(
    reader: impl Read,
    inbox_path: &Path,
    parse: impl FnOnce(&[u8]) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError>,
) -> Result<AgentEventCaptureDisposition, AgentEventCaptureError> {
    let bytes = read_limited(reader)?;
    let disposition = parse(&bytes)?;
    if let AgentEventCaptureDisposition::Recorded(events) = &disposition {
        append_events(inbox_path, events)?;
    }
    Ok(disposition)
}

pub fn append_events(
    inbox_path: &Path,
    events: &[AgentEventEnvelopeV2],
) -> Result<(), AgentEventCaptureError> {
    let mut batch = Vec::new();
    for event in events {
        event
            .validate()
            .map_err(|error| AgentEventCaptureError::InvalidEnvelope(error.to_string()))?;
        let protected = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::InboxEvent,
            event,
        )
        .map_err(|_| AgentEventCaptureError::ProtectionUnavailable)?;
        batch.extend_from_slice(protected.as_bytes());
        batch.push(b'\n');
    }
    if !batch.is_empty() {
        crate::inbox::append_batch(inbox_path, &batch)?;
    }
    Ok(())
}

/// Decode one purpose-bound durable inbox record.
pub fn decode_protected_event_line(
    line: &str,
) -> Result<AgentEventEnvelopeV2, AgentEventCaptureError> {
    let event: AgentEventEnvelopeV2 = crate::content_crypto::unprotect_json(
        crate::content_crypto::ContentPurpose::InboxEvent,
        line,
    )
    .map_err(|error| AgentEventCaptureError::InvalidEnvelope(error.to_string()))?;
    event
        .validate()
        .map_err(|error| AgentEventCaptureError::InvalidEnvelope(error.to_string()))?;
    Ok(event)
}

pub fn record_capture_error(inbox_path: &Path, error: &AgentEventCaptureError) {
    let line = format!(
        "{} agent_event_capture_failed code={}\n",
        unix_time_ms().unwrap_or(0),
        error.code()
    );
    let _ = crate::inbox::append_error(inbox_path, line.as_bytes());
}

pub fn unix_time_ms() -> Result<i64, AgentEventCaptureError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AgentEventCaptureError::InvalidSystemTime)?;
    i64::try_from(duration.as_millis()).map_err(|_| AgentEventCaptureError::InvalidSystemTime)
}

pub fn ensure_input_size(bytes: &[u8]) -> Result<(), AgentEventCaptureError> {
    if bytes.len() as u64 > MAX_AGENT_EVENT_HOOK_INPUT_BYTES {
        Err(AgentEventCaptureError::InputTooLarge)
    } else {
        Ok(())
    }
}

fn read_limited(mut reader: impl Read) -> Result<Vec<u8>, AgentEventCaptureError> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_AGENT_EVENT_HOOK_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure_input_size(&bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AgentCorrelationV2, AgentEventPayloadV2, PromptOrigin, PromptSubmittedV2,
        AGENT_EVENT_SCHEMA_VERSION,
    };
    use crate::source::{AgentKind, SourceDescriptor};
    use std::io::Cursor;

    fn event() -> AgentEventEnvelopeV2 {
        AgentEventEnvelopeV2 {
            metadata: None,
            schema_version: AGENT_EVENT_SCHEMA_VERSION,
            source: SourceDescriptor::new(
                AgentKind::new("fixture").unwrap(),
                "events",
                "hook_inbox",
                "default",
            )
            .unwrap(),
            event_id: "event-id".into(),
            occurred_at: 1,
            observed_at: 1,
            correlation: AgentCorrelationV2::default(),
            payload: AgentEventPayloadV2::PromptSubmitted(PromptSubmittedV2 {
                text: "prompt".into(),
                workspace_path: None,
                model_name: None,
                origin: PromptOrigin::Human,
            }),
        }
    }

    #[test]
    fn batch_append_keeps_all_records_in_one_jsonl_file() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        let outcome = capture_from_reader(Cursor::new(b"input"), &inbox, |_| {
            Ok(AgentEventCaptureDisposition::Recorded(vec![
                event(),
                event(),
            ]))
        })
        .unwrap();
        assert!(
            matches!(outcome, AgentEventCaptureDisposition::Recorded(events) if events.len() == 2)
        );
        assert_eq!(std::fs::read_to_string(inbox).unwrap().lines().count(), 2);
    }

    #[test]
    fn oversized_input_is_rejected_before_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        let result = capture_from_reader(
            Cursor::new(vec![b'x'; MAX_AGENT_EVENT_HOOK_INPUT_BYTES as usize + 1]),
            &inbox,
            |_| panic!("oversized input must not be parsed"),
        );
        assert!(matches!(result, Err(AgentEventCaptureError::InputTooLarge)));
        assert!(!inbox.exists());
    }

    #[test]
    fn error_log_never_persists_input_or_operating_system_details() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        record_capture_error(
            &inbox,
            &AgentEventCaptureError::InvalidInput("private prompt text".into()),
        );
        record_capture_error(
            &inbox,
            &AgentEventCaptureError::Io(io::Error::other("private path and content")),
        );
        let paths = crate::inbox::InboxPaths::new(&inbox);
        let log = std::fs::read_to_string(paths.errors()).unwrap();
        assert!(log.contains("code=HOOK_INPUT_INVALID"));
        assert!(log.contains("code=HOOK_DISK_WRITE_FAILED"));
        assert!(!log.contains("private"));
    }
}
