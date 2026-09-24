//! Typed application service for Admin device lifecycle mutations.
//!
//! HTTP handlers deliberately do not own credential or database transaction
//! logic. This service converts the auth domain's atomic results into the two
//! closed receipt shapes exposed by the Admin API and performs gateway
//! invalidation only after the durable mutation has completed.

use std::{collections::BTreeSet, fmt};

use serde::{Serialize, ser::SerializeStruct as _};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    auth::{DeviceAuthService, DeviceError, DeviceScope, PlaintextDeviceToken},
    gateway::{GatewayInvalidationReason, GatewayRegistry},
    wechat_admin_login::{AdminLoginAccessRegistry, WechatAdminLoginGrantRegistry},
    wechat_login::WechatLoginService,
};

/// Safe default grant set for ordinary devices. `job:control` is intentionally
/// absent and is granted only when a caller explicitly supplies that enum value.
pub const DEFAULT_DEVICE_SCOPES: [DeviceScope; 6] = [
    DeviceScope::NotifyWrite,
    DeviceScope::NotifyReadOwn,
    DeviceScope::ChannelRead,
    DeviceScope::ChannelManage,
    DeviceScope::GatewayConnect,
    DeviceScope::RunQuery,
];

#[derive(Clone)]
pub struct DeviceAdminService {
    auth: DeviceAuthService,
    gateway: GatewayRegistry,
    wechat_admin_authorization: Option<(
        WechatAdminLoginGrantRegistry,
        AdminLoginAccessRegistry,
        Option<WechatLoginService>,
    )>,
}

impl DeviceAdminService {
    pub fn new(auth: DeviceAuthService, gateway: GatewayRegistry) -> Self {
        Self {
            auth,
            gateway,
            wechat_admin_authorization: None,
        }
    }

    pub fn with_wechat_admin_authorization(
        mut self,
        grants: WechatAdminLoginGrantRegistry,
        access: AdminLoginAccessRegistry,
        login: Option<WechatLoginService>,
    ) -> Self {
        self.wechat_admin_authorization = Some((grants, access, login));
        self
    }

    pub async fn create_device(
        &self,
        name: &str,
        scopes: &[DeviceScope],
    ) -> Result<DeviceCredentialReceipt, DeviceAdminError> {
        let created = self
            .auth
            .create_device(name, scopes)
            .await
            .map_err(DeviceAdminError::from)?;
        Ok(DeviceCredentialReceipt {
            receipt_id: receipt_id(),
            action: DeviceCredentialAction::Create,
            device_id: created.id.to_string(),
            scopes: created.scopes,
            one_time_token: created.token,
            issued_at: created.created_at,
        })
    }

    pub async fn rotate_device_preserving_scopes(
        &self,
        device_id: &str,
    ) -> Result<DeviceCredentialReceipt, DeviceAdminError> {
        let device_uuid =
            Uuid::parse_str(device_id).map_err(|_| DeviceAdminError::InvalidDeviceId)?;
        let _guard = if let Some((grants, _, _)) = &self.wechat_admin_authorization {
            Some(grants.lifecycle_guard().await)
        } else {
            None
        };
        let rotated = self
            .auth
            .rotate_device_preserving_scopes(device_id)
            .await
            .map_err(DeviceAdminError::from)?;
        if let Some((grants, access, login)) = &self.wechat_admin_authorization {
            grants.invalidate_device(device_uuid);
            cancel_invalidated_logins(access, login.as_ref(), device_uuid);
        }
        self.gateway
            .invalidate_device(rotated.id, GatewayInvalidationReason::TokenRotated)
            .await;
        Ok(DeviceCredentialReceipt {
            receipt_id: receipt_id(),
            action: DeviceCredentialAction::Rotate,
            device_id: rotated.id.to_string(),
            scopes: rotated.scopes,
            one_time_token: rotated.token,
            issued_at: rotated.rotated_at,
        })
    }

