use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{
    auth::DeviceAuthService,
    gateway::GatewayRuntime,
    outbox::{OutboxError, OutboxService},
    rate_limit::LocalRateLimiter,
    results::ResultService,
    shutdown::TaskSupervisor,
    wechat_admin_login::{AdminLoginAccessRegistry, WechatAdminLoginGrantRegistry},
    wechat_login::WechatLoginService,
    wechat_monitor::{MonitorState, MonitorStatus, WechatMonitorHandle},
};
use serde::Serialize;
use sqlx::SqlitePool;

pub const API_VERSION: u32 = 1;
pub const WECHAT_PROTOCOL_REFERENCE: &str = "2.4.6";

#[derive(Clone)]
pub struct AppState {
    readiness: Arc<AtomicBool>,
    pub db: SqlitePool,
    pub device_auth: DeviceAuthService,
    pub gateway: GatewayRuntime,
    pub outbox: OutboxService,
    pub results: ResultService,
    pub rate_limiter: LocalRateLimiter,
    pub wechat_login: Option<WechatLoginService>,
    pub wechat_admin_login_grants: WechatAdminLoginGrantRegistry,
    pub admin_login_access: AdminLoginAccessRegistry,
    pub wechat_monitor: Option<WechatMonitorHandle>,
    pub server_info: ServerInfo,
    pub shutdown: tokio_util::sync::CancellationToken,
}

impl AppState {
    pub fn new(db: SqlitePool, supervisor: TaskSupervisor) -> Self {
        Self::new_with_wechat_login(db, supervisor, None)
    }

    pub fn new_with_wechat_login(
        db: SqlitePool,
        supervisor: TaskSupervisor,
        wechat_login: Option<WechatLoginService>,
    ) -> Self {
        Self::new_with_wechat_runtime(db, supervisor, wechat_login, None)
    }

    pub fn new_with_wechat_runtime(
        db: SqlitePool,
        supervisor: TaskSupervisor,
        wechat_login: Option<WechatLoginService>,
        wechat_monitor: Option<WechatMonitorHandle>,
    ) -> Self {
        let outbox = OutboxService::new(db.clone());
        Self::new_with_services(db, supervisor, outbox, wechat_login, wechat_monitor)
    }

    pub fn new_with_services(
        db: SqlitePool,
        supervisor: TaskSupervisor,
        outbox: OutboxService,
        wechat_login: Option<WechatLoginService>,
        wechat_monitor: Option<WechatMonitorHandle>,
    ) -> Self {
        let wechat_enabled = wechat_login.is_some();
        let device_auth = DeviceAuthService::new(db.clone());
        let gateway = GatewayRuntime::new(device_auth.clone(), supervisor.clone());
        let (wechat_admin_login_grants, admin_login_access) =
            WechatAdminLoginGrantRegistry::new_pair();
        Self {
            readiness: Arc::new(AtomicBool::new(false)),
            device_auth,
            gateway,
            outbox,
            results: ResultService::disabled(db.clone()),
            rate_limiter: LocalRateLimiter::new(),
            wechat_login,
            wechat_admin_login_grants,
            admin_login_access,
            wechat_monitor,
            db,
            server_info: ServerInfo::phase_eight(wechat_enabled),
            shutdown: supervisor.cancellation_token(),
        }
    }

    pub async fn wechat_status(&self) -> Result<WechatChannelStatus, OutboxError> {
        let outbox = self.outbox.channel_status().await?;
        let Some(monitor) = &self.wechat_monitor else {
            return Ok(WechatChannelStatus {
                state: WechatConnectionState::Disconnected,
                account_fingerprint: None,
                last_poll_at: None,
                last_context_at: None,
                last_provider_accepted_at: outbox.last_provider_accepted_at,
                pending_notifications: outbox.pending_notifications,
                blocked_notifications: outbox.blocked_notifications,
                last_error_code: None,
            });
        };
        let safe = monitor.safe_status();
        let bundle = monitor.snapshot_bundle();
        let state = project_wechat_connection_state(safe, bundle.is_some());
        let account_fingerprint = bundle.as_ref().map(|bundle| {
            let digest = blake3::hash(bundle.credentials.account_id.as_bytes()).to_hex();
            format!("wx:{}", &digest[..12])
        });
        let last_context_at = bundle
            .as_ref()
            .and_then(|bundle| bundle.session.context_token_updated_at);
        Ok(WechatChannelStatus {
            state,
            account_fingerprint,
            last_poll_at: safe.last_poll_at,
            last_context_at,
            last_provider_accepted_at: outbox.last_provider_accepted_at,
            pending_notifications: outbox.pending_notifications,
            blocked_notifications: outbox.blocked_notifications,
            last_error_code: safe.error_code,
        })
    }

    pub fn mark_ready(&self) {
        self.readiness.store(true, Ordering::Release);
    }

