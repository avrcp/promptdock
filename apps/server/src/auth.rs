use std::{collections::BTreeSet, fmt, str::FromStr, time::SystemTime};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::TryRngCore as _;
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};
use subtle::ConstantTimeEq as _;
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const TOKEN_VERSION: &str = "pdv2";
const TOKEN_HASH_CONTEXT: &str = "promptdock-relay/device-token/v2";
const SECRET_BYTES: usize = 32;
const SECRET_TEXT_LENGTH: usize = 43;
const LAST_SEEN_INTERVAL_MS: i64 = 60_000;
const GATEWAY_AUTHORIZATION_REVISION_CONTEXT: &str =
    "promptdock-relay/gateway-authorization-revision/v1";

#[derive(Clone)]
pub struct DeviceAuthService {
    pool: SqlitePool,
    #[cfg(test)]
    authentication_barrier: Option<AuthenticationBarrier>,
    #[cfg(test)]
    rejected_authentication_barrier: Option<AuthenticationBarrier>,
}

impl DeviceAuthService {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            #[cfg(test)]
            authentication_barrier: None,
            #[cfg(test)]
            rejected_authentication_barrier: None,
        }
    }

    pub async fn create_device(
        &self,
        name: &str,
        scopes: &[DeviceScope],
    ) -> Result<CreatedDevice, DeviceError> {
        let name = validate_device_name(name)?;
        let scopes = validate_scopes(scopes)?;
        let device_id = Uuid::new_v4();
        let credential = issue_device_credential(device_id)?;
        let created_at = unix_timestamp_ms()?;

        let mut transaction = self.pool.begin().await.map_err(|_| DeviceError::Database)?;
        sqlx::query(
            "INSERT INTO devices \
             (id, name, token_hash, enabled, created_at, credential_version) \
             VALUES (?1, ?2, ?3, 1, ?4, 2)",
        )
        .bind(device_id.to_string())
        .bind(&name)
        .bind(credential.token_hash.to_vec())
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .map_err(|_| DeviceError::Database)?;
        insert_scopes(&mut transaction, device_id, &scopes).await?;
        transaction
            .commit()
            .await
            .map_err(|_| DeviceError::Database)?;

        Ok(CreatedDevice {
            id: device_id,
            name,
            created_at,
            scopes,
            token: credential.token,
        })
    }

    pub async fn rotate_device(
        &self,
        id: &str,
        scopes: &[DeviceScope],
    ) -> Result<RotatedDevice, DeviceError> {
        let id = parse_device_id(id).map_err(|_| DeviceError::InvalidId)?;
        let scopes = validate_scopes(scopes)?;
        let credential = issue_device_credential(id)?;
        let rotated_at = unix_timestamp_ms()?;
        let mut transaction = self.pool.begin().await.map_err(|_| DeviceError::Database)?;
        let result = sqlx::query("UPDATE devices SET token_hash=?1, credential_version=2, rotated_at=?2 WHERE id=?3 AND enabled=1 AND revoked_at IS NULL")
            .bind(credential.token_hash.to_vec()).bind(rotated_at).bind(id.to_string())
            .execute(&mut *transaction).await.map_err(|_| DeviceError::Database)?;
        if result.rows_affected() == 0 {
            let exists: Option<(i64, Option<i64>)> =
                sqlx::query_as("SELECT enabled, revoked_at FROM devices WHERE id=?1")
                    .bind(id.to_string())
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(|_| DeviceError::Database)?;
            return Err(if exists.is_some() {
                DeviceError::Revoked
            } else {
                DeviceError::NotFound
            });
        }
        sqlx::query("DELETE FROM device_scopes WHERE device_id=?1")
            .bind(id.to_string())
            .execute(&mut *transaction)
            .await
            .map_err(|_| DeviceError::Database)?;
        insert_scopes(&mut transaction, id, &scopes).await?;
        transaction
            .commit()
            .await
            .map_err(|_| DeviceError::Database)?;
        Ok(RotatedDevice {
            id,
            rotated_at,
            scopes,
            token: credential.token,
        })
    }

    /// Rotates a device credential without accepting a caller-supplied scope set.
    ///
    /// The credential update obtains SQLite's writer fence before the scopes are
    /// read. Both operations are then committed together, so a concurrent scope
    /// replacement can only linearize before or after this rotation and the
    /// returned scope snapshot always belongs to the committed credential state.
    pub async fn rotate_device_preserving_scopes(
        &self,
        id: &str,
    ) -> Result<RotatedDevice, DeviceError> {
        let id = parse_device_id(id).map_err(|_| DeviceError::InvalidId)?;
        let credential = issue_device_credential(id)?;
        let rotated_at = unix_timestamp_ms()?;
        let mut transaction = self.pool.begin().await.map_err(|_| DeviceError::Database)?;
        let result = sqlx::query(
            "UPDATE devices \
             SET token_hash=?1, credential_version=2, rotated_at=?2 \
             WHERE id=?3 AND revoked_at IS NULL",
        )
        .bind(credential.token_hash.to_vec())
        .bind(rotated_at)
        .bind(id.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(|_| DeviceError::Database)?;
        if result.rows_affected() == 0 {
            let row: Option<(Option<i64>,)> =
                sqlx::query_as("SELECT revoked_at FROM devices WHERE id=?1")
                    .bind(id.to_string())
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(|_| DeviceError::Database)?;
            return Err(if row.is_some() {
                DeviceError::Revoked
            } else {
                DeviceError::NotFound
            });
        }
        let scopes = load_scopes(&mut transaction, id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| DeviceError::Database)?;
        Ok(RotatedDevice {
            id,
            rotated_at,
            scopes,
            token: credential.token,
        })
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceSummary>, DeviceError> {
        let rows = sqlx::query(
            "SELECT d.id, d.name, d.enabled, d.created_at, d.last_seen_at, d.revoked_at, \
                    d.credential_version, d.rotated_at, \
                    COALESCE(GROUP_CONCAT(s.scope, ','), '') AS scopes \
             FROM devices d \
             LEFT JOIN device_scopes s ON s.device_id = d.id \
             GROUP BY d.id, d.name, d.enabled, d.created_at, d.last_seen_at, d.revoked_at, \
                      d.credential_version, d.rotated_at \
             ORDER BY d.created_at ASC, d.id ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| DeviceError::Database)?;

        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            let id = row
                .try_get::<String, _>("id")
                .map_err(|_| DeviceError::Database)?;
            let scope_text = row
                .try_get::<String, _>("scopes")
                .map_err(|_| DeviceError::Database)?;
            let scopes = scope_text
                .split(',')
                .filter(|value| !value.is_empty())
                .map(|value| DeviceScope::from_str(value).map_err(|_| DeviceError::Database))
                .collect::<Result<_, _>>()?;
            summaries.push(DeviceSummary {
                id,
                name: row
                    .try_get::<String, _>("name")
                    .map_err(|_| DeviceError::Database)?,
                enabled: row
                    .try_get::<i64, _>("enabled")
                    .map_err(|_| DeviceError::Database)?
                    == 1,
                created_at: row
                    .try_get("created_at")
                    .map_err(|_| DeviceError::Database)?,
                last_seen_at: row
                    .try_get("last_seen_at")
                    .map_err(|_| DeviceError::Database)?,
                revoked_at: row
                    .try_get("revoked_at")
                    .map_err(|_| DeviceError::Database)?,
                credential_version: row
                    .try_get("credential_version")
                    .map_err(|_| DeviceError::Database)?,
                rotated_at: row
                    .try_get("rotated_at")
                    .map_err(|_| DeviceError::Database)?,
                scopes,
            });
        }
        Ok(summaries)
    }

    pub async fn revoke_device(&self, id: &str) -> Result<(), DeviceError> {
        self.revoke_device_with_outcome(id).await.map(|_| ())
    }

    /// Irreversibly revokes a device and returns only non-secret mutation data.
    pub async fn revoke_device_with_outcome(
        &self,
        id: &str,
    ) -> Result<DeviceLifecycleMutation, DeviceError> {
        let id = parse_device_id(id).map_err(|_| DeviceError::InvalidId)?;
        let revoked_at = unix_timestamp_ms()?;
        let result = sqlx::query(
            "UPDATE devices \
             SET enabled = 0, revoked_at = COALESCE(revoked_at, ?1) \
             WHERE id = ?2",
        )
        .bind(revoked_at)
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|_| DeviceError::Database)?;
        if result.rows_affected() == 0 {
            return Err(DeviceError::NotFound);
        }
        Ok(DeviceLifecycleMutation {
            id,
            completed_at: revoked_at,
        })
    }

    /// Sets the reversible enabled flag under the same SQLite writer fence used
    /// by authentication. Disabling an already-revoked device is an idempotent
    /// success, while enabling it is always rejected.
    pub async fn set_device_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<DeviceLifecycleMutation, DeviceError> {
        let id = parse_device_id(id).map_err(|_| DeviceError::InvalidId)?;
        let completed_at = unix_timestamp_ms()?;
        let mut transaction = self.pool.begin().await.map_err(|_| DeviceError::Database)?;
        let result =
            sqlx::query("UPDATE devices SET enabled=?1 WHERE id=?2 AND revoked_at IS NULL")
                .bind(i64::from(enabled))
                .bind(id.to_string())
                .execute(&mut *transaction)
                .await
                .map_err(|_| DeviceError::Database)?;
        if result.rows_affected() == 0 {
            let revoked_at: Option<Option<i64>> =
                sqlx::query_scalar("SELECT revoked_at FROM devices WHERE id=?1")
                    .bind(id.to_string())
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(|_| DeviceError::Database)?;
            match revoked_at {
                None => return Err(DeviceError::NotFound),
                Some(Some(_)) if enabled => return Err(DeviceError::Revoked),
                Some(Some(_)) => {}
                Some(None) => return Err(DeviceError::Database),
            }
        }
        transaction
            .commit()
            .await
            .map_err(|_| DeviceError::Database)?;
        Ok(DeviceLifecycleMutation { id, completed_at })
    }

    pub async fn authenticate(
        &self,
        token: &str,
    ) -> Result<AuthenticatedDevice, AuthenticationError> {
        let parsed = ParsedToken::parse(token).map_err(|_| AuthenticationError::Unauthorized)?;
        let row = sqlx::query(
            "SELECT token_hash, enabled, revoked_at, credential_version \
             FROM devices WHERE id = ?1",
        )
        .bind(parsed.device_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AuthenticationError::Database)?;
        let Some(row) = row else {
            #[cfg(test)]
            self.wait_for_rejected_authentication().await;
            return Err(AuthenticationError::Unauthorized);
        };

        let credential_version: i64 = row
            .try_get("credential_version")
            .map_err(|_| AuthenticationError::Database)?;
        if credential_version != 2 {
            #[cfg(test)]
            self.wait_for_rejected_authentication().await;
            return Err(AuthenticationError::Unauthorized);
        }
        let stored_hash = row
            .try_get::<Vec<u8>, _>("token_hash")
            .map_err(|_| AuthenticationError::Database)?;
        let candidate_hash = hash_secret(&parsed.secret);
        if !constant_time_hash_matches(&stored_hash, &candidate_hash) {
            #[cfg(test)]
            self.wait_for_rejected_authentication().await;
            return Err(AuthenticationError::Unauthorized);
        }

        let enabled = row
            .try_get::<i64, _>("enabled")
            .map_err(|_| AuthenticationError::Database)?
            == 1;
        let revoked_at = row
            .try_get::<Option<i64>, _>("revoked_at")
            .map_err(|_| AuthenticationError::Database)?;
        if !enabled || revoked_at.is_some() {
            #[cfg(test)]
            self.wait_for_rejected_authentication().await;
            return Err(AuthenticationError::Revoked);
        }

        let now = unix_timestamp_ms().map_err(|_| AuthenticationError::Database)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AuthenticationError::Database)?;
        let fence = sqlx::query(
            "UPDATE devices \
             SET last_seen_at = CASE \
                 WHEN last_seen_at IS NULL OR last_seen_at <= ?1 THEN ?2 \
                 ELSE last_seen_at END \
             WHERE id = ?3 AND token_hash = ?4 AND credential_version = 2 \
               AND enabled = 1 AND revoked_at IS NULL",
        )
        .bind(now - LAST_SEEN_INTERVAL_MS)
        .bind(now)
        .bind(parsed.device_id.to_string())
        .bind(&stored_hash)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AuthenticationError::Database)?;
        if fence.rows_affected() != 1 {
            let current = sqlx::query(
                "SELECT token_hash, enabled, revoked_at, credential_version \
                 FROM devices WHERE id=?1",
            )
            .bind(parsed.device_id.to_string())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| AuthenticationError::Database)?;
            let Some(current) = current else {
                return Err(AuthenticationError::Unauthorized);
            };
            let current_hash = current
                .try_get::<Vec<u8>, _>("token_hash")
                .map_err(|_| AuthenticationError::Database)?;
            let current_version = current
                .try_get::<i64, _>("credential_version")
                .map_err(|_| AuthenticationError::Database)?;
            let current_enabled = current
                .try_get::<i64, _>("enabled")
                .map_err(|_| AuthenticationError::Database)?
                == 1;
            let current_revoked = current
                .try_get::<Option<i64>, _>("revoked_at")
                .map_err(|_| AuthenticationError::Database)?;
            if current_version == 2
                && constant_time_hash_matches(&current_hash, &candidate_hash)
                && (!current_enabled || current_revoked.is_some())
            {
                return Err(AuthenticationError::Revoked);
            }
            return Err(AuthenticationError::Unauthorized);
        }

        #[cfg(test)]
        if let Some(barrier) = &self.authentication_barrier {
            barrier.entered.wait().await;
            barrier.release.wait().await;
        }

        let authorization = sqlx::query(
            "SELECT name, token_hash, enabled, revoked_at, credential_version
             FROM devices WHERE id=?1",
        )
        .bind(parsed.device_id.to_string())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| AuthenticationError::Database)?;
        let name = authorization
            .try_get::<String, _>("name")
            .map_err(|_| AuthenticationError::Database)?;
        let snapshot_hash = authorization
            .try_get::<Vec<u8>, _>("token_hash")
            .map_err(|_| AuthenticationError::Database)?;
        let snapshot_enabled = authorization
            .try_get::<i64, _>("enabled")
            .map_err(|_| AuthenticationError::Database)?
            == 1;
        let snapshot_revoked_at = authorization
            .try_get::<Option<i64>, _>("revoked_at")
            .map_err(|_| AuthenticationError::Database)?;
        let snapshot_version = authorization
            .try_get::<i64, _>("credential_version")
            .map_err(|_| AuthenticationError::Database)?;

        let scope_rows = sqlx::query_scalar::<_, String>(
            "SELECT scope FROM device_scopes WHERE device_id=?1 ORDER BY scope",
        )
        .bind(parsed.device_id.to_string())
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| AuthenticationError::Database)?;
        let scopes = scope_rows
            .into_iter()
            .map(|value| DeviceScope::from_str(&value).map_err(|_| AuthenticationError::Database))
            .collect::<Result<_, _>>()?;
        let credential_revision = gateway_authorization_revision(
            parsed.device_id,
            &snapshot_hash,
            snapshot_version,
            snapshot_enabled,
            snapshot_revoked_at,
            &scopes,
        );
        let device = AuthenticatedDevice {
            id: parsed.device_id,
            name,
            scopes,
            credential_revision,
        };
        transaction
            .commit()
            .await
            .map_err(|_| AuthenticationError::Database)?;
        Ok(device)
    }

    pub async fn gateway_authorization_lease(
        &self,
        device_id: Uuid,
    ) -> Result<Option<GatewayAuthorizationLease>, AuthenticationError> {
        Ok(match self.gateway_authorization_state(device_id).await? {
            GatewayAuthorizationState::Active(lease) => Some(lease),
            GatewayAuthorizationState::DeviceRevoked
            | GatewayAuthorizationState::DeviceDisabled
            | GatewayAuthorizationState::Missing => None,
        })
    }

    pub async fn gateway_authorization_state(
        &self,
        device_id: Uuid,
    ) -> Result<GatewayAuthorizationState, AuthenticationError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AuthenticationError::Database)?;
        let row = sqlx::query(
            "SELECT token_hash, enabled, revoked_at, credential_version
             FROM devices WHERE id=?1",
        )
        .bind(device_id.to_string())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| AuthenticationError::Database)?;
        let Some(row) = row else {
            transaction
                .commit()
                .await
                .map_err(|_| AuthenticationError::Database)?;
            return Ok(GatewayAuthorizationState::Missing);
        };
        let token_hash = row
            .try_get::<Vec<u8>, _>("token_hash")
            .map_err(|_| AuthenticationError::Database)?;
        let enabled = row
            .try_get::<i64, _>("enabled")
            .map_err(|_| AuthenticationError::Database)?
            == 1;
        let revoked_at = row
            .try_get::<Option<i64>, _>("revoked_at")
            .map_err(|_| AuthenticationError::Database)?;
        let credential_version = row
            .try_get::<i64, _>("credential_version")
            .map_err(|_| AuthenticationError::Database)?;
        let scope_rows = sqlx::query_scalar::<_, String>(
            "SELECT scope FROM device_scopes WHERE device_id=?1 ORDER BY scope",
        )
        .bind(device_id.to_string())
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| AuthenticationError::Database)?;
        let scopes = scope_rows
            .into_iter()
            .map(|value| DeviceScope::from_str(&value).map_err(|_| AuthenticationError::Database))
            .collect::<Result<BTreeSet<_>, _>>()?;
        transaction
            .commit()
            .await
            .map_err(|_| AuthenticationError::Database)?;
        if revoked_at.is_some() || credential_version != 2 {
            return Ok(GatewayAuthorizationState::DeviceRevoked);
        }
        if !enabled {
            return Ok(GatewayAuthorizationState::DeviceDisabled);
        }
        Ok(GatewayAuthorizationState::Active(
            GatewayAuthorizationLease {
                device_id,
                credential_revision: gateway_authorization_revision(
                    device_id,
                    &token_hash,
                    credential_version,
                    enabled,
                    revoked_at,
                    &scopes,
                ),
                scopes,
            },
        ))
    }

    #[cfg(test)]
    fn with_authentication_barrier(
        mut self,
        authentication_barrier: AuthenticationBarrier,
    ) -> Self {
        self.authentication_barrier = Some(authentication_barrier);
        self
    }

    #[cfg(test)]
    fn with_rejected_authentication_barrier(
        mut self,
        rejected_authentication_barrier: AuthenticationBarrier,
    ) -> Self {
        self.rejected_authentication_barrier = Some(rejected_authentication_barrier);
        self
    }

    #[cfg(test)]
    async fn wait_for_rejected_authentication(&self) {
        if let Some(barrier) = &self.rejected_authentication_barrier {
            barrier.entered.wait().await;
            barrier.release.wait().await;
        }
    }

    pub async fn is_device_enabled(&self, id: Uuid) -> Result<bool, AuthenticationError> {
        let row = sqlx::query("SELECT enabled, revoked_at FROM devices WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| AuthenticationError::Database)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let enabled = row
            .try_get::<i64, _>("enabled")
            .map_err(|_| AuthenticationError::Database)?
            == 1;
        let revoked_at = row
            .try_get::<Option<i64>, _>("revoked_at")
            .map_err(|_| AuthenticationError::Database)?;
        Ok(enabled && revoked_at.is_none())
    }
}

