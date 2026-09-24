use serde::Serialize;
use sqlx::{Row as _, SqlitePool};
use thiserror::Error;

use crate::config::AdminMode;

use super::overview::{AlertComponent, AlertSeverity, CurrentAlert};
use crate::admin::{
    ADMIN_SCHEMA_VERSION,
    health::{RuntimeHealthRegistry, WorkerKey, WorkerState},
    state::{SafeAdminRuntimeConfig, SafePublicBindClass},
};

#[derive(Clone)]
pub struct AdminSystemQueryService {
    pool: SqlitePool,
    health: RuntimeHealthRegistry,
    config: SafeAdminRuntimeConfig,
}

impl AdminSystemQueryService {
    pub fn new(
        pool: SqlitePool,
        health: RuntimeHealthRegistry,
        config: SafeAdminRuntimeConfig,
    ) -> Self {
        Self {
            pool,
            health,
            config,
        }
    }

    pub async fn snapshot(&self) -> Result<SystemSnapshot, AdminSystemQueryError> {
        let generated_at = unix_timestamp_ms()?;
        let health = self.health.snapshot();
        let retention_health = health.get(&WorkerKey::Retention);
        let retention_last_run_at = retention_health.and_then(|item| item.last_tick_at);
        let retention_last_result =
            if retention_health.is_some_and(|item| item.state == WorkerState::Failed) {
                RetentionResult::Failed
            } else if retention_last_run_at.is_some() {
                RetentionResult::Success
            } else {
                RetentionResult::NotObserved
            };
        let database = self.database(retention_last_run_at).await?;
        let current_alerts = health
            .iter()
            .filter(|(_, item)| item.state == WorkerState::Failed)
            .map(|(key, item)| CurrentAlert {
                id: format!("worker-{key:?}").to_ascii_lowercase(),
                code: item.last_error_code.unwrap_or("WORKER_FAILED"),
                severity: AlertSeverity::Error,
                component: AlertComponent::Relay,
                message: "a relay worker is unavailable",
                observed_at: item
                    .last_tick_at
                    .or(item.started_at)
                    .unwrap_or(generated_at),
            })
            .collect();
        let workers = health
            .into_iter()
            .map(|(key, health)| WorkerSummary {
                name: worker_name(key),
                state: project_worker_state(health.state),
                last_tick_at: health.last_tick_at,
                detail: health.last_error_code.or_else(|| {
                    (heartbeat_worker(key)
                        && health.state == WorkerState::Running
                        && health.last_tick_at.is_none())
                    .then_some("heartbeat_not_observed")
                }),
            })
            .collect();
        Ok(SystemSnapshot {
            schema_version: ADMIN_SCHEMA_VERSION,
            generated_at,
            build: BuildSummary {
                relay_version: env!("CARGO_PKG_VERSION"),
                git_commit: option_env!("PROMPTDOCK_GIT_COMMIT"),
                build_time: option_env!("PROMPTDOCK_BUILD_TIME"),
                rust_version: option_env!("PROMPTDOCK_RUST_VERSION"),
                api_version: "admin/v2",
                gateway_version: "gateway/v5",
            },
            database,
            workers,
            retention: RetentionSummary {
                accepted_days: self.config.retention.accepted_days,
                dead_letter_days: self.config.retention.dead_letter_days,
                inbound_terminal_days: self.config.retention.inbound_terminal_days,
                inbound_expired_days: self.config.retention.inbound_expired_days,
                last_run_at: retention_last_run_at,
                last_result: retention_last_result,
            },
            configuration: SafeConfigurationSummary {
                wechat_enabled: self.config.wechat_enabled,
                public_bind_class: self.config.public_bind_class,
                admin_bind_class: "loopback",
                admin_mode: self.config.mode,
                forwarded_https_observed: false,
                feature_flags: admin_feature_flags(self.config.mode, self.config.wechat_enabled),
            },
            current_alerts,
        })
    }

