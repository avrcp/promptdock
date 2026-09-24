//! Typed Admin application service for the bounded WeChat operator actions.
//!
//! The HTTP layer supplies no notification content or destination. Test
//! notifications are created by the typed outbox ingress, while disconnect
//! delegates to the existing process-wide login lifecycle service.

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    outbox::{AdminTest, OutboxService},
    wechat_login::WechatLoginService,
};

const TEST_RECEIPT_DOMAIN: &str = "promptdock.admin.wechat-test-receipt.v1";

#[derive(Clone)]
pub struct WechatAdminService {
    outbox: OutboxService,
    login: Option<WechatLoginService>,
}

impl WechatAdminService {
    pub fn new(outbox: OutboxService, login: Option<WechatLoginService>) -> Self {
        Self { outbox, login }
    }

    pub async fn enqueue_test(&self) -> Result<WechatTestReceipt, WechatAdminError> {
        if self.login.is_none() {
            return Err(WechatAdminError::Unavailable);
        }
        let accepted = self
            .outbox
            .enqueue_admin_test(AdminTest)
            .await
            .map_err(|_| WechatAdminError::Unavailable)?;
        Ok(WechatTestReceipt {
            receipt_id: safe_test_receipt_id(&accepted.notification_id),
            state: WechatTestState::AcceptedByRelay,
            issued_at: accepted.accepted_at,
        })
    }