#[cfg(test)]
#[derive(Clone)]
struct AuthenticationBarrier {
    entered: std::sync::Arc<tokio::sync::Barrier>,
    release: std::sync::Arc<tokio::sync::Barrier>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedDevice {
    pub id: Uuid,
    pub name: String,
    pub scopes: BTreeSet<DeviceScope>,
    pub(crate) credential_revision: String,
}

impl AuthenticatedDevice {
    pub fn has_scope(&self, scope: DeviceScope) -> bool {
        self.scopes.contains(&scope)
    }

    /// Opaque authorization revision used to bind process-local grants to the
    /// exact credential and closed scope snapshot authenticated for a request.
    pub(crate) fn authorization_revision(&self) -> &str {
        &self.credential_revision
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayAuthorizationLease {
    pub device_id: Uuid,
    pub scopes: BTreeSet<DeviceScope>,
    pub credential_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GatewayAuthorizationState {
    Active(GatewayAuthorizationLease),
    DeviceRevoked,
    DeviceDisabled,
    Missing,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum DeviceScope {
    #[serde(rename = "notify:write")]
    NotifyWrite,
    #[serde(rename = "notify:read_own")]
    NotifyReadOwn,
    #[serde(rename = "channel:read")]
    ChannelRead,
    #[serde(rename = "channel:manage")]
    ChannelManage,
    #[serde(rename = "gateway:connect")]
    GatewayConnect,
    #[serde(rename = "job:query")]
    RunQuery,
    #[serde(rename = "job:control")]
    RunControl,
}
impl DeviceScope {
    pub const ALL: [Self; 7] = [
        Self::NotifyWrite,
        Self::NotifyReadOwn,
        Self::ChannelRead,
        Self::ChannelManage,
        Self::GatewayConnect,
        Self::RunQuery,
        Self::RunControl,
    ];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotifyWrite => "notify:write",
            Self::NotifyReadOwn => "notify:read_own",
            Self::ChannelRead => "channel:read",
            Self::ChannelManage => "channel:manage",
            Self::GatewayConnect => "gateway:connect",
            Self::RunQuery => "job:query",
            Self::RunControl => "job:control",
        }
    }
}
impl fmt::Display for DeviceScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for DeviceScope {
    type Err = DeviceError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "notify:write" => Self::NotifyWrite,
            "notify:read_own" => Self::NotifyReadOwn,
            "channel:read" => Self::ChannelRead,
            "channel:manage" => Self::ChannelManage,
            "gateway:connect" => Self::GatewayConnect,
            "job:query" => Self::RunQuery,
            "job:control" => Self::RunControl,
            _ => return Err(DeviceError::InvalidScopes),
        })
    }
}

