use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelayNotificationV1 {
    pub schema_version: i64,
    pub notification_id: String,
    pub dedupe_key: String,
    pub kind: String,
    pub priority: i64,
    pub title: String,
    pub body: String,
    pub correlation_key: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationBundleV1 {
    pub schema_version: i64,
    pub bundle_id: String,
    pub dedupe_key: String,
    pub kind: String,
    pub content_mode: String,
    pub source: String,
    pub correlation_key: String,
    pub result_revision: u64,
    pub title: String,
    pub body: String,
    pub source_hash: String,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationBundleStatus {
    Queued,
    Delivering,
    PartialFailed,
    ProviderAccepted,
    Expired,
    BlockedTargetChanged,
    DeliveryUnknown,
}

impl NotificationBundleStatus {
    pub(super) fn parse(value: &str) -> Result<Self, OutboxError> {
        match value {
            "queued" => Ok(Self::Queued),
            "delivering" => Ok(Self::Delivering),
            "partial_failed" => Ok(Self::PartialFailed),
            "provider_accepted" => Ok(Self::ProviderAccepted),
            "expired" => Ok(Self::Expired),
            "blocked_target_changed" => Ok(Self::BlockedTargetChanged),
            "delivery_unknown" => Ok(Self::DeliveryUnknown),
            _ => Err(OutboxError::Database),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationBundleReceipt {
    pub bundle_id: String,
    pub accepted_at: i64,
    pub status: NotificationBundleStatus,
    pub segment_count: u32,
    pub accepted_segments: u32,
    pub source_hash: String,
    pub updated_at: i64,
}

#[allow(dead_code)]
pub struct ClaimedBundleSegment {
    pub(super) row_id: String,
    pub(super) bundle_row_id: String,
    pub(super) claim_token: String,
    pub client_id: String,
    pub body: String,
    pub correlation_key: String,
    pub target_account_fingerprint: String,
    pub expires_at: i64,
    pub attempt_count: i64,
}

#[derive(Clone)]
pub struct InteractiveReplyV1 {
    pub schema_version: i64,
    pub notification_id: String,
    pub dedupe_key: String,
    pub priority: i64,
    pub target_account_fingerprint: String,
    pub title: String,
    pub body: String,
    /// Confirmation prompts carry a short-lived credential and must be
    /// rewritten as soon as the delivery reaches any terminal state.
    pub sensitive_body: bool,
    pub correlation_key: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
}

/// Closed request type for the process-wide Admin test notification. The
/// request intentionally carries no caller-controlled content or target.
#[derive(Clone, Copy, Debug, Default)]
pub struct AdminTest;

impl InteractiveReplyV1 {
    pub(super) fn into_parts(self) -> (RelayNotificationV1, String, bool) {
        let notification = RelayNotificationV1 {
            schema_version: self.schema_version,
            notification_id: self.notification_id,
            dedupe_key: self.dedupe_key,
            kind: "interactive_reply".to_owned(),
            priority: self.priority,
            title: self.title,
            body: self.body,
            correlation_key: self.correlation_key,
            created_at: self.created_at,
            expires_at: self.expires_at,
        };
        (
            notification,
            self.target_account_fingerprint,
            self.sensitive_body,
        )
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedNotification {
    pub notification_id: String,
    pub relay_status: &'static str,
    pub existing: bool,
    pub accepted_at: i64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationStatus {
    pub notification_id: String,
    pub status: String,
    pub attempt_count: i64,
    pub last_error_code: Option<String>,
    pub provider_message_id: Option<String>,
    pub updated_at: i64,
    pub provider_accepted_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelOutboxStatus {
    pub pending_notifications: i64,
    pub blocked_notifications: i64,
    pub last_provider_accepted_at: Option<i64>,
}

pub struct ClaimedNotification {
    pub(super) row_id: String,
    pub(super) claim_token: String,
    pub origin_key: String,
    pub target_account_fingerprint: Option<String>,
    pub notification_id: String,
    pub dedupe_key: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub correlation_key: Option<String>,
    pub priority: i64,
    pub expires_at: i64,
    pub attempt_count: i64,
}

pub struct ChannelMessage {
    pub client_id: String,
    pub body: String,
    pub correlation_key: Option<String>,
    pub target_account_fingerprint: Option<String>,
}

#[derive(Clone)]
pub enum ChannelOutcome {
    Accepted { provider_message_id: Option<String> },
    Cancelled,
    Retryable { class: RetryClass },
    BlockedActivation,
    BlockedReconnect,
    BlockedTargetChanged,
    PermanentFailure,
}

#[derive(Clone, Copy)]
pub enum RetryClass {
    Network,
    RateLimited,
    ContextChanged,
    ContextRejected,
    AmbiguousMinusTwo,
}

impl RetryClass {
    pub(super) fn error_code(self) -> &'static str {
        match self {
            Self::Network => "NETWORK",
            Self::RateLimited => "RATE_LIMITED",
            Self::ContextChanged => "CONTEXT_CHANGED",
            Self::ContextRejected => "CONTEXT_REJECTED",
            Self::AmbiguousMinusTwo => "AMBIGUOUS_MINUS_TWO",
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OutboxError {
    #[error("notification validation failed")]
    Validation,
    #[error("notification idempotency key conflicts with an existing payload")]
    IdempotencyConflict,
    #[error("notification was not found")]
    NotFound,
    #[error("notification bundle queue is full")]
    QueueFull,
    #[error("notification target is unavailable")]
    TargetUnavailable,
    #[error("notification content encryption is unavailable")]
    CryptoUnavailable,
    #[error("notification content could not be authenticated")]
    CorruptContent,
    #[error("notification database operation failed")]
    Database,
    #[error("system clock is invalid")]
    Clock,
}
