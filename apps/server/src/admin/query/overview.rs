use std::{collections::BTreeMap, time::Duration};

use serde::Serialize;
use sqlx::{Row as _, SqlitePool};
use thiserror::Error;

use crate::{
    gateway::GatewayRuntime,
    state::{AppState, WechatChannelStatus, WechatConnectionState},
};

use super::super::{
    ADMIN_SCHEMA_VERSION,
    health::{RuntimeHealthRegistry, WorkerKey, WorkerState},
    state::AdminState,
};

#[derive(Clone)]
pub struct AdminOverviewQueryService {
    pool: SqlitePool,
    app: AppState,
    gateway: GatewayRuntime,
    health: RuntimeHealthRegistry,
    wechat_enabled: bool,
}

impl AdminOverviewQueryService {
    pub fn new(state: &AdminState) -> Self {
        Self {
            pool: state.app.db.clone(),
            app: state.app.clone(),
            gateway: state.app.gateway.clone(),
            health: state.health.clone(),
            wechat_enabled: state.config.wechat_enabled,
        }
    }

    pub async fn snapshot(
        &self,
        started_at: std::time::Instant,
    ) -> Result<AdminOverview, AdminOverviewQueryError> {
        let generated_at = unix_timestamp_ms()?;
        let wechat = self
            .app
            .wechat_status()
            .await
            .map_err(|_| AdminOverviewQueryError::Database)?;
        let queue = queue_summary(&self.pool).await?;
        let connections = self.gateway.connections().await;
        let device_rows =
            sqlx::query("SELECT id, name, enabled, revoked_at, last_seen_at FROM devices")
                .fetch_all(&self.pool)
                .await
                .map_err(|_| AdminOverviewQueryError::Database)?;
        let connection_map = connections
            .iter()
            .map(|item| (item.device_id.to_string(), item))
            .collect::<BTreeMap<_, _>>();
        let names = device_rows
            .iter()
            .filter_map(|row| {
                Some((
                    row.try_get::<String, _>("id").ok()?,
                    row.try_get::<String, _>("name").ok()?,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let mut devices = DeviceSummary::default();
        for row in &device_rows {
            let id: String = row
                .try_get("id")
                .map_err(|_| AdminOverviewQueryError::Database)?;
            let enabled = row
                .try_get::<i64, _>("enabled")
                .map_err(|_| AdminOverviewQueryError::Database)?
                == 1;
            let revoked_at: Option<i64> = row
                .try_get("revoked_at")
                .map_err(|_| AdminOverviewQueryError::Database)?;
            let last_seen_at: Option<i64> = row
                .try_get("last_seen_at")
                .map_err(|_| AdminOverviewQueryError::Database)?;
            if revoked_at.is_some() {
                devices.states.revoked += 1;
            } else if !enabled {
                devices.states.disabled += 1;
            } else {
                devices.enabled_total += 1;
                if connection_map.contains_key(&id) {
                    devices.online_total += 1;
                    devices.states.online += 1;
                } else {
                    devices.states.offline += 1;
                    if last_seen_at.is_some_and(|seen| {
                        generated_at.saturating_sub(seen)
                            <= Duration::from_secs(86_400).as_millis() as i64
                    }) {
                        devices.recently_offline_total += 1;
                    }
                }
            }
        }
        let gateway_connections = connections
            .into_iter()
            .map(|item| GatewayConnection {
                device_id: item.device_id.to_string(),
                device_name: names
                    .get(&item.device_id.to_string())
                    .cloned()
                    .unwrap_or_else(|| "registered device".to_owned()),
                generation: item.generation,
                connected: true,
                last_heartbeat_at: Some(item.last_pong_at),
                client_version: Some(item.client_version),
            })
            .collect();
        let workers = self.health.snapshot();
        let critical_failed = workers.iter().any(|(key, item)| {
            item.state == WorkerState::Failed
                && matches!(
                    key,
                    WorkerKey::Outbox
                        | WorkerKey::Inbound
                        | WorkerKey::Retention
                        | WorkerKey::Gateway
                        | WorkerKey::AdminHttp
                        | WorkerKey::PublicHttp
                )
        });
        let wechat_degraded = self.wechat_enabled
            && matches!(
                wechat.state,
                WechatConnectionState::Disconnected
                    | WechatConnectionState::ConnectedAwaitingActivation
                    | WechatConnectionState::Degraded
                    | WechatConnectionState::NeedsReconnect
                    | WechatConnectionState::CredentialsUnreadable
            );
        let health = if !self.app.is_ready() || critical_failed {
            RelayHealth::Failed
        } else if wechat_degraded
            || queue.blocked > 0
            || queue.failed > 0
            || workers.values().any(|item| {
                matches!(
                    item.state,
                    WorkerState::Degraded
                        | WorkerState::Failed
                        | WorkerState::Starting
                        | WorkerState::Stopping
                )
            })
        {
            RelayHealth::Degraded
        } else {
            RelayHealth::Healthy
        };
        let current_alerts = current_alerts(generated_at, &wechat, &queue, &workers);
        Ok(AdminOverview {
            schema_version: ADMIN_SCHEMA_VERSION,
            generated_at,
            relay: RelaySummary {
                health,
                version: env!("CARGO_PKG_VERSION"),
                uptime_seconds: started_at.elapsed().as_secs(),
                last_check_at: generated_at,
            },
            wechat: WechatSummary::from_status(wechat, queue.clone()),
            devices,
            queue,
            gateway_connections,
            current_alerts,
        })
    }
}

pub async fn queue_summary(pool: &SqlitePool) -> Result<QueueSummary, AdminOverviewQueryError> {
    let row = sqlx::query(
        "SELECT
           COALESCE(SUM(CASE WHEN status = 'pending_channel' THEN 1 ELSE 0 END), 0) pending,
           COALESCE(SUM(CASE WHEN status = 'sending_channel' THEN 1 ELSE 0 END), 0) sending,
           COALESCE(SUM(CASE WHEN status = 'retry_wait' THEN 1 ELSE 0 END), 0) retrying,
           COALESCE(SUM(CASE WHEN status IN ('blocked_activation', 'blocked_reconnect') THEN 1 ELSE 0 END), 0) blocked,
           COALESCE(SUM(CASE WHEN status = 'dead_letter' THEN 1 ELSE 0 END), 0) failed
         FROM notification_outbox",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| AdminOverviewQueryError::Database)?;
    Ok(QueueSummary {
        pending: row
            .try_get("pending")
            .map_err(|_| AdminOverviewQueryError::Database)?,
        sending: row
            .try_get("sending")
            .map_err(|_| AdminOverviewQueryError::Database)?,
        retrying: row
            .try_get("retrying")
            .map_err(|_| AdminOverviewQueryError::Database)?,
        blocked: row
            .try_get("blocked")
            .map_err(|_| AdminOverviewQueryError::Database)?,
        failed: row
            .try_get("failed")
            .map_err(|_| AdminOverviewQueryError::Database)?,
    })
}

fn current_alerts(
    now: i64,
    wechat: &WechatChannelStatus,
    queue: &QueueSummary,
    workers: &BTreeMap<super::super::health::WorkerKey, super::super::health::WorkerHealth>,
) -> Vec<CurrentAlert> {
    let mut alerts = Vec::new();
    if let Some(code) = wechat.last_error_code {
        alerts.push(CurrentAlert::wechat(now, code));
    }
    if queue.blocked > 0 {
        alerts.push(CurrentAlert::outbox(
            now,
            "OUTBOX_BLOCKED",
            "notifications are waiting for channel recovery",
        ));
    }
    if queue.failed > 0 {
        alerts.push(CurrentAlert::outbox(
            now,
            "OUTBOX_DEAD_LETTER",
            "notifications require operator review",
        ));
    }
    for (worker, health) in workers {
        if health.state == WorkerState::Failed {
            alerts.push(CurrentAlert {
                id: format!("worker-{worker:?}").to_ascii_lowercase(),
                code: health.last_error_code.unwrap_or("WORKER_FAILED"),
                severity: AlertSeverity::Error,
                component: AlertComponent::Relay,
                message: "a relay worker is unavailable",
                observed_at: health.last_tick_at.or(health.started_at).unwrap_or(now),
            });
        }
    }
    alerts
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminOverview {
    pub schema_version: u32,
    pub generated_at: i64,
    pub relay: RelaySummary,
    pub wechat: WechatSummary,
    pub devices: DeviceSummary,
    pub queue: QueueSummary,
    pub gateway_connections: Vec<GatewayConnection>,
    pub current_alerts: Vec<CurrentAlert>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelaySummary {
    pub health: RelayHealth,
    pub version: &'static str,
    pub uptime_seconds: u64,
    pub last_check_at: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayHealth {
    Healthy,
    Degraded,
    Failed,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub enabled_total: u64,
    pub online_total: u64,
    pub recently_offline_total: u64,
    pub states: DeviceStateCount,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStateCount {
    pub online: u64,
    pub offline: u64,
    pub disabled: u64,
    pub revoked: u64,
    pub unknown: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSummary {
    pub pending: i64,
    pub sending: i64,
    pub retrying: i64,
    pub blocked: i64,
    pub failed: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatSummary {
    pub state: crate::state::WechatConnectionState,
    pub account_hint: Option<String>,
    pub last_poll_at: Option<i64>,
    pub last_context_at: Option<i64>,
    pub last_provider_accepted_at: Option<i64>,
    pub queue: QueueSummary,
    pub last_error_code: Option<&'static str>,
}

impl WechatSummary {
    fn from_status(status: WechatChannelStatus, queue: QueueSummary) -> Self {
        Self {
            state: status.state,
            account_hint: status.account_fingerprint,
            last_poll_at: status.last_poll_at,
            last_context_at: status.last_context_at,
            last_provider_accepted_at: status.last_provider_accepted_at,
            queue,
            last_error_code: status.last_error_code,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConnection {
    pub device_id: String,
    pub device_name: String,
    pub generation: u64,
    pub connected: bool,
    pub last_heartbeat_at: Option<i64>,
    pub client_version: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentAlert {
    pub id: String,
    pub code: &'static str,
    pub severity: AlertSeverity,
    pub component: AlertComponent,
    pub message: &'static str,
    pub observed_at: i64,
}

impl CurrentAlert {
    fn wechat(now: i64, code: &'static str) -> Self {
        Self {
            id: format!("wechat-{}", code.to_ascii_lowercase()),
            code,
            severity: AlertSeverity::Warning,
            component: AlertComponent::Wechat,
            message: "the WeChat channel requires attention",
            observed_at: now,
        }
    }
    fn outbox(now: i64, code: &'static str, message: &'static str) -> Self {
        Self {
            id: format!("outbox-{}", code.to_ascii_lowercase()),
            code,
            severity: AlertSeverity::Warning,
            component: AlertComponent::Outbox,
            message,
            observed_at: now,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertComponent {
    Relay,
    Wechat,
    Outbox,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AdminOverviewQueryError {
    #[error("admin overview database query failed")]
    Database,
    #[error("system clock is unavailable")]
    Clock,
}

fn unix_timestamp_ms() -> Result<i64, AdminOverviewQueryError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .ok_or(AdminOverviewQueryError::Clock)
}
