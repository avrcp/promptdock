use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use async_trait::async_trait;
use relay_provider_wechat::{
    credentials::{ConnectionBundle, CredentialValidationError},
    http_client::{WechatHttpClient, WechatHttpError},
    protocol::GetUpdatesResponse,
};
use serde::Serialize;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::inbound::InboundMessageSink;
use crate::wechat::secret_store::{SecretStore, SecretStoreError};

use super::poll::set_terminal_failure;

pub const ERROR_AUTHENTICATION_EXPIRED: &str = "WECHAT_AUTHENTICATION_EXPIRED";
pub const ERROR_INVALID_ENDPOINT: &str = "WECHAT_INVALID_ENDPOINT";
pub const ERROR_MONITOR_NETWORK: &str = "WECHAT_MONITOR_NETWORK";
pub const ERROR_MONITOR_PROTOCOL: &str = "WECHAT_MONITOR_PROTOCOL";
pub const ERROR_MONITOR_REJECTED: &str = "WECHAT_MONITOR_REJECTED";
pub const ERROR_MONITOR_PERSIST_FAILED: &str = "WECHAT_MONITOR_PERSIST_FAILED";
pub const ERROR_MONITOR_CAPTURE_FAILED: &str = "WECHAT_MONITOR_CAPTURE_FAILED";
pub const ERROR_MONITOR_WORKER_EXITED: &str = "WECHAT_MONITOR_WORKER_EXITED";

/// A safe, secret-free description of the single getupdates owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorStatus {
    pub activated: bool,
    pub monitor: MonitorState,
    pub last_inbound_at: Option<i64>,
    pub last_poll_at: Option<i64>,
    pub error_code: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorState {
    #[default]
    Stopped,
    Starting,
    Active,
    Reconnecting,
    Degraded,
    ReconnectRequired,
}

/// A secret-bearing in-process snapshot for the sender. It is never serialized
/// or logged; Debug intentionally reports only the monotonic revision.
#[derive(Clone)]
pub struct ConnectionSnapshot {
    pub bundle: ConnectionBundle,
    pub revision: u64,
}

impl fmt::Debug for ConnectionSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionSnapshot")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// Allows the outbox integration to release activation- and reconnect-blocked
/// rows without coupling the monitor to the outbox implementation.
#[async_trait]
pub trait MonitorActivationHook: Send + Sync {
    async fn activated(&self) {}

    async fn reconnected(&self) {}
}

#[derive(Default)]
pub struct NoopMonitorActivationHook;

#[async_trait]
impl MonitorActivationHook for NoopMonitorActivationHook {}

#[async_trait]
pub trait WechatMonitorTransport: Send + Sync {
    async fn notify_start(
        &self,
        base_url: &Url,
        token: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), WechatHttpError>;

    async fn get_updates(
        &self,
        base_url: &Url,
        token: &str,
        get_updates_buf: &str,
        cancellation: &CancellationToken,
    ) -> Result<GetUpdatesResponse, WechatHttpError>;

    async fn notify_stop(
        &self,
        base_url: &Url,
        token: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), WechatHttpError>;
}

#[async_trait]
impl WechatMonitorTransport for WechatHttpClient {
    async fn notify_start(
        &self,
        base_url: &Url,
        token: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), WechatHttpError> {
        WechatHttpClient::notify_start(self, base_url, token, cancellation)
            .await
            .map(|_| ())
    }

    async fn get_updates(
        &self,
        base_url: &Url,
        token: &str,
        get_updates_buf: &str,
        cancellation: &CancellationToken,
    ) -> Result<GetUpdatesResponse, WechatHttpError> {
        WechatHttpClient::get_updates(self, base_url, token, get_updates_buf, None, cancellation)
            .await
    }

    async fn notify_stop(
        &self,
        base_url: &Url,
        token: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), WechatHttpError> {
        WechatHttpClient::notify_stop(self, base_url, token, cancellation)
            .await
            .map(|_| ())
    }
}

#[derive(Debug, Error)]
pub enum WechatMonitorError {
    #[error("WeChat connection bundle is invalid")]
    InvalidBundle(#[from] CredentialValidationError),
    #[error("WeChat secret store failed")]
    SecretStore(#[from] SecretStoreError),
    #[error("WeChat monitor worker failed")]
    Worker,
}

#[derive(Clone)]
pub struct WechatMonitorHandle {
    pub(super) shared: Arc<Shared>,
}

impl WechatMonitorHandle {
    pub fn connection_snapshot(&self) -> Option<ConnectionSnapshot> {
        let state = self.shared.lock_state();
        state.bundle.clone().map(|bundle| ConnectionSnapshot {
            bundle,
            revision: state.revision,
        })
    }

    pub fn snapshot_bundle(&self) -> Option<ConnectionBundle> {
        self.connection_snapshot().map(|snapshot| snapshot.bundle)
    }

    pub fn revision(&self) -> u64 {
        self.shared.lock_state().revision
    }

    pub fn safe_status(&self) -> MonitorStatus {
        self.shared.lock_state().status
    }

    /// Lets the sender fail closed when sendmessage proves that its connection
    /// snapshot is no longer usable. The encrypted bundle is retained so the
    /// monitor (or a later login cutover) remains the recovery authority.
    pub fn mark_reconnect_required(&self, error_code: &'static str) {
        set_terminal_failure(&self.shared, error_code);
    }

    pub fn mark_reconnect_required_if_revision(
        &self,
        expected_revision: u64,
        error_code: &'static str,
    ) -> bool {
        let mut state = self.shared.lock_state();
        if state.revision != expected_revision {
            return false;
        }
        state.status.activated = false;
        state.status.monitor = MonitorState::ReconnectRequired;
        state.status.error_code = Some(error_code);
        true
    }

    /// Keeps the long-poll owner alive while preventing sends from reusing a
    /// provider-rejected context. A later owned inbound context transitions
    /// back to activated and invokes the activation hook.
    pub fn mark_activation_required(&self, error_code: &'static str) {
        let mut state = self.shared.lock_state();
        if state.status.monitor == MonitorState::ReconnectRequired {
            return;
        }
        state.activation_required = true;
        state.status.activated = false;
        state.status.error_code = Some(error_code);
    }

    pub fn mark_activation_required_if_revision(
        &self,
        expected_revision: u64,
        error_code: &'static str,
    ) -> bool {
        let mut state = self.shared.lock_state();
        if state.revision != expected_revision {
            return false;
        }
        if state.status.monitor == MonitorState::ReconnectRequired {
            return false;
        }
        state.activation_required = true;
        state.status.activated = false;
        state.status.error_code = Some(error_code);
        true
    }
}

pub(super) struct Shared {
    pub(super) state: Mutex<SharedState>,
    pub(super) transport: Arc<dyn WechatMonitorTransport>,
    pub(super) store: Arc<dyn SecretStore>,
    pub(super) sink: Arc<dyn InboundMessageSink>,
    pub(super) hook: Arc<dyn MonitorActivationHook>,
    pub(super) reconnect_delay: Duration,
    pub(super) max_reconnect_delay: Duration,
}

pub(super) struct SharedState {
    pub(super) bundle: Option<ConnectionBundle>,
    pub(super) revision: u64,
    pub(super) status: MonitorStatus,
    pub(super) activation_required: bool,
}

impl Shared {
    pub(super) fn lock_state(&self) -> MutexGuard<'_, SharedState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
