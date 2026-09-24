use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{
    EphemeralInboundText, InboundAcceptOutcome, InboundMessageError,
    parser::{parse_confirmation_code, parse_inbound_command},
    repository::{DurableInboundCommand, InboundCommandService},
};

const PAYLOAD_HASH_CONTEXT: &str = "promptdock-relay/inbound-message-payload/v1";

#[async_trait]
pub trait InboundMessageSink: Send + Sync {
    async fn accept(
        &self,
        message: EphemeralInboundText,
        cancellation: &CancellationToken,
    ) -> Result<InboundAcceptOutcome, InboundMessageError>;
}

/// Consumes provider input without creating executable command work. This is
/// the notification-only ingress boundary: a bounded terminal tombstone keeps
/// a cursor-save replay from becoming executable after a mode switch, while
/// the message body is dropped here.
#[derive(Clone)]
pub struct DiscardingInboundMessageSink {
    service: InboundCommandService,
}

impl DiscardingInboundMessageSink {
    pub(crate) fn new(service: InboundCommandService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl InboundMessageSink for DiscardingInboundMessageSink {
    async fn accept(
        &self,
        message: EphemeralInboundText,
        cancellation: &CancellationToken,
    ) -> Result<InboundAcceptOutcome, InboundMessageError> {
        if cancellation.is_cancelled() {
            return Err(InboundMessageError::Cancelled);
        }
        let EphemeralInboundText {
            message_key,
            sender_fingerprint,
            text,
            received_at,
        } = message;
        let payload_hash = if parse_confirmation_code(&text).is_some() {
            confirmation_payload_hash(&message_key, &sender_fingerprint)
        } else {
            payload_hash(text.as_bytes())
        };
        drop(text);

        self.service
            .discard(
                DurableInboundCommand {
                    message_key,
                    sender_fingerprint,
                    command: super::InboundCommandV5::Unknown,
                    command_json: r#"{"action":"unknown"}"#.to_owned(),
                    payload_hash,
                    received_at,
                    last_error_code: Some("NOTIFICATION_ONLY"),
                },
                cancellation,
            )
            .await
            .map(|outcome| match outcome {
                InboundAcceptOutcome::Fresh => InboundAcceptOutcome::Discarded,
                outcome => outcome,
            })
    }
}

#[derive(Clone)]
pub struct DurableInboundMessageSink {
    service: InboundCommandService,
    verifier: super::ConfirmationVerifier,
}

impl DurableInboundMessageSink {
    #[cfg(test)]
    pub fn new(service: InboundCommandService) -> Self {
        Self::new_with_verifier(service, super::ConfirmationVerifier::for_test())
    }

    pub(crate) fn new_with_verifier(
        service: InboundCommandService,
        verifier: super::ConfirmationVerifier,
    ) -> Self {
        Self { service, verifier }
    }
}

#[async_trait]
impl InboundMessageSink for DurableInboundMessageSink {
    async fn accept(
        &self,
        message: EphemeralInboundText,
        cancellation: &CancellationToken,
    ) -> Result<InboundAcceptOutcome, InboundMessageError> {
        if cancellation.is_cancelled() {
            return Err(InboundMessageError::Cancelled);
        }

        let EphemeralInboundText {
            message_key,
            sender_fingerprint,
            text,
            received_at,
        } = message;
        let confirmation_code = parse_confirmation_code(&text);
        // A six-digit code is low-entropy.  Even a one-way generic payload
        // hash would be cheaply enumerable, so confirmation attempts use an
        // idempotency hash of immutable envelope metadata instead.
        let payload_hash = confirmation_code.map_or_else(
            || payload_hash(text.as_bytes()),
            |_| confirmation_payload_hash(&message_key, &sender_fingerprint),
        );
        let parsed = match confirmation_code {
            Some(code) => self
                .service
                .resolve_confirmation_code(&sender_fingerprint, code, &self.verifier, received_at)
                .await
                .map(|confirmation_id| super::parser::ParsedInboundCommand {
                    command: super::InboundCommandV5::Confirm { confirmation_id },
                    error_code: None,
                })
                .unwrap_or_else(|error_code| super::parser::ParsedInboundCommand {
                    command: super::InboundCommandV5::Unknown,
                    error_code: Some(error_code),
                }),
            None => parse_inbound_command(&text),
        };
        let command_json = serde_json::to_string(&parsed.command).map_err(database_error)?;
        drop(text);

        self.service
            .accept(
                DurableInboundCommand {
                    message_key,
                    sender_fingerprint,
                    command: parsed.command,
                    command_json,
                    payload_hash,
                    received_at,
                    last_error_code: parsed.error_code,
                },
                cancellation,
            )
            .await
    }
}

fn payload_hash(text: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(PAYLOAD_HASH_CONTEXT);
    hasher.update(&(text.len() as u64).to_be_bytes());
    hasher.update(text);
    hasher.finalize().to_hex().to_string()
}

fn confirmation_payload_hash(message_key: &str, sender_fingerprint: &str) -> String {
    let mut hasher =
        blake3::Hasher::new_derive_key("promptdock-relay/inbound-confirmation-envelope/v1");
    for value in [message_key, sender_fingerprint] {
        hasher.update(&(value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn database_error<T>(_error: T) -> InboundMessageError {
    InboundMessageError::Database
}