    pub async fn set_device_enabled(
        &self,
        device_id: &str,
        enabled: bool,
    ) -> Result<DeviceActionReceipt, DeviceAdminError> {
        let device_uuid =
            Uuid::parse_str(device_id).map_err(|_| DeviceAdminError::InvalidDeviceId)?;
        let _guard = if !enabled {
            if let Some((grants, _, _)) = &self.wechat_admin_authorization {
                Some(grants.lifecycle_guard().await)
            } else {
                None
            }
        } else {
            None
        };
        let mutation = self
            .auth
            .set_device_enabled(device_id, enabled)
            .await
            .map_err(DeviceAdminError::from)?;
        if !enabled {
            if let Some((grants, access, login)) = &self.wechat_admin_authorization {
                grants.invalidate_device(device_uuid);
                cancel_invalidated_logins(access, login.as_ref(), device_uuid);
            }
            self.gateway
                .invalidate_device(mutation.id, GatewayInvalidationReason::DeviceDisabled)
                .await;
        }
        Ok(DeviceActionReceipt {
            receipt_id: receipt_id(),
            action: if enabled {
                DeviceAction::Enable
            } else {
                DeviceAction::Disable
            },
            completed_at: mutation.completed_at,
        })
    }

    pub async fn revoke_device(
        &self,
        device_id: &str,
    ) -> Result<DeviceActionReceipt, DeviceAdminError> {
        let device_uuid =
            Uuid::parse_str(device_id).map_err(|_| DeviceAdminError::InvalidDeviceId)?;
        let _guard = if let Some((grants, _, _)) = &self.wechat_admin_authorization {
            Some(grants.lifecycle_guard().await)
        } else {
            None
        };
        let mutation = self
            .auth
            .revoke_device_with_outcome(device_id)
            .await
            .map_err(DeviceAdminError::from)?;
        if let Some((grants, access, login)) = &self.wechat_admin_authorization {
            grants.invalidate_device(device_uuid);
            cancel_invalidated_logins(access, login.as_ref(), device_uuid);
        }
        self.gateway
            .invalidate_device(mutation.id, GatewayInvalidationReason::DeviceRevoked)
            .await;
        Ok(DeviceActionReceipt {
            receipt_id: receipt_id(),
            action: DeviceAction::Revoke,
            completed_at: mutation.completed_at,
        })
    }
}