    async fn database(
        &self,
        last_retention_pass_at: Option<i64>,
    ) -> Result<DatabaseSummary, AdminSystemQueryError> {
        let quick_check: String = sqlx::query_scalar("PRAGMA quick_check(1)")
            .fetch_one(&self.pool)
            .await
            .map_err(|_| AdminSystemQueryError::Database)?;
        let row = sqlx::query(
            "SELECT
               (SELECT value FROM app_metadata WHERE key = 'schema_identity') schema_identity,
               CAST((SELECT value FROM app_metadata WHERE key = 'schema_revision') AS INTEGER) schema_revision,
               (SELECT page_count FROM pragma_page_count) page_count,
               (SELECT page_size FROM pragma_page_size) page_size,
               (SELECT journal_mode FROM pragma_journal_mode) journal_mode,
               (SELECT foreign_keys FROM pragma_foreign_keys) foreign_keys",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| AdminSystemQueryError::Database)?;
        let page_count: i64 = row
            .try_get("page_count")
            .map_err(|_| AdminSystemQueryError::Database)?;
        let page_size: i64 = row
            .try_get("page_size")
            .map_err(|_| AdminSystemQueryError::Database)?;
        let journal_mode: String = row
            .try_get("journal_mode")
            .map_err(|_| AdminSystemQueryError::Database)?;
        Ok(DatabaseSummary {
            schema_identity: row
                .try_get("schema_identity")
                .map_err(|_| AdminSystemQueryError::Database)?,
            schema_revision: row
                .try_get("schema_revision")
                .map_err(|_| AdminSystemQueryError::Database)?,
            integrity_status: if quick_check == "ok" {
                DatabaseIntegrity::Ok
            } else {
                DatabaseIntegrity::Failed
            },
            wal_status: if journal_mode.eq_ignore_ascii_case("wal") {
                WalStatus::Enabled
            } else {
                WalStatus::Disabled
            },
            foreign_keys_enabled: row
                .try_get::<i64, _>("foreign_keys")
                .map_err(|_| AdminSystemQueryError::Database)?
                == 1,
            pool_health: ComponentHealth::Healthy,
            size_bucket: DatabaseSizeBucket::from_bytes(page_count.saturating_mul(page_size)),
            last_retention_pass_at,
        })
    }
}

fn heartbeat_worker(key: WorkerKey) -> bool {
    matches!(
        key,
        WorkerKey::Outbox | WorkerKey::Inbound | WorkerKey::Retention
    )
}

fn admin_feature_flags(mode: AdminMode, wechat_enabled: bool) -> Vec<&'static str> {
    if mode == AdminMode::Operator && wechat_enabled {
        vec![
            "admin_read_v2",
            "admin_device_manage_v2",
            "admin_wechat_status_v2",
            "admin_wechat_manage_v2",
            "admin_maintenance_v2",
        ]
    } else if mode == AdminMode::Operator {
        vec![
            "admin_read_v2",
            "admin_device_manage_v2",
            "admin_wechat_status_v2",
            "admin_maintenance_v2",
        ]
    } else {
        vec!["admin_read_v2", "admin_wechat_status_v2"]
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemSnapshot {
    pub schema_version: u32,
    pub generated_at: i64,
    pub build: BuildSummary,
    pub database: DatabaseSummary,
    pub workers: Vec<WorkerSummary>,
    pub retention: RetentionSummary,
    pub configuration: SafeConfigurationSummary,
    pub current_alerts: Vec<CurrentAlert>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildSummary {
    pub relay_version: &'static str,
    pub git_commit: Option<&'static str>,
    pub build_time: Option<&'static str>,
    pub rust_version: Option<&'static str>,
    pub api_version: &'static str,
    pub gateway_version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSummary {
    pub schema_identity: String,
    pub schema_revision: i64,
    pub integrity_status: DatabaseIntegrity,
    pub wal_status: WalStatus,
    pub foreign_keys_enabled: bool,
    pub pool_health: ComponentHealth,
    pub size_bucket: DatabaseSizeBucket,
    pub last_retention_pass_at: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseIntegrity {
    Ok,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WalStatus {
    Enabled,
    Disabled,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentHealth {
    Healthy,
}

#[derive(Debug, Serialize)]
pub enum DatabaseSizeBucket {
    #[serde(rename = "lt_10mb")]
    Lt10mb,
    #[serde(rename = "10_to_100mb")]
    Mb10To100,
    #[serde(rename = "100_to_500mb")]
    Mb100To500,
    #[serde(rename = "gt_500mb")]
    Gt500mb,
}

impl DatabaseSizeBucket {
    fn from_bytes(bytes: i64) -> Self {
        match bytes {
            value if value < 10 * 1024 * 1024 => Self::Lt10mb,
            value if value < 100 * 1024 * 1024 => Self::Mb10To100,
            value if value < 500 * 1024 * 1024 => Self::Mb100To500,
            _ => Self::Gt500mb,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerSummary {
    pub name: &'static str,
    pub state: SafeWorkerState,
    pub last_tick_at: Option<i64>,
    pub detail: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeWorkerState {
    Running,
    Idle,
    Degraded,
    Failed,
    Disabled,
    Stopping,
}

fn project_worker_state(state: WorkerState) -> SafeWorkerState {
    match state {
        WorkerState::Running | WorkerState::Starting => SafeWorkerState::Running,
        WorkerState::Idle | WorkerState::Stopped => SafeWorkerState::Idle,
        WorkerState::Degraded => SafeWorkerState::Degraded,
        WorkerState::Failed => SafeWorkerState::Failed,
        WorkerState::Disabled => SafeWorkerState::Disabled,
        WorkerState::Stopping => SafeWorkerState::Stopping,
    }
}

fn worker_name(key: WorkerKey) -> &'static str {
    match key {
        WorkerKey::Outbox => "outbox-worker",
        WorkerKey::Inbound => "inbound-worker",
        WorkerKey::Retention => "retention-worker",
        WorkerKey::WechatMonitor => "wechat-monitor",
        WorkerKey::Gateway => "gateway-supervisor",
        WorkerKey::AdminHttp => "admin-http",
        WorkerKey::PublicHttp => "public-http",
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionSummary {
    pub accepted_days: u32,
    pub dead_letter_days: u32,
    pub inbound_terminal_days: u32,
    pub inbound_expired_days: u32,
    pub last_run_at: Option<i64>,
    pub last_result: RetentionResult,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionResult {
    Success,
    Failed,
    NotObserved,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeConfigurationSummary {
    pub wechat_enabled: bool,
    pub public_bind_class: SafePublicBindClass,
    pub admin_bind_class: &'static str,
    pub admin_mode: AdminMode,
    pub forwarded_https_observed: bool,
    pub feature_flags: Vec<&'static str>,
}

impl Serialize for SafePublicBindClass {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self {
            Self::Loopback => "loopback",
            Self::Container => "container",
        })
    }
}

impl Serialize for AdminMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self {
            Self::ReadOnly => "read_only",
            Self::Operator => "operator",
        })
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AdminSystemQueryError {
    #[error("system query is unavailable")]
    Database,
    #[error("system clock is unavailable")]
    Clock,
}

fn unix_timestamp_ms() -> Result<i64, AdminSystemQueryError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .ok_or(AdminSystemQueryError::Clock)
}