    pub async fn disconnect(&self, reason: &str) -> Result<WechatActionReceipt, WechatAdminError> {
        validate_reason(reason)?;
        let login = self.login.as_ref().ok_or(WechatAdminError::Unavailable)?;
        login
            .disconnect()
            .await
            .map_err(|_| WechatAdminError::Unavailable)?;
        let completed_at = unix_timestamp_ms()?;
        Ok(WechatActionReceipt {
            receipt_id: Uuid::new_v4().to_string(),
            action: WechatAction::Disconnect,
            completed_at,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum WechatTestState {
    #[serde(rename = "accepted_by_relay")]
    AcceptedByRelay,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatTestReceipt {
    pub receipt_id: String,
    state: WechatTestState,
    pub issued_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum WechatAction {
    #[serde(rename = "wechat.disconnect")]
    Disconnect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatActionReceipt {
    pub receipt_id: String,
    action: WechatAction,
    pub completed_at: i64,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WechatAdminError {
    #[error("WeChat Admin request is invalid")]
    InvalidRequest,
    #[error("WeChat Admin action is unavailable")]
    Unavailable,
}

fn validate_reason(reason: &str) -> Result<(), WechatAdminError> {
    if (1..=120).contains(&reason.chars().count()) {
        Ok(())
    } else {
        Err(WechatAdminError::InvalidRequest)
    }
}

fn safe_test_receipt_id(notification_id: &str) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(TEST_RECEIPT_DOMAIN);
    hasher.update(notification_id.as_bytes());
    format!("wechat_test_{}", hasher.finalize().to_hex())
}

fn unix_timestamp_ms() -> Result<i64, WechatAdminError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .ok_or(WechatAdminError::Unavailable)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use relay_provider_wechat::protocol::{GetBotQrCodeResponse, GetQrCodeStatusResponse};
    use sqlx::Row as _;

    use super::*;
    use crate::{
        auth::DeviceAuthService,
        config::DatabaseConfig,
        db,
        qr_login::{PollInput, QrLoginProvider, QrLoginProviderError},
        shutdown::TaskSupervisor,
        wechat::secret_store::EncryptedFileSecretStore,
    };

    struct UnusedProvider;

    #[async_trait]
    impl QrLoginProvider for UnusedProvider {
        async fn fetch_qr(
            &self,
            _local_token_list: &[String],
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> Result<GetBotQrCodeResponse, QrLoginProviderError> {
            unreachable!("disconnect does not contact the provider")
        }

        async fn poll_status(
            &self,
            _input: &PollInput,
        ) -> Result<GetQrCodeStatusResponse, QrLoginProviderError> {
            unreachable!("disconnect does not contact the provider")
        }
    }

    #[tokio::test]
    async fn test_enqueue_maps_private_identity_to_a_safe_closed_receipt() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pool = db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let supervisor = TaskSupervisor::new();
        let login = WechatLoginService::for_test(
            EncryptedFileSecretStore::new_for_test(
                directory.path().join("wechat-connection.enc"),
                [6; 32],
            ),
            None,
            DeviceAuthService::new(pool.clone()),
            supervisor.clone(),
            Arc::new(UnusedProvider),
        );
        let service = WechatAdminService::new(OutboxService::new(pool.clone()), Some(login));

        let receipt = service.enqueue_test().await.expect("test receipt");
        assert_eq!(receipt.state, WechatTestState::AcceptedByRelay);
        assert!(receipt.receipt_id.starts_with("wechat_test_"));
        assert_eq!(receipt.receipt_id.len(), "wechat_test_".len() + 64);
        let row = sqlx::query(
            "SELECT notification_id, title, body, target_account_fingerprint
             FROM notification_outbox WHERE origin_kind='admin'",
        )
        .fetch_one(&pool)
        .await
        .expect("Admin test row");
        let raw_id = row.get::<String, _>("notification_id");
        assert_eq!(receipt.receipt_id, safe_test_receipt_id(&raw_id));
        assert!(
            !serde_json::to_string(&receipt)
                .expect("receipt JSON")
                .contains(&raw_id)
        );
        assert_eq!(row.get::<String, _>("title"), "PromptDock Relay 测试");
        assert_eq!(row.get::<String, _>("body"), "✅ PromptDock Relay 测试通知");
        assert_eq!(
            row.get::<Option<String>, _>("target_account_fingerprint"),
            None
        );
        supervisor.begin_shutdown();
        supervisor.wait().await;
        pool.close().await;
    }

    #[tokio::test]
    async fn disconnect_validates_unicode_reason_and_delegates_to_existing_service() {
        assert_eq!(validate_reason(""), Err(WechatAdminError::InvalidRequest));
        assert!(validate_reason(&"界".repeat(120)).is_ok());
        assert_eq!(
            validate_reason(&"界".repeat(121)),
            Err(WechatAdminError::InvalidRequest)
        );

        let directory = tempfile::tempdir().expect("temporary directory");
        let pool = db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let supervisor = TaskSupervisor::new();
        let login = WechatLoginService::for_test(
            EncryptedFileSecretStore::new_for_test(
                directory.path().join("wechat-connection.enc"),
                [7; 32],
            ),
            None,
            DeviceAuthService::new(pool.clone()),
            supervisor.clone(),
            Arc::new(UnusedProvider),
        );
        let service = WechatAdminService::new(OutboxService::new(pool.clone()), Some(login));
        let private_reason = "SENTINEL_PRIVATE_OPERATOR_REASON";
        let before_disconnect = unix_timestamp_ms().expect("clock before disconnect");
        let receipt = service
            .disconnect(private_reason)
            .await
            .expect("safe disconnect");
        let after_disconnect = unix_timestamp_ms().expect("clock after disconnect");
        let serialized = serde_json::to_string(&receipt).expect("receipt JSON");
        assert_eq!(receipt.action, WechatAction::Disconnect);
        assert!((before_disconnect..=after_disconnect).contains(&receipt.completed_at));
        assert!(!serialized.contains(private_reason));
        assert_eq!(serialized.matches("wechat.disconnect").count(), 1);

        supervisor.begin_shutdown();
        supervisor.wait().await;
        pool.close().await;
    }

    #[tokio::test]
    async fn disconnect_without_a_channel_fails_closed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pool = db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let service = WechatAdminService::new(OutboxService::new(pool.clone()), None);
        assert_eq!(
            service.enqueue_test().await,
            Err(WechatAdminError::Unavailable)
        );
        assert_eq!(
            service.disconnect("operator requested").await,
            Err(WechatAdminError::Unavailable)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM notification_outbox WHERE origin_kind='admin'",
            )
            .fetch_one(&pool)
            .await
            .expect("outbox count"),
            0
        );
        pool.close().await;
    }
}