pub struct CreatedDevice {
    pub id: Uuid,
    pub name: String,
    pub created_at: i64,
    pub scopes: BTreeSet<DeviceScope>,
    pub token: PlaintextDeviceToken,
}

pub struct RotatedDevice {
    pub id: Uuid,
    pub rotated_at: i64,
    pub scopes: BTreeSet<DeviceScope>,
    pub token: PlaintextDeviceToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceLifecycleMutation {
    pub id: Uuid,
    pub completed_at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub credential_version: i64,
    pub rotated_at: Option<i64>,
    pub scopes: BTreeSet<DeviceScope>,
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct PlaintextDeviceToken(String);

impl PlaintextDeviceToken {
    fn new(device_id: Uuid, secret: &[u8; SECRET_BYTES]) -> Self {
        let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(secret));
        Self(format!("{TOKEN_VERSION}.{device_id}.{}", encoded.as_str()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PlaintextDeviceToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlaintextDeviceToken([REDACTED])")
    }
}

struct ParsedToken {
    device_id: Uuid,
    secret: Zeroizing<[u8; SECRET_BYTES]>,
}

impl ParsedToken {
    fn parse(token: &str) -> Result<Self, TokenParseError> {
        let mut segments = token.split('.');
        let version = segments.next().ok_or(TokenParseError)?;
        let device = segments.next().ok_or(TokenParseError)?;
        let secret = segments.next().ok_or(TokenParseError)?;
        if segments.next().is_some()
            || version != TOKEN_VERSION
            || secret.len() != SECRET_TEXT_LENGTH
            || secret.contains('=')
        {
            return Err(TokenParseError);
        }

        let device_id = parse_device_id(device)?;
        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(secret)
                .map_err(|_| TokenParseError)?,
        );
        let canonical = Zeroizing::new(URL_SAFE_NO_PAD.encode(decoded.as_slice()));
        if canonical.as_str() != secret {
            return Err(TokenParseError);
        }
        let secret = Zeroizing::new(
            decoded
                .as_slice()
                .try_into()
                .map_err(|_: std::array::TryFromSliceError| TokenParseError)?,
        );
        Ok(Self { device_id, secret })
    }
}

fn parse_device_id(value: &str) -> Result<Uuid, TokenParseError> {
    let id = Uuid::parse_str(value).map_err(|_| TokenParseError)?;
    if id.to_string() != value {
        return Err(TokenParseError);
    }
    Ok(id)
}

fn validate_device_name(name: &str) -> Result<String, DeviceError> {
    let trimmed = name.trim();
    if !(1..=80).contains(&trimmed.chars().count()) {
        return Err(DeviceError::InvalidName);
    }
    Ok(trimmed.to_owned())
}

pub(crate) fn sanitize_device_display_name(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    let mut pending_space = false;
    for character in name.trim().chars() {
        if character.is_control()
            || character.is_whitespace()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            pending_space = !sanitized.is_empty();
        } else {
            if pending_space {
                sanitized.push(' ');
                pending_space = false;
            }
            sanitized.push(character);
        }
    }
    if sanitized.is_empty() {
        "未命名设备".to_owned()
    } else {
        sanitized
    }
}

fn validate_scopes(scopes: &[DeviceScope]) -> Result<BTreeSet<DeviceScope>, DeviceError> {
    let scopes = scopes.iter().copied().collect::<BTreeSet<_>>();
    if scopes.is_empty() {
        Err(DeviceError::InvalidScopes)
    } else {
        Ok(scopes)
    }
}

async fn insert_scopes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: Uuid,
    scopes: &BTreeSet<DeviceScope>,
) -> Result<(), DeviceError> {
    for scope in scopes {
        sqlx::query("INSERT INTO device_scopes(device_id,scope) VALUES(?1,?2)")
            .bind(id.to_string())
            .bind(scope.as_str())
            .execute(&mut **transaction)
            .await
            .map_err(|_| DeviceError::Database)?;
    }
    Ok(())
}

