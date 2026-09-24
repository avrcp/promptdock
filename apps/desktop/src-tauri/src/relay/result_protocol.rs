//! Result-page wire contract. Responses never contain a read token or body.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RelayResultPublication {
    pub schema_version: u32,
    pub result_id: String,
    pub dedupe_key: String,
    pub kind: String,
    pub content_mode: String,
    pub source: String,
    pub correlation_key: String,
    /// Decimal text prevents a JavaScript number from changing the frozen identity.
    pub result_revision: String,
    pub title: String,
    pub body: String,
    pub source_hash: String,
    pub created_at: i64,
    pub notification_expires_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RelayResultReceipt {
    pub schema_version: u32,
    pub result_id: String,
    pub source_hash: String,
    pub accepted_at: i64,
    pub updated_at: i64,
    pub page_state: RelayResultPageState,
    pub page_expires_at: i64,
    pub notification_id: String,
    pub notification_status: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RelayResultPageState {
    Available,
    Revoked,
    Expired,
    ContentUnavailable,
}

impl RelayResultPageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::ContentUnavailable => "content_unavailable",
        }
    }
}

impl RelayResultReceipt {
    pub(crate) fn is_valid_for(&self, id: &str, hash: &str) -> bool {
        self.schema_version == 1
            && self.result_id == id
            && self.source_hash == hash
            && valid_hash(hash)
            && self.accepted_at >= 0
            && self.updated_at >= self.accepted_at
            && self.page_expires_at >= self.accepted_at
            && valid_id(&self.notification_id)
            && valid_status(&self.notification_status)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RelayResultLink {
    pub url: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RelayDeviceStatus {
    pub device_id: String,
    pub can_submit: bool,
    pub can_read_own: bool,
    pub channel_state: RelayChannelState,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RelayChannelState {
    Unknown,
    Ready,
    ActivationRequired,
    ReconnectRequired,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn valid_status(value: &str) -> bool {
    matches!(
        value,
        "pending_channel"
            | "sending_channel"
            | "retry_wait"
            | "provider_accepted"
            | "blocked_activation"
            | "blocked_reconnect"
            | "dead_letter"
            | "expired"
            | "cancelled"
            | "blocked_target_changed"
            | "delivery_unknown"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULT_PUBLICATION_FIXTURE: &str =
        include_str!("../../../../../contracts/server-http/v1/result-publication-v1.json");
    const RESULT_RECEIPT_FIXTURE: &str =
        include_str!("../../../../../contracts/server-http/v1/result-receipt-v1.json");

    #[test]
    fn vendored_result_fixtures_decode_with_frozen_hash_and_identity() {
        let publication: RelayResultPublication =
            serde_json::from_str(RESULT_PUBLICATION_FIXTURE).expect("result publication fixture");
        let receipt: RelayResultReceipt =
            serde_json::from_str(RESULT_RECEIPT_FIXTURE).expect("result receipt fixture");

        assert_eq!(
            crate::agent::source_hash_sha256(RESULT_PUBLICATION_FIXTURE),
            "55c65ad118dcc0798f5e980da3a8901e296e7e478234748e4e89a172412254fe"
        );
        assert_eq!(
            crate::agent::source_hash_sha256(RESULT_RECEIPT_FIXTURE),
            "765a35243f7e85e926995c321ca8194f0e9e2b267d9e8a0fe47e88073e204ba2"
        );
        assert_eq!(
            crate::agent::source_hash_sha256(&publication.body),
            publication.source_hash
        );
        assert_eq!(
            serde_json::to_value(&publication).expect("publication serializes"),
            serde_json::from_str::<serde_json::Value>(RESULT_PUBLICATION_FIXTURE)
                .expect("fixture JSON")
        );
        assert!(receipt.is_valid_for(&publication.result_id, &publication.source_hash));
        assert!(!receipt.is_valid_for("other-result", &publication.source_hash));
        assert!(!receipt.is_valid_for(&publication.result_id, &"b".repeat(64)));
    }

    #[test]
    fn receipt_rejects_unknown_notification_state() {
        let receipt = RelayResultReceipt {
            schema_version: 1,
            result_id: "r".into(),
            source_hash: "a".repeat(64),
            accepted_at: 1,
            updated_at: 1,
            page_state: RelayResultPageState::Available,
            page_expires_at: 2,
            notification_id: "n".into(),
            notification_status: "bad".into(),
        };
        assert!(!receipt.is_valid_for("r", &"a".repeat(64)));
    }
}