fn cancel_invalidated_logins(
    access: &AdminLoginAccessRegistry,
    login: Option<&WechatLoginService>,
    device_id: Uuid,
) {
    let login_ids = access.invalidate_device(device_id);
    if let Some(login) = login {
        for login_id in login_ids {
            let _ = login.cancel(device_id, login_id);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum DeviceCredentialAction {
    #[serde(rename = "create")]
    Create,
    #[serde(rename = "rotate")]
    Rotate,
}

/// A one-time credential carrier. It is intentionally not cloneable, does not
/// expose the token as a public field, and redacts that field from `Debug`.
pub struct DeviceCredentialReceipt {
    pub receipt_id: String,
    pub action: DeviceCredentialAction,
    pub device_id: String,
    /// The committed scope snapshot is available to application callers but is
    /// not part of the strict credential receipt wire contract.
    pub scopes: BTreeSet<DeviceScope>,
    one_time_token: PlaintextDeviceToken,
    pub issued_at: i64,
}

impl DeviceCredentialReceipt {
    pub fn one_time_token(&self) -> &str {
        self.one_time_token.expose()
    }
}

impl fmt::Debug for DeviceCredentialReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceCredentialReceipt")
            .field("receipt_id", &self.receipt_id)
            .field("action", &self.action)
            .field("device_id", &self.device_id)
            .field("scopes", &self.scopes)
            .field("one_time_token", &"[REDACTED]")
            .field("issued_at", &self.issued_at)
            .finish()
    }
}

impl Serialize for DeviceCredentialReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut receipt = serializer.serialize_struct("DeviceCredentialReceipt", 5)?;
        receipt.serialize_field("receiptId", &self.receipt_id)?;
        receipt.serialize_field("action", &self.action)?;
        receipt.serialize_field("deviceId", &self.device_id)?;
        receipt.serialize_field("oneTimeToken", self.one_time_token.expose())?;
        receipt.serialize_field("issuedAt", &self.issued_at)?;
        receipt.end()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum DeviceAction {
    #[serde(rename = "device.enable")]
    Enable,
    #[serde(rename = "device.disable")]
    Disable,
    #[serde(rename = "device.revoke")]
    Revoke,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceActionReceipt {
    pub receipt_id: String,
    pub action: DeviceAction,
    pub completed_at: i64,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum DeviceAdminError {
    #[error("device name is invalid")]
    InvalidName,
    #[error("device identifier is invalid")]
    InvalidDeviceId,
    #[error("device scopes are invalid")]
    InvalidScopes,
    #[error("device was not found")]
    NotFound,
    #[error("revoked device state conflicts with this action")]
    RevokedConflict,
    #[error("device administration is unavailable")]
    Unavailable,
}

impl From<DeviceError> for DeviceAdminError {
    fn from(error: DeviceError) -> Self {
        match error {
            DeviceError::InvalidName => Self::InvalidName,
            DeviceError::InvalidId => Self::InvalidDeviceId,
            DeviceError::InvalidScopes => Self::InvalidScopes,
            DeviceError::NotFound => Self::NotFound,
            DeviceError::Revoked => Self::RevokedConflict,
            DeviceError::Random | DeviceError::Clock | DeviceError::Database => Self::Unavailable,
        }
    }
}

fn receipt_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::{
        auth::{AuthenticationError, DeviceAuthService},
        config::DatabaseConfig,
        db,
    };

    async fn service() -> (
        TempDir,
        sqlx::SqlitePool,
        DeviceAuthService,
        GatewayRegistry,
        DeviceAdminService,
    ) {
        let directory = TempDir::new().expect("temporary database directory");
        let pool = db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let auth = DeviceAuthService::new(pool.clone());
        let gateway = GatewayRegistry::new();
        let service = DeviceAdminService::new(auth.clone(), gateway.clone());
        (directory, pool, auth, gateway, service)
    }

    #[test]
    fn scopes_are_closed_and_run_control_is_never_implicit() {
        assert_eq!(DEFAULT_DEVICE_SCOPES.len(), 6);
        assert!(!DEFAULT_DEVICE_SCOPES.contains(&DeviceScope::RunControl));
        assert_eq!(
            serde_json::from_str::<DeviceScope>(r#""job:control""#).expect("explicit scope"),
            DeviceScope::RunControl
        );
        assert!(serde_json::from_str::<DeviceScope>(r#""unknown:scope""#).is_err());
    }

    #[tokio::test]
    async fn credential_receipt_is_strict_and_secret_safe_outside_its_one_time_field() {
        let (_directory, pool, auth, _gateway, service) = service().await;
        let receipt = service
            .create_device("ADMIN-DEVICE", &DEFAULT_DEVICE_SCOPES)
            .await
            .expect("create receipt");
        let token = receipt.one_time_token().to_owned();
        let secret = token.rsplit_once('.').expect("secret segment").1.to_owned();
        let debug = format!("{receipt:?}");
        assert!(!debug.contains(&token));
        assert!(!debug.contains(&secret));
        assert!(debug.contains("[REDACTED]"));

        let serialized = serde_json::to_string(&receipt).expect("credential receipt JSON");
        assert_eq!(serialized.matches(&token).count(), 1);
        let value: serde_json::Value = serde_json::from_str(&serialized).expect("receipt value");
        assert_eq!(value.as_object().expect("receipt object").keys().count(), 5);
        assert_eq!(value["action"], "create");
        assert_eq!(value["deviceId"], receipt.device_id);
        assert_eq!(value["oneTimeToken"], token);
        assert!(value.get("scopes").is_none());
        assert_eq!(
            auth.authenticate(receipt.one_time_token())
                .await
                .expect("created credential")
                .scopes,
            BTreeSet::from(DEFAULT_DEVICE_SCOPES)
        );

        let disabled = service
            .set_device_enabled(&receipt.device_id, false)
            .await
            .expect("disable receipt");
        let action_json = serde_json::to_string(&disabled).expect("action receipt JSON");
        assert!(!action_json.contains(&token));
        assert!(!action_json.contains(&secret));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&action_json).expect("action value")["action"],
            "device.disable"
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn lost_rotate_response_is_recovered_by_rotating_again_without_scope_drift() {
        let (_directory, pool, auth, _gateway, service) = service().await;
        let created = service
            .create_device(
                "ROTATION-RECOVERY",
                &[DeviceScope::GatewayConnect, DeviceScope::RunQuery],
            )
            .await
            .expect("created device");
        let device_id = created.device_id.clone();
        let original_token = created.one_time_token().to_owned();

        let lost = service
            .rotate_device_preserving_scopes(&device_id)
            .await
            .expect("first rotation");
        let lost_token = lost.one_time_token().to_owned();
        drop(lost);
        let recovered = service
            .rotate_device_preserving_scopes(&device_id)
            .await
            .expect("recovery rotation");
        let recovered_token = recovered.one_time_token().to_owned();

        assert_ne!(original_token, lost_token);
        assert_ne!(lost_token, recovered_token);
        assert_eq!(
            auth.authenticate(&original_token).await,
            Err(AuthenticationError::Unauthorized)
        );
        assert_eq!(
            auth.authenticate(&lost_token).await,
            Err(AuthenticationError::Unauthorized)
        );
        assert_eq!(
            auth.authenticate(&recovered_token)
                .await
                .expect("recovered credential")
                .scopes,
            BTreeSet::from([DeviceScope::GatewayConnect, DeviceScope::RunQuery])
        );
        assert_eq!(
            recovered.scopes,
            BTreeSet::from([DeviceScope::GatewayConnect, DeviceScope::RunQuery])
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn concurrent_preserving_rotations_are_atomic_and_keep_the_complete_scope_set() {
        let (_directory, pool, _auth, _gateway, service) = service().await;
        let expected = BTreeSet::from([
            DeviceScope::NotifyWrite,
            DeviceScope::ChannelManage,
            DeviceScope::GatewayConnect,
        ]);
        let created = service
            .create_device(
                "CONCURRENT-ROTATE",
                &expected.iter().copied().collect::<Vec<_>>(),
            )
            .await
            .expect("created device");
        let mut rotations = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let service = service.clone();
            let device_id = created.device_id.clone();
            rotations
                .spawn(async move { service.rotate_device_preserving_scopes(&device_id).await });
        }
        while let Some(rotation) = rotations.join_next().await {
            assert_eq!(
                rotation
                    .expect("rotation task")
                    .expect("atomic rotation")
                    .scopes,
                expected
            );
        }
        let stored = sqlx::query_scalar::<_, String>(
            "SELECT scope FROM device_scopes WHERE device_id=?1 ORDER BY scope",
        )
        .bind(&created.device_id)
        .fetch_all(&pool)
        .await
        .expect("stored scopes");
        let mut expected_strings = expected.iter().map(ToString::to_string).collect::<Vec<_>>();
        expected_strings.sort();
        assert_eq!(stored, expected_strings);
        pool.close().await;
    }

    #[tokio::test]
    async fn enable_disable_are_idempotent_and_revoke_wins_every_race() {
        let (_directory, pool, auth, _gateway, service) = service().await;
        let created = service
            .create_device("LIFECYCLE-RACE", &[DeviceScope::NotifyReadOwn])
            .await
            .expect("created device");
        let device_id = created.device_id.clone();
        let token = created.one_time_token().to_owned();

        service
            .set_device_enabled(&device_id, false)
            .await
            .expect("first disable");
        service
            .set_device_enabled(&device_id, false)
            .await
            .expect("idempotent disable");
        service
            .set_device_enabled(&device_id, true)
            .await
            .expect("first enable");
        service
            .set_device_enabled(&device_id, true)
            .await
            .expect("idempotent enable");
        auth.authenticate(&token)
            .await
            .expect("re-enabled credential");

        let enabling = tokio::spawn({
            let service = service.clone();
            let device_id = device_id.clone();
            async move { service.set_device_enabled(&device_id, true).await }
        });
        let revoking = tokio::spawn({
            let service = service.clone();
            let device_id = device_id.clone();
            async move { service.revoke_device(&device_id).await }
        });
        let _ = enabling.await.expect("enable task");
        revoking.await.expect("revoke task").expect("revoke wins");

        let state: (i64, Option<i64>) =
            sqlx::query_as("SELECT enabled, revoked_at FROM devices WHERE id=?1")
                .bind(&device_id)
                .fetch_one(&pool)
                .await
                .expect("device state");
        assert_eq!(state.0, 0);
        assert!(state.1.is_some());
        assert_eq!(
            service.set_device_enabled(&device_id, true).await,
            Err(DeviceAdminError::RevokedConflict)
        );
        service
            .set_device_enabled(&device_id, false)
            .await
            .expect("revoked disable remains idempotent");
        service
            .revoke_device(&device_id)
            .await
            .expect("idempotent revoke");
        assert_eq!(
            auth.authenticate(&token).await,
            Err(AuthenticationError::Revoked)
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn credential_mutations_invalidate_gateway_only_after_database_success() {
        let (_directory, pool, _auth, gateway, service) = service().await;
        let rotated = service
            .create_device("ONLINE-ROTATE", &[DeviceScope::GatewayConnect])
            .await
            .expect("rotate fixture");
        let rotated_id = Uuid::parse_str(&rotated.device_id).expect("device UUID");
        let rotated_cancellation = gateway.register_test_connection(rotated_id).await;
        service
            .rotate_device_preserving_scopes(&rotated.device_id)
            .await
            .expect("rotate");
        assert!(rotated_cancellation.is_cancelled());
        assert_eq!(gateway.connection(rotated_id).await, None);

        let disabled = service
            .create_device("ONLINE-DISABLE", &[DeviceScope::GatewayConnect])
            .await
            .expect("disable fixture");
        let disabled_cancellation = gateway
            .register_test_connection(Uuid::parse_str(&disabled.device_id).expect("device UUID"))
            .await;
        assert!(
            gateway
                .connection(Uuid::parse_str(&disabled.device_id).unwrap())
                .await
                .is_some()
        );
        service
            .set_device_enabled(&disabled.device_id, false)
            .await
            .expect("disable");
        assert!(disabled_cancellation.is_cancelled());
        assert!(
            gateway
                .connection(Uuid::parse_str(&disabled.device_id).unwrap())
                .await
                .is_none()
        );

        let revoked = service
            .create_device("ONLINE-REVOKE", &[DeviceScope::GatewayConnect])
            .await
            .expect("revoke fixture");
        let revoked_id = Uuid::parse_str(&revoked.device_id).expect("device UUID");
        let revoked_cancellation = gateway.register_test_connection(revoked_id).await;
        service
            .revoke_device(&revoked.device_id)
            .await
            .expect("revoke");
        assert!(revoked_cancellation.is_cancelled());
        assert_eq!(gateway.connection(revoked_id).await, None);

        let failed = service
            .create_device("FAILED-DISABLE", &[DeviceScope::GatewayConnect])
            .await
            .expect("failure fixture");
        let failed_id = Uuid::parse_str(&failed.device_id).expect("device UUID");
        let failed_cancellation = gateway.register_test_connection(failed_id).await;
        pool.close().await;
        assert_eq!(
            service.set_device_enabled(&failed.device_id, false).await,
            Err(DeviceAdminError::Unavailable)
        );
        assert!(!failed_cancellation.is_cancelled());
        assert!(gateway.connection(failed_id).await.is_some());
    }

    #[tokio::test]
    async fn failed_durable_mutation_preserves_login_grant() {
        let (_directory, pool, auth, gateway, service) = service().await;
        let created = service
            .create_device(
                "FAILED-ROTATE-GRANT",
                &[DeviceScope::ChannelRead, DeviceScope::ChannelManage],
            )
            .await
            .expect("failure fixture");
        let device_id = Uuid::parse_str(&created.device_id).expect("device UUID");
        let revision = auth
            .gateway_authorization_lease(device_id)
            .await
            .expect("authorization lookup")
            .expect("active authorization")
            .credential_revision;
        let (grants, access) = WechatAdminLoginGrantRegistry::new_pair();
        let grant = grants
            .issue(device_id, revision, std::time::Duration::from_secs(60))
            .expect("login grant");
        let grant_id = grant.grant_id;
        let grant_token = grant.grant_token().to_owned();
        let service = service.with_wechat_admin_authorization(grants.clone(), access, None);

        pool.close().await;
        assert!(matches!(
            service
                .rotate_device_preserving_scopes(&created.device_id)
                .await,
            Err(DeviceAdminError::Unavailable)
        ));
        assert!(grants.begin_consume(grant_id, &grant_token).is_ok());
        assert!(gateway.connection(device_id).await.is_none());
    }
}