async fn load_scopes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: Uuid,
) -> Result<BTreeSet<DeviceScope>, DeviceError> {
    let scope_rows = sqlx::query_scalar::<_, String>(
        "SELECT scope FROM device_scopes WHERE device_id=?1 ORDER BY scope",
    )
    .bind(id.to_string())
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| DeviceError::Database)?;
    let scopes = scope_rows
        .into_iter()
        .map(|scope| DeviceScope::from_str(&scope).map_err(|_| DeviceError::Database))
        .collect::<Result<Vec<_>, _>>()?;
    validate_scopes(&scopes)
}

struct IssuedDeviceCredential {
    token_hash: [u8; 32],
    token: PlaintextDeviceToken,
}

fn issue_device_credential(device_id: Uuid) -> Result<IssuedDeviceCredential, DeviceError> {
    let mut secret = Zeroizing::new([0_u8; SECRET_BYTES]);
    rand::rngs::OsRng
        .try_fill_bytes(&mut *secret)
        .map_err(|_| DeviceError::Random)?;
    Ok(IssuedDeviceCredential {
        token_hash: hash_secret(&secret),
        token: PlaintextDeviceToken::new(device_id, &secret),
    })
}

fn hash_secret(secret: &[u8; SECRET_BYTES]) -> [u8; 32] {
    blake3::derive_key(TOKEN_HASH_CONTEXT, secret)
}

