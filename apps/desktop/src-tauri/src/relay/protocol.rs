use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

pub(crate) const RELAY_API_VERSION: u32 = 1;
pub(crate) const FEATURE_NOTIFICATIONS: &str = "notifications";
pub(crate) const FEATURE_DEVICE_SCOPES: &str = "device_scopes_v1";
const MAX_SERVER_VERSION_BYTES: usize = 128;
const MAX_FEATURES: usize = 256;
const MAX_FEATURE_BYTES: usize = 128;
const MAX_ERROR_CODE_BYTES: usize = 128;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelayServerInfo {
    pub(crate) api_version: u32,
    pub(crate) server_version: String,
    pub(crate) features: Vec<String>,
}

impl RelayServerInfo {
    pub(crate) fn validate(mut self) -> Result<Self, RelayProtocolError> {
        if self.api_version != RELAY_API_VERSION {
            return Err(RelayProtocolError::UnsupportedApiVersion(self.api_version));
        }
        validate_required_bounded(
            &self.server_version,
            MAX_SERVER_VERSION_BYTES,
            RelayProtocolError::InvalidServerInfo,
        )?;
        if self.features.len() > MAX_FEATURES
            || self.features.iter().any(|feature| {
                feature.trim() != feature
                    || feature.is_empty()
                    || feature.len() > MAX_FEATURE_BYTES
                    || feature.chars().any(char::is_control)
            })
        {
            return Err(RelayProtocolError::InvalidServerInfo);
        }
        let mut seen = HashSet::with_capacity(self.features.len());
        self.features.retain(|feature| seen.insert(feature.clone()));
        Ok(self)
    }
}

#[derive(Deserialize)]
pub(super) struct RelayHealthResponse {
    status: String,
}

impl RelayHealthResponse {
    pub(super) fn is_status(&self, expected: &str) -> bool {
        self.status == expected
    }
}

#[derive(Deserialize)]
struct RelayErrorEnvelope {
    error: RelayErrorBody,
}

#[derive(Deserialize)]
struct RelayErrorBody {
    code: String,
}