    pub fn project_server_mode(&mut self, notification_only: bool) {
        if notification_only {
            self.server_info.features.retain(|feature| {
                !matches!(
                    feature.as_str(),
                    "remote_gateway_v5" | "remote_runs_v2" | "remote_harness_control_v2"
                )
            });
        }
    }

    pub async fn mark_not_ready(&self) {
        self.readiness.store(false, Ordering::Release);
        let _guard = self.wechat_admin_login_grants.lifecycle_guard().await;
        self.wechat_admin_login_grants.clear();
        let invalidated = self.admin_login_access.drain();
        if let Some(login) = &self.wechat_login {
            for (login_id, owner_device_id) in invalidated {
                let _ = login.cancel(owner_device_id, login_id);
            }
        }
    }

    pub fn is_ready(&self) -> bool {
        self.readiness.load(Ordering::Acquire)
    }
}

fn project_wechat_connection_state(
    status: MonitorStatus,
    has_bundle: bool,
) -> WechatConnectionState {
    if status.error_code == Some("WECHAT_CREDENTIALS_UNREADABLE") {
        return WechatConnectionState::CredentialsUnreadable;
    }
    match status.monitor {
        MonitorState::ReconnectRequired => WechatConnectionState::NeedsReconnect,
        MonitorState::Reconnecting | MonitorState::Degraded => WechatConnectionState::Degraded,
        MonitorState::Active if status.activated && has_bundle => WechatConnectionState::Ready,
        MonitorState::Stopped | MonitorState::Starting | MonitorState::Active if has_bundle => {
            WechatConnectionState::ConnectedAwaitingActivation
        }
        MonitorState::Stopped | MonitorState::Starting | MonitorState::Active => {
            WechatConnectionState::Disconnected
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WechatConnectionState {
    Disconnected,
    ConnectedAwaitingActivation,
    Ready,
    Degraded,
    NeedsReconnect,
    CredentialsUnreadable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatChannelStatus {
    pub state: WechatConnectionState,
    pub account_fingerprint: Option<String>,
    pub last_poll_at: Option<i64>,
    pub last_context_at: Option<i64>,
    pub last_provider_accepted_at: Option<i64>,
    pub pending_notifications: i64,
    pub blocked_notifications: i64,
    pub last_error_code: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub api_version: u32,
    pub server_version: String,
    pub features: Vec<String>,
    pub wechat_protocol_reference: String,
}

impl ServerInfo {
    fn phase_eight(wechat_login: bool) -> Self {
        let mut features = vec![
            "notifications".to_owned(),
            "device_status_v1".to_owned(),
            "device_scopes_v1".to_owned(),
            "remote_gateway_v5".to_owned(),
            "remote_runs_v2".to_owned(),
            "remote_harness_control_v2".to_owned(),
            "wechat_handoff_preflight_v1".to_owned(),
        ];
        if wechat_login {
            features.push("wechat_login".to_owned());
            features.push("wechat_channel".to_owned());
            features.push("wechat_admin_login_grant_v1".to_owned());
        }
        Self {
            api_version: API_VERSION,
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            features,
            wechat_protocol_reference: WECHAT_PROTOCOL_REFERENCE.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor_status(
        monitor: MonitorState,
        activated: bool,
        error_code: Option<&'static str>,
    ) -> MonitorStatus {
        MonitorStatus {
            monitor,
            activated,
            error_code,
            ..MonitorStatus::default()
        }
    }

    #[test]
    fn channel_projection_has_distinct_transient_and_terminal_states() {
        for monitor in [MonitorState::Reconnecting, MonitorState::Degraded] {
            assert_eq!(
                project_wechat_connection_state(monitor_status(monitor, false, None), true),
                WechatConnectionState::Degraded
            );
        }
        assert_eq!(
            project_wechat_connection_state(
                monitor_status(MonitorState::ReconnectRequired, false, None),
                true
            ),
            WechatConnectionState::NeedsReconnect
        );
        assert_eq!(
            project_wechat_connection_state(monitor_status(MonitorState::Active, true, None), true),
            WechatConnectionState::Ready
        );
        assert_eq!(
            project_wechat_connection_state(
                monitor_status(MonitorState::Active, false, None),
                true
            ),
            WechatConnectionState::ConnectedAwaitingActivation
        );
        assert_eq!(
            project_wechat_connection_state(
                monitor_status(MonitorState::Stopped, false, None),
                false
            ),
            WechatConnectionState::Disconnected
        );
    }

    #[test]
    fn credentials_unreadable_has_priority_over_internal_monitor_state() {
        for monitor in [
            MonitorState::Stopped,
            MonitorState::Degraded,
            MonitorState::ReconnectRequired,
        ] {
            assert_eq!(
                project_wechat_connection_state(
                    monitor_status(monitor, false, Some("WECHAT_CREDENTIALS_UNREADABLE")),
                    false
                ),
                WechatConnectionState::CredentialsUnreadable
            );
        }
    }
}