fn gateway_authorization_revision(
    device_id: Uuid,
    token_hash: &[u8],
    credential_version: i64,
    enabled: bool,
    revoked_at: Option<i64>,
    scopes: &BTreeSet<DeviceScope>,
) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(GATEWAY_AUTHORIZATION_REVISION_CONTEXT);
    hasher.update(device_id.as_bytes());
    hasher.update(&(token_hash.len() as u64).to_be_bytes());
    hasher.update(token_hash);
    hasher.update(&credential_version.to_be_bytes());
    hasher.update(&[u8::from(enabled)]);
    match revoked_at {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&(scopes.len() as u64).to_be_bytes());
    for scope in scopes {
        let value = scope.as_str().as_bytes();
        hasher.update(&(value.len() as u64).to_be_bytes());
        hasher.update(value);
    }
    hasher.finalize().to_hex().to_string()
}

fn constant_time_hash_matches(stored: &[u8], candidate: &[u8; 32]) -> bool {
    stored.len() == candidate.len() && stored.ct_eq(candidate).into()
}

fn unix_timestamp_ms() -> Result<i64, DeviceError> {
    let millis = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_err(|_| DeviceError::Clock)?
        .as_millis();
    i64::try_from(millis).map_err(|_| DeviceError::Clock)
}

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("device name must contain between 1 and 80 characters")]
    InvalidName,
    #[error("device ID is invalid")]
    InvalidId,
    #[error("at least one valid device scope is required")]
    InvalidScopes,
    #[error("device was not found")]
    NotFound,
    #[error("revoked devices cannot be changed")]
    Revoked,
    #[error("secure random generation failed")]
    Random,
    #[error("system clock is invalid")]
    Clock,
    #[error("device database operation failed")]
    Database,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthenticationError {
    #[error("authentication required")]
    Unauthorized,
    #[error("device credential is revoked")]
    Revoked,
    #[error("authentication service is unavailable")]
    Database,
}

