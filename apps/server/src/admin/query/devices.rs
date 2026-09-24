//! Safe read projections for the Admin device surface.
//!
//! This module deliberately has no HTTP dependencies. The router owns request
//! parsing and opaque cursor translation; this service owns the constrained
//! database read and the join with the process-local gateway snapshot.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::{Row as _, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::gateway::{GatewayConnectionSnapshot, GatewayRegistry, protocol::GatewayCapabilityV5};

use super::super::cursor::CursorPosition;

pub const DEFAULT_DEVICE_PAGE_SIZE: usize = 50;
pub const MAX_DEVICE_SEARCH_CHARS: usize = 64;

#[derive(Clone)]
pub struct AdminDeviceQueryService {
    pool: SqlitePool,
    gateway: GatewayRegistry,
    max_page_size: usize,
}

impl AdminDeviceQueryService {
    pub fn new(pool: SqlitePool, gateway: GatewayRegistry, max_page_size: usize) -> Self {
        Self {
            pool,
            gateway,
            max_page_size,
        }
    }

    pub async fn page(
        &self,
        request: AdminDevicePageRequest,
    ) -> Result<AdminDevicePage, AdminDeviceQueryError> {
        let limit = request.limit.unwrap_or(DEFAULT_DEVICE_PAGE_SIZE);
        validate_page(limit, self.max_page_size)?;
        let search = validate_search(request.search)?;
        let search_pattern = search.as_deref().map(like_contains_pattern);
        validate_cursor_position(request.after.as_ref())?;
        let fetch_limit =
            i64::try_from((limit + 1).max(101)).map_err(|_| AdminDeviceQueryError::InvalidPage)?;
        let (mut cursor_timestamp, mut cursor_id) = request
            .after
            .map(|position| (Some(position.sort_timestamp), Some(position.tie_break)))
            .unwrap_or((None, None));
        let database_state = request.state.map(|state| match state {
            AdminDeviceState::Revoked => "revoked",
            AdminDeviceState::Disabled => "disabled",
            AdminDeviceState::Online | AdminDeviceState::Offline => "active",
        });

        let connections = self.gateway.connections().await;
        let gateways = connections
            .into_iter()
            .map(|connection| (connection.device_id, connection))
            .collect::<BTreeMap<_, _>>();
        let mut items = Vec::with_capacity(limit + 1);

        // This is intentionally a static, explicitly enumerated projection.
        // In particular, do not add devices.token_hash here: Admin read paths
        // must never load credential material, even transiently.
        loop {
            let rows = sqlx::query(
                "SELECT d.id, d.name, d.enabled, d.credential_version, d.created_at, \
                    d.last_seen_at, d.revoked_at, d.rotated_at, \
                    COALESCE(( \
                        SELECT GROUP_CONCAT(scope, ',') \
                        FROM ( \
                            SELECT scope FROM device_scopes \
                            WHERE device_id = d.id ORDER BY scope \
                        ) \
                    ), '') AS scopes \
             FROM devices d \
             WHERE (?1 IS NULL \
                    OR lower(d.name) LIKE ?2 ESCAPE '\\' \
                    OR d.id LIKE ?2 ESCAPE '\\') \
               AND (?3 IS NULL \
                    OR (?3 = 'revoked' AND d.revoked_at IS NOT NULL) \
                    OR (?3 = 'disabled' AND d.revoked_at IS NULL AND d.enabled = 0) \
                    OR (?3 = 'active' AND d.revoked_at IS NULL AND d.enabled = 1)) \
               AND (?4 IS NULL \
                    OR d.created_at < ?4 \
                    OR (d.created_at = ?4 AND d.id > ?5)) \
             ORDER BY d.created_at DESC, d.id ASC \
             LIMIT ?6",
            )
            .bind(search.as_deref())
            .bind(search_pattern.as_deref())
            .bind(database_state)
            .bind(cursor_timestamp)
            .bind(cursor_id.as_deref())
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| AdminDeviceQueryError::Database)?;
            let row_count = rows.len();
            if row_count == 0 {
                break;
            }
            for row in rows {
                let device = database_device(&row)?;
                cursor_timestamp = Some(device.created_at);
                cursor_id = Some(device.id.to_string());
                let gateway = gateways.get(&device.id).cloned();
                let summary = project_summary(device, gateway);
                if request.state.is_none_or(|state| summary.state == state) {
                    items.push(summary);
                    if items.len() > limit {
                        break;
                    }
                }
            }
            if items.len() > limit || row_count < usize::try_from(fetch_limit).unwrap_or(usize::MAX)
            {
                break;
            }
        }

        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_position = has_more.then(|| {
            let last = items.last().expect("a full page has a final item");
            CursorPosition {
                sort_timestamp: last.created_at,
                tie_break: last.id.clone(),
            }
        });