pub(super) fn decode_error_code(bytes: &[u8]) -> Option<String> {
    let envelope: RelayErrorEnvelope = serde_json::from_slice(bytes).ok()?;
    let code = envelope.error.code;
    valid_required_bounded(&code, MAX_ERROR_CODE_BYTES).then_some(code)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayProbeResult {
    pub(crate) device_status: Option<super::result_protocol::RelayDeviceStatus>,
    pub(crate) server_info: RelayServerInfo,
    pub(crate) supports_notifications: bool,
    pub(crate) supports_device_scopes: bool,
}

impl RelayProbeResult {
    pub(super) fn from_server_info(server_info: RelayServerInfo) -> Self {
        let supports_notifications = server_info
            .features
            .iter()
            .any(|feature| feature == FEATURE_NOTIFICATIONS);
        let supports_device_scopes = server_info
            .features
            .iter()
            .any(|feature| feature == FEATURE_DEVICE_SCOPES);
        Self {
            device_status: None,
            server_info,
            supports_notifications,
            supports_device_scopes,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelayNotificationV1 {
    pub(crate) schema_version: i64,
    pub(crate) notification_id: String,
    pub(crate) dedupe_key: String,
    pub(crate) kind: String,
    pub(crate) priority: i64,
    pub(crate) title: String,
    pub(crate) body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) correlation_key: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) expires_at: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelayAcceptedNotification {
    pub(crate) notification_id: String,
    pub(crate) relay_status: String,
    pub(crate) existing: bool,
    pub(crate) accepted_at: i64,
}

impl RelayAcceptedNotification {
    pub(crate) fn is_valid_for(&self, expected_notification_id: Option<&str>) -> bool {
        self.relay_status == "accepted"
            && self.accepted_at >= 0
            && valid_required_bounded(&self.notification_id, 128)
            && expected_notification_id.is_none_or(|expected| self.notification_id == expected)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelayNotificationStatus {
    pub(crate) notification_id: String,
    pub(crate) status: RelayRemoteNotificationState,
    pub(crate) attempt_count: i64,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub(crate) last_error_code: Option<String>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub(crate) provider_message_id: Option<String>,
    pub(crate) updated_at: i64,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub(crate) provider_accepted_at: Option<i64>,
}

impl RelayNotificationStatus {
    pub(crate) fn is_valid_for(&self, expected_notification_id: &str) -> bool {
        self.notification_id == expected_notification_id
            && valid_required_bounded(&self.notification_id, 128)
            && self.attempt_count >= 0
            && self.updated_at >= 0
            && self
                .provider_accepted_at
                .is_none_or(|value| value >= 0 && value <= self.updated_at)
            && valid_optional_bounded(self.last_error_code.as_deref(), 128)
            && valid_optional_bounded(self.provider_message_id.as_deref(), 512)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RelayRemoteNotificationState {
    Queued,
    Delivering,
    PartialFailed,
    BlockedTargetChanged,
    DeliveryUnknown,
    PendingChannel,
    SendingChannel,
    RetryWait,
    BlockedActivation,
    BlockedReconnect,
    ProviderAccepted,
    DeadLetter,
    Expired,
    Cancelled,
}

impl RelayRemoteNotificationState {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delivering => "delivering",
            Self::PartialFailed => "partial_failed",
            Self::BlockedTargetChanged => "blocked_target_changed",
            Self::DeliveryUnknown => "delivery_unknown",
            Self::PendingChannel => "pending_channel",
            Self::SendingChannel => "sending_channel",
            Self::RetryWait => "retry_wait",
            Self::BlockedActivation => "blocked_activation",
            Self::BlockedReconnect => "blocked_reconnect",
            Self::ProviderAccepted => "provider_accepted",
            Self::DeadLetter => "dead_letter",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayProtocolError {
    InvalidServerInfo,
    UnsupportedApiVersion(u32),
}

fn validate_required_bounded(
    value: &str,
    max_bytes: usize,
    error: RelayProtocolError,
) -> Result<(), RelayProtocolError> {
    if valid_required_bounded(value, max_bytes) {
        Ok(())
    } else {
        Err(error)
    }
}

fn valid_required_bounded(value: &str, max_bytes: usize) -> bool {
    value.trim() == value
        && !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(char::is_control)
}

fn valid_optional_bounded(value: Option<&str>, max_bytes: usize) -> bool {
    value.is_none_or(|value| valid_required_bounded(value, max_bytes))
}

fn deserialize_required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_info_accepts_unknown_fields_and_requires_notification_scopes() {
        let info: RelayServerInfo = serde_json::from_value(serde_json::json!({
            "apiVersion": 1,
            "serverVersion": "1.0.0",
            "features": ["notifications", "device_scopes_v1", "notifications"],
            "serverOwnedProvider": "ignored"
        }))
        .unwrap();
        let probe = RelayProbeResult::from_server_info(info.validate().unwrap());
        assert!(probe.supports_notifications);
        assert!(probe.supports_device_scopes);
        assert_eq!(probe.server_info.features.len(), 2);
    }

    #[test]
    fn unsupported_api_version_is_rejected() {
        let info: RelayServerInfo = serde_json::from_value(serde_json::json!({
            "apiVersion": 2, "serverVersion": "2", "features": []
        }))
        .unwrap();
        assert_eq!(
            info.validate(),
            Err(RelayProtocolError::UnsupportedApiVersion(2))
        );
    }

    #[test]
    fn notification_status_is_closed_and_bounded() {
        let status: RelayNotificationStatus = serde_json::from_value(serde_json::json!({
            "notificationId": "notification-1", "status": "provider_accepted",
            "attemptCount": 1, "lastErrorCode": null, "providerMessageId": null,
            "updatedAt": 10, "providerAcceptedAt": 10
        }))
        .unwrap();
        assert!(status.is_valid_for("notification-1"));
        assert!(
            serde_json::from_value::<RelayNotificationStatus>(serde_json::json!({
                "notificationId": "notification-1", "status": "future",
                "attemptCount": 1, "lastErrorCode": null, "providerMessageId": null,
                "updatedAt": 10, "providerAcceptedAt": null
            }))
            .is_err()
        );
    }

    #[test]
    fn error_decode_returns_only_a_bounded_code() {
        assert_eq!(
            decode_error_code(br#"{"error":{"code":"INSUFFICIENT_SCOPE","message":"secret"}}"#)
                .as_deref(),
            Some("INSUFFICIENT_SCOPE")
        );
    }
}