#[derive(Debug)]
struct TokenParseError;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use tempfile::TempDir;

    use super::*;
    use crate::{config::DatabaseConfig, db};

    async fn service() -> (TempDir, DeviceAuthService) {
        let directory = TempDir::new().expect("temp directory");
        let config = DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        };
        let pool = db::open(&config).await.expect("database");
        (directory, DeviceAuthService::new(pool))
    }

    #[tokio::test]
    async fn created_token_is_strict_and_plaintext_never_reaches_sqlite() {
        let (directory, service) = service().await;
        let created = service
            .create_device("  OFFICE-PC  ", &DeviceScope::ALL)
            .await
            .expect("device");
        assert_eq!(created.name, "OFFICE-PC");
        assert_eq!(
            format!("{:?}", created.token),
            "PlaintextDeviceToken([REDACTED])"
        );

        let parsed = ParsedToken::parse(created.token.expose()).expect("token parse");
        assert_eq!(parsed.device_id, created.id);
        assert_eq!(parsed.secret.len(), 32);

        let row = sqlx::query("SELECT token_hash FROM devices WHERE id = ?1")
            .bind(created.id.to_string())
            .fetch_one(&service.pool)
            .await
            .expect("token hash");
        let stored: Vec<u8> = row.get("token_hash");
        assert_eq!(stored.len(), 32);
        assert!(constant_time_hash_matches(
            &stored,
            &hash_secret(&parsed.secret)
        ));

        let plaintext = created.token.expose().as_bytes().to_vec();
        let secret_text = created
            .token
            .expose()
            .rsplit_once('.')
            .expect("secret segment")
            .1
            .as_bytes()
            .to_vec();
        service.pool.close().await;
        for suffix in ["relay.db", "relay.db-wal", "relay.db-shm"] {
            let path = directory.path().join(suffix);
            if path.exists() {
                let bytes = std::fs::read(path).expect("database artifact");
                assert!(!contains_bytes(&bytes, &plaintext), "full token leaked");
                assert!(!contains_bytes(&bytes, &secret_text), "secret leaked");
            }
        }
    }

    #[test]
    fn token_parser_rejects_non_canonical_or_wrong_length_values() {
        let id = Uuid::new_v4();
        let valid_secret = URL_SAFE_NO_PAD.encode([1_u8; 32]);
        let mut non_canonical_secret = URL_SAFE_NO_PAD.encode([0_u8; 32]);
        non_canonical_secret.pop();
        non_canonical_secret.push('B');
        let invalid = [
            String::new(),
            format!("pdv0.{id}.{valid_secret}"),
            format!("pdv2.{}.{}", id.to_string().to_uppercase(), valid_secret),
            format!("pdv2.{id}.{}", URL_SAFE_NO_PAD.encode([1_u8; 31])),
            format!("pdv2.{id}.{}", URL_SAFE_NO_PAD.encode([1_u8; 33])),
            format!("pdv2.{id}.{valid_secret}="),
            format!("pdv2.{id}.{non_canonical_secret}"),
            format!("pdv2.{id}.{valid_secret}.extra"),
            format!(" pdv2.{id}.{valid_secret}"),
        ];
        for token in invalid {
            assert!(ParsedToken::parse(&token).is_err(), "accepted {token}");
        }
    }

    #[test]
    fn hash_comparison_checks_every_position() {
        let candidate = hash_secret(&[9_u8; 32]);
        assert!(constant_time_hash_matches(&candidate, &candidate));
        assert!(!constant_time_hash_matches(&candidate[..31], &candidate));
        for index in [0, 15, 31] {
            let mut different = candidate;
            different[index] ^= 1;
            assert!(!constant_time_hash_matches(&different, &candidate));
        }
    }

    #[tokio::test]
    async fn valid_bad_and_revoked_tokens_have_safe_distinct_results() {
        let (_directory, service) = service().await;
        let created = service
            .create_device("DEVICE", &DeviceScope::ALL)
            .await
            .expect("device");
        let identity = service
            .authenticate(created.token.expose())
            .await
            .expect("valid auth");
        assert_eq!(identity.id, created.id);

        let bad = format!("pdv2.{}.{}", created.id, URL_SAFE_NO_PAD.encode([3_u8; 32]));
        assert_eq!(
            service.authenticate(&bad).await,
            Err(AuthenticationError::Unauthorized)
        );
        service
            .revoke_device(&created.id.to_string())
            .await
            .expect("revoke");
        service
            .revoke_device(&created.id.to_string())
            .await
            .expect("idempotent revoke");
        assert_eq!(
            service.authenticate(created.token.expose()).await,
            Err(AuthenticationError::Revoked)
        );
        assert_eq!(
            service.authenticate(&bad).await,
            Err(AuthenticationError::Unauthorized)
        );
        service.pool.close().await;
    }

    #[tokio::test]
    async fn successful_authentication_throttles_last_seen_writes() {
        let (_directory, service) = service().await;
        let created = service
            .create_device("DEVICE", &DeviceScope::ALL)
            .await
            .expect("device");
        let fixed_last_seen = unix_timestamp_ms().expect("time");
        sqlx::query("UPDATE devices SET last_seen_at = ?1 WHERE id = ?2")
            .bind(fixed_last_seen)
            .bind(created.id.to_string())
            .execute(&service.pool)
            .await
            .expect("seed last seen");

        service
            .authenticate(created.token.expose())
            .await
            .expect("authentication");
        let unchanged: i64 = sqlx::query_scalar("SELECT last_seen_at FROM devices WHERE id = ?1")
            .bind(created.id.to_string())
            .fetch_one(&service.pool)
            .await
            .expect("last seen");
        assert_eq!(unchanged, fixed_last_seen);

        sqlx::query("UPDATE devices SET last_seen_at = 0 WHERE id = ?1")
            .bind(created.id.to_string())
            .execute(&service.pool)
            .await
            .expect("age last seen");
        service
            .authenticate(created.token.expose())
            .await
            .expect("authentication after interval");
        let refreshed: i64 = sqlx::query_scalar("SELECT last_seen_at FROM devices WHERE id = ?1")
            .bind(created.id.to_string())
            .fetch_one(&service.pool)
            .await
            .expect("refreshed last seen");
        assert!(refreshed > 0);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn concurrent_device_creation_produces_unique_credentials() {
        let (_directory, service) = service().await;
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..24 {
            let service = service.clone();
            tasks.spawn(async move {
                service
                    .create_device(&format!("DEVICE-{index}"), &DeviceScope::ALL)
                    .await
            });
        }
        let mut ids = HashSet::new();
        let mut tokens = HashSet::new();
        while let Some(result) = tasks.join_next().await {
            let created = result.expect("create task").expect("created device");
            assert!(ids.insert(created.id));
            assert!(tokens.insert(created.token.expose().to_owned()));
        }
        assert_eq!(service.list_devices().await.expect("device list").len(), 24);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn scopes_are_required_and_rotation_is_atomic_and_never_revives_revoked_devices() {
        let (_directory, service) = service().await;
        assert!(matches!(
            service.create_device("EMPTY", &[]).await,
            Err(DeviceError::InvalidScopes)
        ));
        let created = service
            .create_device("ROTATE", &[DeviceScope::NotifyWrite])
            .await
            .expect("device");
        assert!(created.token.expose().starts_with("pdv2."));
        let old_token = created.token.expose().to_owned();
        let rotated = service
            .rotate_device(
                &created.id.to_string(),
                &[DeviceScope::ChannelRead, DeviceScope::ChannelRead],
            )
            .await
            .expect("rotate");
        assert_eq!(rotated.scopes, BTreeSet::from([DeviceScope::ChannelRead]));
        assert_eq!(
            service.authenticate(&old_token).await,
            Err(AuthenticationError::Unauthorized)
        );
        assert_eq!(
            service
                .authenticate(rotated.token.expose())
                .await
                .expect("new token")
                .scopes,
            rotated.scopes
        );
        service
            .revoke_device(&created.id.to_string())
            .await
            .expect("revoke");
        assert!(matches!(
            service
                .rotate_device(&created.id.to_string(), &[DeviceScope::NotifyWrite])
                .await,
            Err(DeviceError::Revoked)
        ));
    }

    #[tokio::test]
    async fn rotation_cannot_cross_the_authentication_credential_and_scope_snapshot() {
        let (_directory, service) = service().await;
        let created = service
            .create_device("ROTATION-RACE", &[DeviceScope::NotifyWrite])
            .await
            .expect("device");
        let old_token = created.token.expose().to_owned();
        let entered = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let release = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let fenced_service = service
            .clone()
            .with_authentication_barrier(AuthenticationBarrier {
                entered: entered.clone(),
                release: release.clone(),
            });

        let authenticating = tokio::spawn({
            let old_token = old_token.clone();
            async move { fenced_service.authenticate(&old_token).await }
        });
        entered.wait().await;
        let (rotation_started_tx, rotation_started_rx) = tokio::sync::oneshot::channel();
        let mut rotating = tokio::spawn({
            let service = service.clone();
            let id = created.id.to_string();
            async move {
                let _ = rotation_started_tx.send(());
                service
                    .rotate_device(&id, &[DeviceScope::ChannelRead])
                    .await
            }
        });
        rotation_started_rx.await.expect("rotation task started");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut rotating)
                .await
                .is_err(),
            "rotation crossed the authentication writer fence"
        );

        release.wait().await;
        let identity = authenticating
            .await
            .expect("authentication task")
            .expect("authentication linearized before rotation");
        assert_eq!(
            identity.scopes,
            BTreeSet::from([DeviceScope::NotifyWrite]),
            "the old token must never observe post-rotation scopes"
        );
        let rotated = rotating.await.expect("rotation task").expect("rotation");
        assert_eq!(
            service.authenticate(&old_token).await,
            Err(AuthenticationError::Unauthorized)
        );
        assert_eq!(
            service
                .authenticate(rotated.token.expose())
                .await
                .expect("new credential")
                .scopes,
            BTreeSet::from([DeviceScope::ChannelRead])
        );
    }

    #[tokio::test]
    async fn revocation_cannot_commit_inside_an_authentication_snapshot() {
        let (_directory, service) = service().await;
        let created = service
            .create_device("REVOCATION-RACE", &[DeviceScope::NotifyReadOwn])
            .await
            .expect("device");
        let token = created.token.expose().to_owned();
        let entered = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let release = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let fenced_service = service
            .clone()
            .with_authentication_barrier(AuthenticationBarrier {
                entered: entered.clone(),
                release: release.clone(),
            });

        let authenticating = tokio::spawn({
            let token = token.clone();
            async move { fenced_service.authenticate(&token).await }
        });
        entered.wait().await;
        let (revocation_started_tx, revocation_started_rx) = tokio::sync::oneshot::channel();
        let mut revoking = tokio::spawn({
            let service = service.clone();
            let id = created.id.to_string();
            async move {
                let _ = revocation_started_tx.send(());
                service.revoke_device(&id).await
            }
        });
        revocation_started_rx
            .await
            .expect("revocation task started");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut revoking)
                .await
                .is_err(),
            "revocation crossed the authentication writer fence"
        );

        release.wait().await;
        authenticating
            .await
            .expect("authentication task")
            .expect("authentication linearized before revocation");
        revoking
            .await
            .expect("revocation task")
            .expect("revocation");
        assert_eq!(
            service.authenticate(&token).await,
            Err(AuthenticationError::Revoked),
            "no authentication may succeed after revocation commits"
        );
    }

    #[tokio::test]
    async fn unknown_and_wrong_secrets_never_hold_the_sqlite_writer_fence() {
        let (_directory, service) = service().await;
        let created = service
            .create_device("REJECTED-AUTH", &[DeviceScope::NotifyWrite])
            .await
            .expect("device");
        let wrong_token = format!(
            "pdv2.{}.{}",
            created.id,
            URL_SAFE_NO_PAD.encode([31_u8; SECRET_BYTES])
        );
        let entered = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let release = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let rejected_service =
            service
                .clone()
                .with_rejected_authentication_barrier(AuthenticationBarrier {
                    entered: entered.clone(),
                    release: release.clone(),
                });
        let rejected =
            tokio::spawn(async move { rejected_service.authenticate(&wrong_token).await });
        entered.wait().await;
        let rotated = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.rotate_device(&created.id.to_string(), &[DeviceScope::ChannelRead]),
        )
        .await;
        release.wait().await;
        assert!(
            rotated.is_ok(),
            "wrong-secret authentication blocked a legitimate rotation"
        );
        rotated.expect("rotation timeout").expect("rotation");
        assert_eq!(
            rejected.await.expect("rejected auth task"),
            Err(AuthenticationError::Unauthorized)
        );

        let unknown_id = Uuid::new_v4();
        let unknown_token = format!(
            "pdv2.{unknown_id}.{}",
            URL_SAFE_NO_PAD.encode([47_u8; SECRET_BYTES])
        );
        let entered = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let release = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let rejected_service =
            service
                .clone()
                .with_rejected_authentication_barrier(AuthenticationBarrier {
                    entered: entered.clone(),
                    release: release.clone(),
                });
        let rejected =
            tokio::spawn(async move { rejected_service.authenticate(&unknown_token).await });
        entered.wait().await;
        let created_while_rejected = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.create_device("WRITER-PROBE", &[DeviceScope::RunQuery]),
        )
        .await;
        release.wait().await;
        assert!(
            created_while_rejected.is_ok(),
            "unknown-device authentication blocked an unrelated database write"
        );
        created_while_rejected
            .expect("create timeout")
            .expect("create device");
        assert_eq!(
            rejected.await.expect("unknown auth task"),
            Err(AuthenticationError::Unauthorized)
        );
    }

    #[tokio::test]
    async fn legacy_pdv1_and_version_one_rows_are_unconditionally_unauthorized() {
        let (_directory, service) = service().await;
        let id = Uuid::new_v4();
        let secret = [9_u8; SECRET_BYTES];
        let legacy_hash = blake3::derive_key("promptdock-relay/device-token/v1", &secret);
        sqlx::query(
            "INSERT INTO devices(id,name,token_hash,enabled,created_at) VALUES(?1,'LEGACY',?2,1,1)",
        )
        .bind(id.to_string())
        .bind(legacy_hash.to_vec())
        .execute(&service.pool)
        .await
        .expect("legacy row");
        let token = format!("pdv1.{id}.{}", URL_SAFE_NO_PAD.encode(secret));
        assert_eq!(
            service.authenticate(&token).await,
            Err(AuthenticationError::Unauthorized)
        );
        assert!(
            service.list_devices().await.expect("list")[0]
                .scopes
                .is_empty()
        );
    }

    #[tokio::test]
    async fn name_and_identifier_validation_fail_before_mutation() {
        let (_directory, service) = service().await;
        let too_long = "x".repeat(81);
        for name in ["", "   ", too_long.as_str()] {
            assert!(matches!(
                service.create_device(name, &DeviceScope::ALL).await,
                Err(DeviceError::InvalidName)
            ));
        }
        assert!(matches!(
            service.create_device("VALID", &[]).await,
            Err(DeviceError::InvalidScopes)
        ));
        assert!(matches!(
            service.revoke_device("not-a-uuid").await,
            Err(DeviceError::InvalidId)
        ));
        assert!(matches!(
            service.revoke_device(&Uuid::new_v4().to_string()).await,
            Err(DeviceError::NotFound)
        ));
        assert!(service.list_devices().await.expect("list").is_empty());
        service.pool.close().await;
    }

    #[tokio::test]
    async fn gateway_authorization_revision_fences_rotation_scope_and_device_lifecycle() {
        let (_directory, service) = service().await;
        let created = service
            .create_device(
                "GATEWAY",
                &[DeviceScope::GatewayConnect, DeviceScope::RunQuery],
            )
            .await
            .expect("gateway device");
        let authenticated = service
            .authenticate(created.token.expose())
            .await
            .expect("authenticated gateway device");
        let last_seen_before: Option<i64> =
            sqlx::query_scalar("SELECT last_seen_at FROM devices WHERE id=?1")
                .bind(created.id.to_string())
                .fetch_one(&service.pool)
                .await
                .expect("last seen before lease");
        let initial = service
            .gateway_authorization_lease(created.id)
            .await
            .expect("initial lease")
            .expect("active initial lease");
        assert_eq!(
            authenticated.credential_revision,
            initial.credential_revision
        );
        assert_eq!(authenticated.scopes, initial.scopes);
        let last_seen_after: Option<i64> =
            sqlx::query_scalar("SELECT last_seen_at FROM devices WHERE id=?1")
                .bind(created.id.to_string())
                .fetch_one(&service.pool)
                .await
                .expect("last seen after lease");
        assert_eq!(last_seen_before, last_seen_after);

        let first_rotation = service
            .rotate_device(
                &created.id.to_string(),
                &[DeviceScope::GatewayConnect, DeviceScope::RunQuery],
            )
            .await
            .expect("first rotation");
        sqlx::query("UPDATE devices SET rotated_at=123456789 WHERE id=?1")
            .bind(created.id.to_string())
            .execute(&service.pool)
            .await
            .expect("force first rotation timestamp");
        let first_revision = service
            .authenticate(first_rotation.token.expose())
            .await
            .expect("first rotated token")
            .credential_revision;
        let second_rotation = service
            .rotate_device(
                &created.id.to_string(),
                &[DeviceScope::GatewayConnect, DeviceScope::RunQuery],
            )
            .await
            .expect("second rotation");
        sqlx::query("UPDATE devices SET rotated_at=123456789 WHERE id=?1")
            .bind(created.id.to_string())
            .execute(&service.pool)
            .await
            .expect("force same second rotation timestamp");
        let second_revision = service
            .authenticate(second_rotation.token.expose())
            .await
            .expect("second rotated token")
            .credential_revision;
        assert_ne!(initial.credential_revision, first_revision);
        assert_ne!(first_revision, second_revision);

        sqlx::query("DELETE FROM device_scopes WHERE device_id=?1 AND scope='job:query'")
            .bind(created.id.to_string())
            .execute(&service.pool)
            .await
            .expect("remove query scope");
        let scope_changed = service
            .gateway_authorization_lease(created.id)
            .await
            .expect("scope changed lease")
            .expect("active scope changed lease");
        assert_ne!(scope_changed.credential_revision, second_revision);
        assert!(!scope_changed.scopes.contains(&DeviceScope::RunQuery));

        service
            .revoke_device(&created.id.to_string())
            .await
            .expect("revoke gateway device");
        assert_eq!(
            service
                .gateway_authorization_lease(created.id)
                .await
                .expect("revoked lease lookup"),
            None
        );

        let disabled = service
            .create_device("DISABLED", &[DeviceScope::GatewayConnect])
            .await
            .expect("disabled fixture");
        sqlx::query("UPDATE devices SET enabled=0 WHERE id=?1")
            .bind(disabled.id.to_string())
            .execute(&service.pool)
            .await
            .expect("disable device");
        assert_eq!(
            service
                .gateway_authorization_lease(disabled.id)
                .await
                .expect("disabled lease lookup"),
            None
        );
        sqlx::query("DELETE FROM devices WHERE id=?1")
            .bind(disabled.id.to_string())
            .execute(&service.pool)
            .await
            .expect("delete disabled device");
        assert_eq!(
            service
                .gateway_authorization_lease(disabled.id)
                .await
                .expect("deleted lease lookup"),
            None
        );
        service.pool.close().await;
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle)
    }
}