        Ok(AdminDevicePage {
            items,
            next_position,
        })
    }

    pub async fn detail(
        &self,
        device_id: &str,
    ) -> Result<Option<AdminDeviceDetail>, AdminDeviceQueryError> {
        let device_id =
            Uuid::parse_str(device_id).map_err(|_| AdminDeviceQueryError::InvalidDeviceId)?;
        let row = sqlx::query(
            "SELECT d.id, d.name, d.enabled, d.credential_version, d.created_at, \
                    d.last_seen_at, d.revoked_at, d.rotated_at, \
                    COALESCE(( \
                        SELECT GROUP_CONCAT(scope, ',') \
                        FROM ( \
                            SELECT scope FROM device_scopes \
                            WHERE device_id = d.id ORDER BY scope \
                        ) \
                    ), '') AS scopes \
             FROM devices d WHERE d.id = ?1",
        )
        .bind(device_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AdminDeviceQueryError::Database)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let device = database_device(&row)?;
        let gateway = self.gateway.connection(device.id).await;
        Ok(Some(project_detail(device, gateway)))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdminDevicePageRequest {
    pub after: Option<CursorPosition>,
    pub limit: Option<usize>,
    pub search: Option<String>,
    pub state: Option<AdminDeviceState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDevicePage {
    pub items: Vec<AdminDeviceSummary>,
    #[serde(skip)]
    pub(crate) next_position: Option<CursorPosition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDeviceSummary {
    pub id: String,
    pub name: String,
    pub state: AdminDeviceState,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seen_at: Option<i64>,
    pub token_format_version: i64,
    pub last_rotated_at: Option<i64>,
    pub gateway_state: AdminGatewayState,
    pub client_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDeviceDetail {
    pub id: String,
    pub full_device_uuid: String,
    pub name: String,
    pub state: AdminDeviceState,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seen_at: Option<i64>,
    pub token_format_version: i64,
    pub last_rotated_at: Option<i64>,
    pub gateway_state: AdminGatewayState,
    pub client_version: Option<String>,
    pub connected_at: Option<i64>,
    pub gateway_generation: Option<u64>,
    pub capabilities: Vec<String>,
    pub last_heartbeat_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminDeviceState {
    Revoked,
    Disabled,
    Online,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminGatewayState {
    Connected,
    Offline,
    NotEnabled,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AdminDeviceQueryError {
    #[error("device page request is invalid")]
    InvalidPage,
    #[error("device search is invalid")]
    InvalidSearch,
    #[error("device identifier is invalid")]
    InvalidDeviceId,
    #[error("device projection is invalid")]
    InvalidProjection,
    #[error("device query is unavailable")]
    Database,
}

struct DatabaseDevice {
    id: Uuid,
    name: String,
    enabled: bool,
    credential_version: i64,
    created_at: i64,
    last_seen_at: Option<i64>,
    revoked_at: Option<i64>,
    rotated_at: Option<i64>,
    scopes: Vec<String>,
}

fn database_device(row: &sqlx::sqlite::SqliteRow) -> Result<DatabaseDevice, AdminDeviceQueryError> {
    let id = row
        .try_get::<String, _>("id")
        .map_err(|_| AdminDeviceQueryError::Database)
        .and_then(|id| Uuid::parse_str(&id).map_err(|_| AdminDeviceQueryError::Database))?;
    let scopes = row
        .try_get::<String, _>("scopes")
        .map_err(|_| AdminDeviceQueryError::Database)?
        .split(',')
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect();
    let device = DatabaseDevice {
        id,
        name: row
            .try_get("name")
            .map_err(|_| AdminDeviceQueryError::Database)?,
        enabled: row
            .try_get::<i64, _>("enabled")
            .map_err(|_| AdminDeviceQueryError::Database)?
            == 1,
        credential_version: row
            .try_get("credential_version")
            .map_err(|_| AdminDeviceQueryError::Database)?,
        created_at: row
            .try_get("created_at")
            .map_err(|_| AdminDeviceQueryError::Database)?,
        last_seen_at: row
            .try_get("last_seen_at")
            .map_err(|_| AdminDeviceQueryError::Database)?,
        revoked_at: row
            .try_get("revoked_at")
            .map_err(|_| AdminDeviceQueryError::Database)?,
        rotated_at: row
            .try_get("rotated_at")
            .map_err(|_| AdminDeviceQueryError::Database)?,
        scopes,
    };
    if device.credential_version <= 0
        || device.created_at < 0
        || [device.last_seen_at, device.revoked_at, device.rotated_at]
            .into_iter()
            .flatten()
            .any(|value| value < 0)
    {
        return Err(AdminDeviceQueryError::InvalidProjection);
    }
    Ok(device)
}

fn project_summary(
    device: DatabaseDevice,
    gateway: Option<GatewayConnectionSnapshot>,
) -> AdminDeviceSummary {
    let state = device_state(&device, gateway.is_some());
    let updated_at = updated_at(&device);
    let gateway_state = gateway_state(&device, gateway.is_some());
    let client_version = gateway
        .as_ref()
        .map(|snapshot| snapshot.client_version.clone());
    AdminDeviceSummary {
        id: device.id.to_string(),
        name: device.name,
        state,
        scopes: device.scopes,
        created_at: device.created_at,
        updated_at,
        last_seen_at: device.last_seen_at,
        token_format_version: device.credential_version,
        last_rotated_at: device.rotated_at,
        gateway_state,
        client_version,
    }
}

fn project_detail(
    device: DatabaseDevice,
    gateway: Option<GatewayConnectionSnapshot>,
) -> AdminDeviceDetail {
    let active_gateway = device.enabled && device.revoked_at.is_none();
    let gateway_state = gateway_state(&device, gateway.is_some());
    let updated_at = updated_at(&device);
    let full_device_uuid = device.id.to_string();
    let (connected_at, gateway_generation, capabilities, last_heartbeat_at, client_version) =
        gateway
            .filter(|_| active_gateway)
            .map(|snapshot| {
                (
                    Some(snapshot.connected_at),
                    Some(snapshot.generation),
                    snapshot
                        .capabilities
                        .into_iter()
                        .map(GatewayCapabilityV5::wire_name)
                        .map(str::to_owned)
                        .collect(),
                    Some(snapshot.last_pong_at),
                    Some(snapshot.client_version),
                )
            })
            .unwrap_or((None, None, Vec::new(), None, None));
    let state = device_state(&device, active_gateway && connected_at.is_some());
    AdminDeviceDetail {
        id: device.id.to_string(),
        full_device_uuid,
        name: device.name,
        state,
        scopes: device.scopes,
        created_at: device.created_at,
        updated_at,
        last_seen_at: device.last_seen_at,
        token_format_version: device.credential_version,
        last_rotated_at: device.rotated_at,
        gateway_state,
        client_version,
        connected_at,
        gateway_generation,
        capabilities,
        last_heartbeat_at,
    }
}

fn device_state(device: &DatabaseDevice, gateway_connected: bool) -> AdminDeviceState {
    if device.revoked_at.is_some() {
        AdminDeviceState::Revoked
    } else if !device.enabled {
        AdminDeviceState::Disabled
    } else if gateway_connected {
        AdminDeviceState::Online
    } else {
        AdminDeviceState::Offline
    }
}

fn gateway_state(device: &DatabaseDevice, gateway_connected: bool) -> AdminGatewayState {
    if !device.enabled || device.revoked_at.is_some() {
        AdminGatewayState::NotEnabled
    } else if gateway_connected {
        AdminGatewayState::Connected
    } else {
        AdminGatewayState::Offline
    }
}

fn updated_at(device: &DatabaseDevice) -> i64 {
    [
        Some(device.created_at),
        device.rotated_at,
        device.revoked_at,
        device.last_seen_at,
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(device.created_at)
}

fn validate_page(limit: usize, max_page_size: usize) -> Result<(), AdminDeviceQueryError> {
    if limit == 0 || limit > max_page_size {
        return Err(AdminDeviceQueryError::InvalidPage);
    }
    Ok(())
}

fn validate_cursor_position(
    position: Option<&CursorPosition>,
) -> Result<(), AdminDeviceQueryError> {
    let Some(position) = position else {
        return Ok(());
    };
    if position.sort_timestamp < 0 || Uuid::parse_str(&position.tie_break).is_err() {
        return Err(AdminDeviceQueryError::InvalidPage);
    }
    Ok(())
}

fn validate_search(search: Option<String>) -> Result<Option<String>, AdminDeviceQueryError> {
    let Some(search) = search else {
        return Ok(None);
    };
    if search.chars().count() > MAX_DEVICE_SEARCH_CHARS {
        return Err(AdminDeviceQueryError::InvalidSearch);
    }
    let search = search.trim().to_owned();
    Ok((!search.is_empty()).then_some(search))
}

fn like_contains_pattern(search: &str) -> String {
    let mut escaped = String::with_capacity(search.len() + 2);
    escaped.push('%');
    for character in search.chars() {
        if matches!(character, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('%');
    escaped
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::{config::DatabaseConfig, db};

    async fn service() -> (
        TempDir,
        SqlitePool,
        GatewayRegistry,
        AdminDeviceQueryService,
    ) {
        let directory = TempDir::new().expect("temporary database directory");
        let pool = db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let gateway = GatewayRegistry::new();
        let service = AdminDeviceQueryService::new(pool.clone(), gateway.clone(), 100);
        (directory, pool, gateway, service)
    }

    async fn insert_device(
        pool: &SqlitePool,
        id: Uuid,
        name: &str,
        enabled: bool,
        created_at: i64,
        revoked_at: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO devices( \
                id, name, token_hash, enabled, credential_version, created_at, last_seen_at, revoked_at, rotated_at \
             ) VALUES (?1, ?2, ?3, ?4, 2, ?5, 25, ?6, 20)",
        )
        .bind(id.to_string())
        .bind(name)
        .bind(vec![7_u8; 32])
        .bind(i64::from(enabled))
        .bind(created_at)
        .bind(revoked_at)
        .execute(pool)
        .await
        .expect("device fixture");
        sqlx::query("INSERT INTO device_scopes(device_id, scope) VALUES (?1, 'gateway:connect')")
            .bind(id.to_string())
            .execute(pool)
            .await
            .expect("scope fixture");
    }

    #[tokio::test]
    async fn page_uses_a_safe_projection_and_handles_an_offline_gateway_snapshot() {
        let (_directory, pool, gateway, service) = service().await;
        let offline = Uuid::new_v4();
        let disabled = Uuid::new_v4();
        let revoked = Uuid::new_v4();
        insert_device(&pool, offline, "Needle % _", true, 30, None).await;
        insert_device(&pool, disabled, "Disabled", false, 20, None).await;
        insert_device(&pool, revoked, "Revoked", true, 10, Some(40)).await;
        assert!(gateway.connections().await.is_empty());

        let page = service
            .page(AdminDevicePageRequest {
                search: Some("Needle % _".to_owned()),
                ..AdminDevicePageRequest::default()
            })
            .await
            .expect("safe page");
        assert_eq!(page.items.len(), 1);
        let item = &page.items[0];
        assert_eq!(item.id, offline.to_string());
        assert_eq!(item.state, AdminDeviceState::Offline);
        assert_eq!(item.gateway_state, AdminGatewayState::Offline);
        assert_eq!(item.updated_at, 30);
        let encoded = serde_json::to_string(&page).expect("page JSON");
        assert!(!encoded.contains("token_hash"));
        assert!(!encoded.contains("070707"));

        let disabled = service
            .detail(&disabled.to_string())
            .await
            .expect("detail query")
            .expect("disabled device");
        assert_eq!(disabled.state, AdminDeviceState::Disabled);
        assert_eq!(disabled.gateway_state, AdminGatewayState::NotEnabled);
        assert_eq!(disabled.connected_at, None);
        assert!(disabled.capabilities.is_empty());

        let revoked = service
            .detail(&revoked.to_string())
            .await
            .expect("detail query")
            .expect("revoked device");
        assert_eq!(revoked.state, AdminDeviceState::Revoked);
        assert_eq!(revoked.updated_at, 40);
    }

    #[tokio::test]
    async fn page_validates_bounds_and_unicode_search_length() {
        let (_directory, _pool, _gateway, service) = service().await;
        assert_eq!(
            service
                .page(AdminDevicePageRequest {
                    limit: Some(0),
                    ..AdminDevicePageRequest::default()
                })
                .await,
            Err(AdminDeviceQueryError::InvalidPage)
        );
        assert_eq!(
            service
                .page(AdminDevicePageRequest {
                    limit: Some(101),
                    ..AdminDevicePageRequest::default()
                })
                .await,
            Err(AdminDeviceQueryError::InvalidPage)
        );
        assert_eq!(
            service
                .page(AdminDevicePageRequest {
                    search: Some("测".repeat(MAX_DEVICE_SEARCH_CHARS + 1)),
                    ..AdminDevicePageRequest::default()
                })
                .await,
            Err(AdminDeviceQueryError::InvalidSearch)
        );
    }
}
