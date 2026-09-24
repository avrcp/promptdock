use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use url::Url;
use zeroize::Zeroizing;

use crate::error::AppError;
#[cfg(test)]
use crate::model::NotificationBackendKind;
use crate::notification::outbox::{
    DeliveryTransport, NotificationEventKind, NotificationPayloadV2, OutboxRepository,
    RelayRemoteStatusUpdate, NOTIFICATION_PAYLOAD_SCHEMA_VERSION,
};
use crate::notification::runtime::NotificationRuntimeState;
use crate::notification::TestNotificationResult;

use super::credentials::{RelayConnectionProfile, RelayCredentialError, RelayCredentialStore};
use super::http_client::{RelayHttpClient, RelayHttpError};
use super::protocol::{RelayNotificationStatus, RelayProbeResult};

enum RemoteReceipt {
    Short(RelayNotificationStatus),
    Result(super::result_protocol::RelayResultReceipt),
}

use super::sender::RelaySender;

const REMOTE_RECONCILIATION_LIMIT: u16 = 25;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelayStatus {
    pub(crate) runtime_epoch: String,
    pub(crate) revision: u64,
    pub(crate) state: String,
    pub(crate) can_submit: Option<bool>,
    pub(crate) can_read_own: Option<bool>,
    pub(crate) configured: bool,
    pub(crate) reachable: bool,
    pub(crate) authenticated: bool,
    pub(crate) api_version: Option<u32>,
    pub(crate) server_version: Option<String>,
    pub(crate) features: Vec<String>,
    pub(crate) notification_capable: bool,
    pub(crate) scoped_auth_capable: bool,
    pub(crate) endpoint_hint: Option<String>,
    pub(crate) last_checked_at: Option<i64>,
    pub(crate) last_success_at: Option<i64>,
    pub(crate) last_error_code: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelayRemoteReconciliationResult {
    pub(crate) examined: u16,
    pub(crate) updated: u16,
    pub(crate) unchanged: u16,
    pub(crate) failed: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error_code: Option<&'static str>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelayReceiptSyncDiagnostics {
    pub(crate) last_attempt_at: Option<i64>,
    pub(crate) last_success_at: Option<i64>,
    pub(crate) last_error_code: Option<String>,
    pub(crate) total_examined: u64,
    pub(crate) total_updated: u64,
    pub(crate) total_failed: u64,
}

#[derive(Clone)]
pub(crate) struct RelayRuntimeState {
    inner: Arc<RelayRuntimeInner>,
}

type StatusChanged = Arc<dyn Fn(RelayStatus) + Send + Sync>;

struct RelayRuntimeInner {
    store: Arc<RelayCredentialStore>,
    client: Option<RelayHttpClient>,
    operation_lock: Mutex<()>,
    credential_revision: AtomicU64,
    status: RwLock<RelayStatus>,
    status_changed: RwLock<Option<StatusChanged>>,
    probe_lock: Mutex<()>,
    receipt_lock: Mutex<()>,
    result_action_lock: Mutex<()>,
    receipt_cursor: RwLock<Option<String>>,
    outbox: RwLock<Option<OutboxRepository>>,
    sender: RwLock<Option<Arc<RelaySender>>>,
    receipt_sync: RwLock<RelayReceiptSyncDiagnostics>,
    cancellation: CancellationToken,
}

impl RelayRuntimeState {
    /// Builds a local-only snapshot. Startup never contacts the Relay server and
    /// unreadable credentials never prevent PromptDock from launching.
    #[cfg(test)]
    pub(crate) fn start(app_data_dir: &Path) -> Self {
        Self::start_inner(app_data_dir)
    }

    fn start_inner(app_data_dir: &Path) -> Self {
        let store = Arc::new(RelayCredentialStore::new(app_data_dir));
        let configured = store.is_configured();
        let mut status = RelayStatus {
            configured,
            runtime_epoch: uuid::Uuid::new_v4().to_string(),
            state: if configured {
                "checking"
            } else {
                "not_configured"
            }
            .into(),
            ..RelayStatus::default()
        };
        if configured {
            match store.load_profile() {
                Ok(Some(profile)) => status.endpoint_hint = endpoint_hint(&profile.base_url),
                Ok(None) => status.configured = false,
                Err(error) => {
                    status.configured = !profile_error_requires_reconfiguration(&error);
                    status.last_error_code = Some("RELAY_PROFILE_UNREADABLE".into());
                }
            }
        }
        Self {
            inner: Arc::new(RelayRuntimeInner {
                store,
                client: RelayHttpClient::new().ok(),
                operation_lock: Mutex::new(()),
                credential_revision: AtomicU64::new(0),
                status: RwLock::new(status),
                status_changed: RwLock::new(None),
                probe_lock: Mutex::new(()),
                receipt_lock: Mutex::new(()),
                result_action_lock: Mutex::new(()),
                receipt_cursor: RwLock::new(None),
                outbox: RwLock::new(None),
                sender: RwLock::new(None),
                receipt_sync: RwLock::new(RelayReceiptSyncDiagnostics::default()),
                cancellation: CancellationToken::new(),
            }),
        }
    }

    pub(crate) fn start_with_delivery(app_data_dir: &Path, outbox: OutboxRepository) -> Self {
        let state = Self::start_inner(app_data_dir);
        *state
            .inner
            .outbox
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(outbox.clone());
        if let Some(client) = state.inner.client.clone() {
            let weak_inner = Arc::downgrade(&state.inner);
            let capabilities = Arc::downgrade(&state.inner);
            let sender = Arc::new(RelaySender::new(
                client,
                Arc::clone(&state.inner.store),
                tauri::async_runtime::handle().inner().clone(),
                Arc::new(move |profile, error| {
                    if let Some(inner) = weak_inner.upgrade() {
                        let Ok(_commit) = inner.operation_lock.try_lock() else {
                            return;
                        };
                        if !inner
                            .store
                            .load_profile()
                            .ok()
                            .flatten()
                            .is_some_and(|current| {
                                current.base_url == profile.base_url
                                    && current.device_token == profile.device_token
                            })
                        {
                            return;
                        }
                        match error {
                            Some(error) => record_operational_http_failure(&inner, error, false),
                            None => update_status(&inner, |status| {
                                status.reachable = true;
                                status.authenticated = true;
                                status.can_submit = Some(true);
                                status.last_checked_at = Some(now_ms());
                                status.last_success_at = status.last_checked_at;
                                status.last_error_code = None;
                            }),
                        }
                    }
                }),
                Arc::new(move || {
                    capabilities.upgrade().is_some_and(|inner| {
                        inner
                            .status
                            .read()
                            .unwrap_or_else(|p| p.into_inner())
                            .features
                            .iter()
                            .any(|feature| feature == "result_pages_v1")
                    })
                }),
                outbox.clone(),
            ));
            *state
                .inner
                .sender
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sender);
        }
        state
    }

    pub(crate) fn delivery_transport(&self) -> Option<Arc<dyn DeliveryTransport>> {
        self.inner
            .sender
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|sender| sender.clone() as Arc<dyn DeliveryTransport>)
    }

    /// A Relay delivery worker is allowed only after an authenticated feature
    /// probe proves notification support. A stored profile alone is not ready.
    pub(crate) fn notification_delivery_ready(&self) -> bool {
        let status = self.status();
        status.configured
            && status.reachable
            && status.authenticated
            && status.scoped_auth_capable
            && status.notification_capable
            && status.can_submit != Some(false)
    }

    pub(crate) fn status(&self) -> RelayStatus {
        self.inner
            .status
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn configured_base_url(&self) -> Option<String> {
        self.inner
            .store
            .load_profile()
            .ok()
            .flatten()
            .map(|profile| profile.base_url.clone())
    }

    pub(crate) async fn configure(
        &self,
        base_url: String,
        mut device_token: Zeroizing<String>,
    ) -> Result<RelayStatus, AppError> {
        let revision = self.inner.credential_revision.load(Ordering::Acquire);
        let client = self.client()?;
        let canonical_url = client
            .canonical_base_url(&base_url)
            .map_err(|error| relay_http_app_error(&error))?;
        let profile = RelayConnectionProfile::new(
            canonical_url,
            std::mem::take(&mut *device_token),
            now_ms(),
        )
        .map_err(|error| relay_credential_app_error(&error, "RELAY_PROFILE_INVALID"))?;
        let candidate_hint = endpoint_hint(&profile.base_url);
        let cancellation = self.inner.cancellation.child_token();
        let probe = match client
            .probe(&profile.base_url, &profile.device_token, &cancellation)
            .await
        {
            Ok(probe) => probe,
            Err(error) => return Err(relay_http_app_error(&error)),
        };

        let mut status = status_from_probe(probe, candidate_hint, now_ms());
        if !status.scoped_auth_capable {
            return Err(AppError::new(
                "RELAY_SCOPED_AUTH_REQUIRED",
                "Relay 服务器必须声明 device_scopes_v1，配置才可保存",
            ));
        }

        let _commit = self.inner.operation_lock.lock().await;
        if self.inner.credential_revision.load(Ordering::Acquire) != revision {
            return Err(AppError::new(
                "RELAY_CONFIGURATION_CHANGED",
                "Relay 配置已改变，请重试",
            ));
        }
        let store = Arc::clone(&self.inner.store);
        let outbox = self
            .inner
            .outbox
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        tokio::task::spawn_blocking(move || {
            let same_destination = store.load_profile().ok().flatten().is_some_and(|old| {
                Url::parse(&old.base_url).ok() == Url::parse(&profile.base_url).ok()
                    && old.device_id == profile.device_id
            });
            let save = || {
                store.save_profile(&profile).map_err(|error| {
                    relay_credential_app_error(&error, "RELAY_PROFILE_WRITE_FAILED")
                })
            };
            if !same_destination {
                if let Some(outbox) = outbox {
                    return outbox.guard_destination_change(save);
                }
            }
            save()
        })
        .await
        .map_err(|_| AppError::new("RELAY_PROFILE_WRITE_FAILED", "Relay 配置无法安全保存"))??;
        self.inner
            .credential_revision
            .fetch_add(1, Ordering::AcqRel);

        status.configured = true;
        let previous = self.status();
        self.replace_status(status.clone());
        if let Some(outbox) = self
            .inner
            .outbox
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            // Saving verified credentials is itself the corresponding recovery
            // event. Capability and target blocks still require their own facts.
            resume_blocked_delivery(&outbox, &previous, &status, true)?;
        }
        Ok(status)
    }

    pub(crate) async fn test_connection(&self) -> Result<RelayStatus, AppError> {
        self.probe_connection(true).await
    }

    /// Explicit diagnostics observes authentication and capabilities only. The
    /// independent maintenance owner remains responsible for resuming delivery.
    pub(crate) async fn probe_only(&self) -> Result<RelayStatus, AppError> {
        self.probe_connection(false).await
    }

    async fn probe_connection(&self, resume_delivery: bool) -> Result<RelayStatus, AppError> {
        let Ok(_probe) = self.inner.probe_lock.try_lock() else {
            return Ok(self.status());
        };
        let revision = self.inner.credential_revision.load(Ordering::Acquire);
        let client = self.client().inspect_err(|error| {
            self.record_local_failure(error.code);
        })?;
        let store = Arc::clone(&self.inner.store);
        let loaded = tokio::task::spawn_blocking(move || store.load_profile())
            .await
            .map_err(|_| AppError::new("RELAY_PROFILE_UNREADABLE", "Relay 配置暂时无法读取"));
        let read_commit = self.inner.operation_lock.lock().await;
        if self.inner.credential_revision.load(Ordering::Acquire) != revision {
            return Ok(self.status());
        }
        let profile = match loaded {
            Ok(Ok(Some(profile))) => profile,
            Ok(Ok(None)) => {
                self.replace_status(RelayStatus::default());
                return Err(AppError::new(
                    "RELAY_NOT_CONFIGURED",
                    "尚未配置 Relay 服务器",
                ));
            }
            Ok(Err(error)) => {
                let previous_checked_at = self.status().last_checked_at;
                self.replace_status(RelayStatus {
                    configured: !profile_error_requires_reconfiguration(&error),
                    last_checked_at: previous_checked_at,
                    last_error_code: Some("RELAY_PROFILE_UNREADABLE".into()),
                    ..RelayStatus::default()
                });
                return Err(relay_credential_app_error(
                    &error,
                    "RELAY_PROFILE_UNREADABLE",
                ));
            }
            Err(error) => {
                self.record_local_failure("RELAY_PROFILE_UNREADABLE");
                return Err(error);
            }
        };
        drop(read_commit);
        let hint = endpoint_hint(&profile.base_url);
        let cancellation = self.inner.cancellation.child_token();
        let result = client
            .probe(&profile.base_url, &profile.device_token, &cancellation)
            .await;
        let _commit = self.inner.operation_lock.lock().await;
        if self.inner.credential_revision.load(Ordering::Acquire) != revision {
            return Ok(self.status());
        }
        match result {
            Ok(probe) => {
                let mut status = status_from_probe(probe, hint, now_ms());
                let previous = self.status();
                status.can_submit = status.can_submit.or(previous.can_submit);
                status.can_read_own = status.can_read_own.or(previous.can_read_own);
                if !status.scoped_auth_capable {
                    status.last_error_code = Some("RELAY_SCOPED_AUTH_REQUIRED".into());
                    self.replace_status(status);
                    return Err(AppError::new(
                        "RELAY_SCOPED_AUTH_REQUIRED",
                        "Relay 服务器未声明 device_scopes_v1，不能继续使用",
                    ));
                }
                self.replace_status(status.clone());
                if resume_delivery {
                    if let Ok(outbox) = self.outbox() {
                        resume_blocked_delivery(&outbox, &previous, &status, false)?;
                    }
                }
                Ok(status)
            }
            Err(error) => {
                self.replace_status(status_from_probe_error(&error, hint, now_ms()));
                Err(relay_http_app_error(&error))
            }
        }
    }

    pub(crate) async fn refresh_remote_notification_statuses(
        &self,
    ) -> Result<RelayRemoteReconciliationResult, AppError> {
        let Ok(_single_flight) = self.inner.receipt_lock.try_lock() else {
            return Ok(RelayRemoteReconciliationResult::default());
        };
        let started_at = now_ms();
        self.update_receipt_sync(|diagnostics| {
            diagnostics.last_attempt_at = Some(started_at);
        });
        let started = Instant::now();
        let result = match tokio::time::timeout(
            Duration::from_secs(5),
            self.perform_remote_notification_refresh(),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(AppError::new(
                "RELAY_RECEIPT_BUDGET",
                "回执同步已达到本轮时间预算",
            )),
        };
        self.record_receipt_sync_result(&result);
        let (stage, code, attempt) = match &result {
            Ok(summary) if summary.failed == 0 => ("reconcile_complete", "OK", summary.examined),
            Ok(summary) => (
                "reconcile_partial",
                "RELAY_RECONCILIATION_PARTIAL_FAILURE",
                summary.examined,
            ),
            Err(error) => ("reconcile_failed", error.code, 0),
        };
        tracing::info!(
            backend = "relay",
            stage,
            attempt,
            duration_ms = bounded_duration_ms(started.elapsed()),
            code,
            "notification receipt reconciliation finished"
        );
        result
    }

    async fn perform_remote_notification_refresh(
        &self,
    ) -> Result<RelayRemoteReconciliationResult, AppError> {
        if self.inner.cancellation.is_cancelled() {
            return Err(AppError::new(
                "RELAY_RECONCILIATION_CANCELLED",
                "Relay 回执刷新已取消",
            ));
        }
        let outbox = self.outbox()?;
        let cursor = self
            .inner
            .receipt_cursor
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let mut candidates = outbox.relay_reconciliation_candidates_after(
            REMOTE_RECONCILIATION_LIMIT,
            cursor.as_deref(),
        )?;
        if candidates.is_empty() && cursor.is_some() {
            candidates =
                outbox.relay_reconciliation_candidates_after(REMOTE_RECONCILIATION_LIMIT, None)?;
        }
        let mut result = RelayRemoteReconciliationResult {
            examined: u16::try_from(candidates.len()).unwrap_or(REMOTE_RECONCILIATION_LIMIT),
            ..RelayRemoteReconciliationResult::default()
        };
        if candidates.is_empty() {
            return Ok(result);
        }
        let revision = self.inner.credential_revision.load(Ordering::Acquire);
        let profile = self.load_profile()?;
        let client = self.client()?;
        for (index, candidate) in candidates.into_iter().enumerate() {
            // Advance before awaiting: a timeout must not starve later records.
            *self
                .inner
                .receipt_cursor
                .write()
                .unwrap_or_else(|p| p.into_inner()) = Some(candidate.outbox_id.clone());
            let item_started = Instant::now();
            let attempt = u16::try_from(index.saturating_add(1)).unwrap_or(u16::MAX);
            if self.inner.cancellation.is_cancelled() {
                return Err(AppError::new(
                    "RELAY_RECONCILIATION_CANCELLED",
                    "Relay 回执刷新已取消",
                ));
            }
            let cancellation = self.inner.cancellation.child_token();
            let fetched =
                if let Some(publication) = outbox.result_publication(&candidate.outbox_id)? {
                    if publication.destination_identity != destination_identity(&profile) {
                        result.failed = result.failed.saturating_add(1);
                        result.error_code = Some("RELAY_RECONCILIATION_PARTIAL_FAILURE");
                        continue;
                    }
                    client
                        .result_status(
                            &profile.base_url,
                            &profile.device_token,
                            &candidate.outbox_id,
                            &publication.source_hash,
                            &cancellation,
                        )
                        .await
                        .map(RemoteReceipt::Result)
                } else {
                    client
                        .notification_status(
                            &profile.base_url,
                            &profile.device_token,
                            &candidate.notification_id,
                            &cancellation,
                        )
                        .await
                        .map(RemoteReceipt::Short)
                };
            let _commit = self.inner.operation_lock.lock().await;
            if self.inner.credential_revision.load(Ordering::Acquire) != revision {
                return Err(AppError::new(
                    "RELAY_CONFIGURATION_CHANGED",
                    "Relay 配置已改变",
                ));
            }
            let fetched = fetched
                .map_err(|error| self.operational_http_error(&error))
                .and_then(|status| match status {
                    RemoteReceipt::Short(status) => {
                        validate_notification_status(status, &candidate.notification_id)
                            .map(RemoteReceipt::Short)
                    }
                    result => Ok(result),
                });
            let status = match fetched {
                Ok(status) => status,
                Err(error) => {
                    result.failed = result.failed.saturating_add(1);
                    result.error_code = Some("RELAY_RECONCILIATION_PARTIAL_FAILURE");
                    tracing::warn!(
                        backend = "relay",
                        stage = "reconcile_fetch_failed",
                        attempt,
                        duration_ms = bounded_duration_ms(item_started.elapsed()),
                        code = error.code,
                        "notification receipt reconciliation stage"
                    );
                    continue;
                }
            };
            update_status(&self.inner, |current| {
                current.can_read_own = Some(true);
                current.reachable = true;
                current.authenticated = true;
                current.last_success_at = Some(now_ms());
                current.last_checked_at = current.last_success_at;
                current.last_error_code = None;
            });
            let persisted = match status {
                RemoteReceipt::Short(status) => {
                    outbox.apply_relay_remote_status(RelayRemoteStatusUpdate {
                        outbox_id: &candidate.outbox_id,
                        notification_id: &candidate.notification_id,
                        status: status.status.as_str(),
                        updated_at: status.updated_at,
                        provider_message_id: status.provider_message_id.as_deref(),
                    })
                }
                RemoteReceipt::Result(status) => outbox
                    .apply_result_receipt(&candidate.outbox_id, &status, false)
                    .map(|outcome| {
                        matches!(outcome, super::result_store::ResultReceiptApply::Applied)
                    }),
            };
            match persisted {
                Ok(true) => result.updated = result.updated.saturating_add(1),
                Ok(false) => result.unchanged = result.unchanged.saturating_add(1),
                Err(error) => {
                    result.failed = result.failed.saturating_add(1);
                    result.error_code = Some("RELAY_RECONCILIATION_PARTIAL_FAILURE");
                    tracing::warn!(
                        backend = "relay",
                        stage = "reconcile_persist_failed",
                        attempt,
                        duration_ms = bounded_duration_ms(item_started.elapsed()),
                        code = error.code,
                        "notification receipt reconciliation stage"
                    );
                }
            }
        }
        Ok(result)
    }

    pub(crate) fn begin_shutdown(&self) {
        self.inner.cancellation.cancel();
    }

    pub(crate) async fn send_test_notification(
        &self,
        delivery: &NotificationRuntimeState,
    ) -> Result<TestNotificationResult, AppError> {
        delivery.require_notification_not_held(now_ms())?;
        let outbox = self.outbox()?;
        let now = now_ms();
        let dedupe_key = format!("relay-test:{}", uuid::Uuid::new_v4());
        outbox.enqueue_control(
            NotificationEventKind::Test,
            &dedupe_key,
            &relay_test_payload(),
            now,
        )?;
        let _ = delivery.wake()?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(delivery) = outbox.delivery_state(&dedupe_key)? {
                match delivery.status.as_str() {
                    "delivered" => {
                        let receipt = outbox.acceptance_receipt(&dedupe_key)?.ok_or_else(|| {
                            AppError::new("RELAY_RECEIPT_MISSING", "Relay 接管回执缺失")
                        })?;
                        if receipt.acceptance_stage
                            != Some(crate::notification::outbox::TransportAcceptanceStage::Relay)
                        {
                            return Err(AppError::new(
                                "RELAY_RECEIPT_INVALID",
                                "Relay 接管回执无效",
                            ));
                        }
                        return Ok(TestNotificationResult::Relay {
                            status: "relay_accepted",
                            notification_id: receipt.relay_notification_id.ok_or_else(|| {
                                AppError::new("RELAY_RECEIPT_INVALID", "Relay 接管回执无效")
                            })?,
                            accepted_at: receipt.relay_accepted_at.ok_or_else(|| {
                                AppError::new("RELAY_RECEIPT_INVALID", "Relay 接管回执无效")
                            })?,
                        });
                    }
                    "blocked_reconnect" => {
                        return Err(AppError::new(
                            "RELAY_RECONNECT_REQUIRED",
                            "Relay 配置缺失、认证失效或服务器需要重新连接",
                        ));
                    }
                    "dead_letter" | "expired" | "cancelled" => {
                        return Err(relay_terminal_delivery_error(
                            delivery.last_error_code.as_deref(),
                        ));
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(AppError::new(
            "RELAY_SEND_TIMEOUT",
            "等待 Relay 接管测试通知超时",
        ))
    }

    pub(crate) async fn result_link(
        &self,
        outbox_id: &str,
    ) -> Result<super::result_protocol::RelayResultLink, AppError> {
        let publication = self
            .outbox()?
            .result_publication(outbox_id)?
            .ok_or_else(|| AppError::new("RESULT_NOT_PUBLISHED", "此投递尚未创建服务器结果页"))?;
        let profile = self.load_profile()?;
        if publication.destination_identity != destination_identity(&profile) {
            return Err(AppError::new(
                "RESULT_DESTINATION_CHANGED",
                "此结果属于先前的 Relay 设备身份",
            ));
        }
        let revision = self.inner.credential_revision.load(Ordering::Acquire);
        let link = self
            .client()?
            .result_link(
                &profile.base_url,
                &profile.device_token,
                outbox_id,
                &self.inner.cancellation.child_token(),
            )
            .await
            .map_err(|error| relay_http_app_error(&error))?;
        if self.inner.credential_revision.load(Ordering::Acquire) != revision {
            return Err(AppError::new(
                "RELAY_CONFIGURATION_CHANGED",
                "Relay 配置已改变",
            ));
        }
        validate_result_link(&profile.base_url, &link.url)?;
        if link.expires_at != publication.page_expires_at.unwrap_or(link.expires_at) {
            return Err(AppError::new(
                "RESULT_LINK_INVALID",
                "结果链接有效期与已接管事实不一致",
            ));
        }
        Ok(link)
    }

    pub(crate) async fn result_action(
        &self,
        outbox_id: &str,
        action: &str,
        request_id: &str,
    ) -> Result<super::result_protocol::RelayResultReceipt, AppError> {
        let _action_guard = self.inner.result_action_lock.lock().await;
        if action == "resend" {
            self.outbox()?
                .hold_controller()
                .require_not_held(now_ms())?;
        }
        if request_id.is_empty()
            || request_id.len() > 128
            || uuid::Uuid::parse_str(request_id).is_err()
        {
            return Err(AppError::new(
                "INVALID_RESULT_REQUEST",
                "结果操作请求标识无效",
            ));
        }
        let registration_guard = self.inner.operation_lock.lock().await;
        let publication = self
            .outbox()?
            .result_publication(outbox_id)?
            .ok_or_else(|| AppError::new("RESULT_NOT_PUBLISHED", "此投递尚未创建服务器结果页"))?;
        if !matches!(action, "revoke" | "resend") {
            return Err(AppError::new("INVALID_RESULT_ACTION", "结果操作无效"));
        }
        let profile = self.load_profile()?;
        if publication.destination_identity != destination_identity(&profile) {
            return Err(AppError::new(
                "RESULT_DESTINATION_CHANGED",
                "此结果属于先前的 Relay 设备身份",
            ));
        }
        let revision = self.inner.credential_revision.load(Ordering::Acquire);
        let request_id = self
            .outbox()?
            .register_result_action(outbox_id, action, request_id)?;
        drop(registration_guard);
        let send_permit = if action == "resend" {
            Some(
                self.outbox()?
                    .hold_controller()
                    .acquire_send_permit(now_ms())?
                    .ok_or_else(|| {
                        AppError::new("NOTIFICATION_HOLD_ACTIVE", "请先恢复推送，再重新通知")
                    })?,
            )
        } else {
            None
        };
        let client = self.client()?;
        let cancellation = self.inner.cancellation.child_token();
        let request = client.result_action(
            &profile.base_url,
            &profile.device_token,
            outbox_id,
            action,
            &request_id,
            &cancellation,
        );
        let receipt = match send_permit {
            Some(permit) => permit.start_http(request, || false).await.map_err(|_| {
                AppError::new("NOTIFICATION_HOLD_ACTIVE", "请先恢复推送，再重新通知")
            })?,
            None => request.await,
        }
        .map_err(|error| relay_http_app_error(&error))?;
        let _commit_guard = self.inner.operation_lock.lock().await;
        if self.inner.credential_revision.load(Ordering::Acquire) != revision {
            return Err(AppError::new(
                "RELAY_CONFIGURATION_CHANGED",
                "Relay 配置已改变",
            ));
        }
        if !receipt.is_valid_for(outbox_id, &publication.source_hash) {
            return Err(AppError::new(
                "RESULT_RECEIPT_INVALID",
                "Relay 返回的结果回执无效",
            ));
        }
        match self.outbox()?.apply_result_action_receipt(
            outbox_id,
            &receipt,
            action,
            &request_id,
        )? {
            super::result_store::ResultReceiptApply::Applied
            | super::result_store::ResultReceiptApply::AlreadyApplied => {}
            super::result_store::ResultReceiptApply::Stale => {
                return Err(AppError::new(
                    "RESULT_RECEIPT_STALE",
                    "Relay 返回了过期的结果操作回执",
                ))
            }
        }
        Ok(receipt)
    }

    fn client(&self) -> Result<&RelayHttpClient, AppError> {
        self.inner
            .client
            .as_ref()
            .ok_or_else(|| AppError::new("RELAY_CLIENT_UNAVAILABLE", "Relay 网络客户端暂时不可用"))
    }

    fn outbox(&self) -> Result<OutboxRepository, AppError> {
        self.inner
            .outbox
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| AppError::new("RELAY_RUNTIME_UNAVAILABLE", "Relay 投递后台暂时不可用"))
    }

    fn load_profile(&self) -> Result<RelayConnectionProfile, AppError> {
        self.inner
            .store
            .load_profile()
            .map_err(|error| relay_credential_app_error(&error, "RELAY_PROFILE_UNREADABLE"))?
            .ok_or_else(|| AppError::new("RELAY_NOT_CONFIGURED", "尚未配置 Relay 服务器"))
    }

    fn record_local_failure(&self, code: &'static str) {
        let previous_checked_at = self.status().last_checked_at;
        self.replace_status(RelayStatus {
            configured: self.inner.store.is_configured(),
            last_checked_at: previous_checked_at,
            last_error_code: Some(code.into()),
            ..RelayStatus::default()
        });
    }

    pub(crate) fn on_status_changed(&self, callback: Arc<dyn Fn(RelayStatus) + Send + Sync>) {
        *self
            .inner
            .status_changed
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(callback);
    }

    fn replace_status(&self, status: RelayStatus) {
        update_status(&self.inner, |current| {
            let epoch = current.runtime_epoch.clone();
            let revision = current.revision;
            *current = status;
            current.runtime_epoch = epoch;
            current.revision = revision;
        });
    }

    fn operational_http_error(&self, error: &RelayHttpError) -> AppError {
        record_operational_http_failure(&self.inner, error, true);
        relay_http_app_error(error)
    }

    fn record_receipt_sync_result(
        &self,
        result: &Result<RelayRemoteReconciliationResult, AppError>,
    ) {
        self.update_receipt_sync(|diagnostics| match result {
            Ok(summary) => {
                diagnostics.total_examined = diagnostics
                    .total_examined
                    .saturating_add(u64::from(summary.examined));
                diagnostics.total_updated = diagnostics
                    .total_updated
                    .saturating_add(u64::from(summary.updated));
                diagnostics.total_failed = diagnostics
                    .total_failed
                    .saturating_add(u64::from(summary.failed));
                if summary.failed == 0 {
                    diagnostics.last_success_at = Some(now_ms());
                    diagnostics.last_error_code = None;
                } else {
                    diagnostics.last_error_code =
                        Some("RELAY_RECONCILIATION_PARTIAL_FAILURE".into());
                }
            }
            Err(error) => {
                diagnostics.total_failed = diagnostics.total_failed.saturating_add(1);
                diagnostics.last_error_code = Some(error.code.into());
            }
        });
    }

    fn update_receipt_sync(&self, operation: impl FnOnce(&mut RelayReceiptSyncDiagnostics)) {
        let mut diagnostics = self
            .inner
            .receipt_sync
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        operation(&mut diagnostics);
    }
}

fn update_status(inner: &RelayRuntimeInner, update: impl FnOnce(&mut RelayStatus)) {
    let published = {
        let mut status = inner.status.write().unwrap_or_else(|p| p.into_inner());
        update(&mut status);
        status.revision = status.revision.saturating_add(1);
        status.state = availability(&status).into();
        status.clone()
    };
    let callback = inner
        .status_changed
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if let Some(callback) = callback {
        callback(published);
    }
}

fn record_operational_http_failure(
    inner: &RelayRuntimeInner,
    error: &RelayHttpError,
    receipt: bool,
) {
    if matches!(error, RelayHttpError::Cancelled) {
        return;
    }
    update_status(inner, |status| {
        status.last_checked_at = Some(now_ms());
        status.last_error_code = Some(error.code().into());
        match error {
            RelayHttpError::AuthenticationRejected(_) | RelayHttpError::InvalidDeviceToken => {
                status.reachable = true;
                status.authenticated = false;
                status.can_submit = None;
                status.can_read_own = None;
            }
            RelayHttpError::InsufficientScope => {
                status.reachable = true;
                status.authenticated = true;
                if receipt {
                    status.can_read_own = Some(false);
                } else {
                    status.can_submit = Some(false);
                }
            }
            RelayHttpError::Transport(_) | RelayHttpError::Timeout if !receipt => {
                status.reachable = false
            }
            _ => {}
        }
    });
}

fn resume_blocked_delivery(
    outbox: &OutboxRepository,
    previous: &RelayStatus,
    current: &RelayStatus,
    credentials_replaced: bool,
) -> Result<usize, AppError> {
    let credentials_ready = credentials_replaced
        || (!previous.authenticated && current.authenticated && current.scoped_auth_capable);
    let result_pages_ready = current.notification_capable
        && current.authenticated
        && current.scoped_auth_capable
        && current.can_submit != Some(false)
        && current.features.iter().any(|f| f == "result_pages_v1");
    let target_ready = previous.can_submit != Some(true) && current.can_submit == Some(true);
    if !(credentials_ready || result_pages_ready || target_ready) {
        return Ok(0);
    }
    outbox.retry_blocked_for_relay(
        now_ms(),
        credentials_ready,
        result_pages_ready,
        target_ready,
    )
}

fn availability(status: &RelayStatus) -> &'static str {
    if !status.configured {
        return "not_configured";
    }
    match status.last_error_code.as_deref() {
        Some("RELAY_AUTH_FAILED" | "RELAY_TOKEN_INVALID") => "auth_failed",
        Some("RELAY_INSUFFICIENT_SCOPE") => "permission_denied",
        Some("RELAY_SCOPED_AUTH_REQUIRED" | "RELAY_API_UNSUPPORTED") => "capability_missing",
        Some(_) if !status.reachable => "unreachable",
        Some(_) => "stale",
        None if status.last_checked_at.is_none() => "checking",
        None if status.can_submit == Some(false) || status.can_read_own == Some(false) => {
            "permission_denied"
        }
        None if !status.notification_capable || !status.scoped_auth_capable => "capability_missing",
        None if status.reachable && status.authenticated => "ready",
        None => "unreachable",
    }
}

fn bounded_duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn validate_notification_status(
    status: RelayNotificationStatus,
    expected_notification_id: &str,
) -> Result<RelayNotificationStatus, AppError> {
    if status.is_valid_for(expected_notification_id) {
        Ok(status)
    } else {
        Err(AppError::new(
            "RELAY_NOTIFICATION_STATUS_INVALID",
            "Relay 返回了无效的通知状态",
        ))
    }
}

pub(crate) fn relay_test_payload() -> NotificationPayloadV2 {
    NotificationPayloadV2 {
        content_mode: crate::model::ResultContentMode::StatusOnly,
        result_revision: None,
        source_hash: None,
        content_bytes: 0,
        schema_version: NOTIFICATION_PAYLOAD_SCHEMA_VERSION,
        agent_label: String::new(),
        workspace_label: String::new(),
        model_name: None,
        title: "PromptDock 测试通知".to_owned(),
        body: "Relay 通知链路已完成可靠接管。".to_owned(),
        started_at: None,
        completed_at: None,
        duration_ms: None,
        delayed_delivery: false,
    }
}

fn relay_terminal_delivery_error(last_error_code: Option<&str>) -> AppError {
    if last_error_code == Some("RELAY_INSUFFICIENT_SCOPE") {
        AppError::new(
            "RELAY_INSUFFICIENT_SCOPE",
            "Relay 连接有效，但 Device Token 缺少通知写入权限",
        )
    } else {
        AppError::new("RELAY_SEND_FAILED", "Relay 未接管测试通知")
    }
}

fn status_from_probe(
    probe: RelayProbeResult,
    endpoint_hint: Option<String>,
    checked_at: i64,
) -> RelayStatus {
    let notification_capable = probe.supports_notifications;
    let features = probe.server_info.features;
    RelayStatus {
        configured: true,
        reachable: true,
        authenticated: true,
        api_version: Some(probe.server_info.api_version),
        server_version: Some(probe.server_info.server_version),
        notification_capable,
        can_submit: probe.device_status.as_ref().map(|s| s.can_submit),
        can_read_own: probe.device_status.as_ref().map(|s| s.can_read_own),
        scoped_auth_capable: probe.supports_device_scopes,
        features,
        endpoint_hint,
        last_checked_at: Some(checked_at),
        last_success_at: Some(checked_at),
        last_error_code: None,
        ..RelayStatus::default()
    }
}

fn status_from_probe_error(
    error: &RelayHttpError,
    endpoint_hint: Option<String>,
    checked_at: i64,
) -> RelayStatus {
    let reachable = matches!(
        error,
        RelayHttpError::AuthenticationRejected(_)
            | RelayHttpError::InsufficientScope
            | RelayHttpError::NotReady(_)
            | RelayHttpError::HttpStatus { .. }
            | RelayHttpError::InvalidJson(_)
            | RelayHttpError::InvalidResponse(_)
            | RelayHttpError::UnsupportedApiVersion(_)
            | RelayHttpError::ResponseTooLarge
    );
    RelayStatus {
        configured: true,
        reachable,
        authenticated: matches!(error, RelayHttpError::InsufficientScope),
        endpoint_hint,
        last_checked_at: Some(checked_at),
        last_error_code: Some(error.code().into()),
        ..RelayStatus::default()
    }
}

fn relay_http_app_error(error: &RelayHttpError) -> AppError {
    let message = match error {
        RelayHttpError::InvalidEndpoint => "Relay 地址无效或不符合安全要求",
        RelayHttpError::InvalidDeviceToken => "Relay Device Token 格式无效",
        RelayHttpError::AuthenticationRejected(_) => "Relay Device Token 未通过认证",
        RelayHttpError::InsufficientScope => "Relay 连接有效，但 Device Token 权限不足",
        RelayHttpError::TargetUnavailable => "请在 Relay 管理端连接微信账号，全文通知将等待重试",
        RelayHttpError::NotReady(_) => "Relay 服务器尚未就绪",
        RelayHttpError::RetryAfter { .. } => "Relay 暂时限流，正在等待重试",
        RelayHttpError::Timeout => "连接 Relay 服务器超时",
        RelayHttpError::Cancelled => "Relay 连接检测已取消",
        RelayHttpError::UnsupportedApiVersion(_) => "Relay API 版本不受支持",
        RelayHttpError::ResponseTooLarge
        | RelayHttpError::InvalidJson(_)
        | RelayHttpError::InvalidResponse(_) => "Relay 返回了无效响应",
        RelayHttpError::ClientConfiguration => "Relay 网络客户端暂时不可用",
        RelayHttpError::Transport(_) | RelayHttpError::HttpStatus { .. } => "无法连接 Relay 服务器",
    };
    AppError::new(error.code(), message)
}

fn relay_credential_app_error(_error: &RelayCredentialError, code: &'static str) -> AppError {
    let message = match code {
        "RELAY_PROFILE_INVALID" => "Relay 配置格式无效",
        "RELAY_PROFILE_WRITE_FAILED" => "Relay 配置无法安全保存，请稍后重试",
        "RELAY_PROFILE_CLEAR_FAILED" => "Relay 配置暂时无法清除",
        _ => "Relay 配置暂时无法读取",
    };
    AppError::new(code, message)
}

fn profile_error_requires_reconfiguration(error: &RelayCredentialError) -> bool {
    matches!(error, RelayCredentialError::InvalidData(_))
}

fn endpoint_hint(base_url: &str) -> Option<String> {
    let url = Url::parse(base_url).ok()?;
    Some(url.origin().ascii_serialization())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn validate_result_link(base_url: &str, value: &str) -> Result<(), AppError> {
    let base =
        Url::parse(base_url).map_err(|_| AppError::new("RESULT_LINK_INVALID", "Relay 地址无效"))?;
    let link = Url::parse(value)
        .map_err(|_| AppError::new("RESULT_LINK_INVALID", "Relay 返回的结果链接无效"))?;
    if link.scheme() != "https"
        || link.scheme() != base.scheme()
        || link.host_str() != base.host_str()
        || link.port_or_known_default() != base.port_or_known_default()
        || link.query().is_some()
        || link.fragment().is_some()
        || !link.path().starts_with("/r/")
        || link.path().trim_start_matches("/r/").is_empty()
        || link.path().trim_start_matches("/r/").contains('/')
    {
        return Err(AppError::new(
            "RESULT_LINK_INVALID",
            "Relay 返回的结果链接不属于当前安全来源",
        ));
    }
    Ok(())
}

fn destination_identity(profile: &RelayConnectionProfile) -> String {
    // JSON string serialization supplies unambiguous length framing for arbitrary URL text.
    crate::agent::source_hash_sha256(
        &serde_json::to_string(&(&profile.base_url, &profile.device_id))
            .expect("relay profile is serializable"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::db::Db;
    use crate::notification::outbox::{
        DeliveryResult, TransportAcceptance, TransportAcceptanceStage,
    };
    #[cfg(windows)]
    use crate::platform::dpapi::write_encrypted_json;
    use crate::relay::http_client::RelayEndpoint;
    use crate::relay::protocol::{RelayServerInfo, RELAY_API_VERSION};
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_DEVICE_TOKEN: &str =
        "pdv2.123e4567-e89b-12d3-a456-426614174000.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[tokio::test]
    async fn explicit_probe_with_queued_and_blocked_deliveries_only_uses_get_requests() {
        let server = MockServer::start().await;
        mount_probe_response(&server, ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "apiVersion":1,"serverVersion":"fixture","features":["notifications","device_scopes_v1","result_pages_v1"]
        }))).await;
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = OutboxRepository::new(db.clone());
        for key in ["pending-probe", "blocked-probe"] {
            outbox
                .enqueue_control(
                    NotificationEventKind::Test,
                    key,
                    &relay_test_payload(),
                    1000,
                )
                .unwrap();
        }
        db.with_connection(|conn| {
            conn.execute("UPDATE notification_outbox SET status='blocked_reconnect',last_error_code='RELAY_UNAVAILABLE' WHERE dedupe_key='blocked-probe'",[])?;
            Ok(())
        }).unwrap();
        let mut runtime = RelayRuntimeState::start(directory.path());
        Arc::get_mut(&mut runtime.inner).unwrap().client = Some(
            RelayHttpClient::new_for_test(
                Url::parse(&server.uri()).unwrap(),
                Duration::from_secs(2),
            )
            .unwrap(),
        );
        *runtime.inner.outbox.write().unwrap() = Some(outbox.clone());
        runtime
            .inner
            .store
            .save_profile(
                &RelayConnectionProfile::new(server.uri(), TEST_DEVICE_TOKEN.into(), 1).unwrap(),
            )
            .unwrap();
        let before = db.revision();
        let status = runtime.probe_only().await.unwrap();
        assert!(status.authenticated);
        assert_eq!(db.revision(), before);
        db.with_connection(|conn| {
            assert_eq!(
                conn.query_row(
                    "SELECT SUM(attempt_count) FROM notification_outbox",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                conn.query_row(
                    "SELECT status FROM notification_outbox WHERE dedupe_key='blocked-probe'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "blocked_reconnect"
            );
            Ok(())
        })
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert!(!requests.is_empty());
        assert!(requests
            .iter()
            .all(|request| request.method.as_str() == "GET"));
    }

    async fn mount_probe_response(server: &MockServer, server_info: ResponseTemplate) {
        for (path_value, body) in [
            ("/health/live", serde_json::json!({"status":"ok"})),
            ("/health/ready", serde_json::json!({"status":"ready"})),
        ] {
            Mock::given(method("GET"))
                .and(path(path_value))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/v1/server-info"))
            .and(header(
                "authorization",
                format!("Bearer {TEST_DEVICE_TOKEN}"),
            ))
            .respond_with(server_info)
            .mount(server)
            .await;
    }

    fn accepted_relay_candidate(
        outbox: &OutboxRepository,
        key: &str,
        notification_id: &str,
        at: i64,
    ) -> crate::notification::outbox::RelayReconciliationCandidate {
        outbox
            .enqueue_control(NotificationEventKind::Test, key, &relay_test_payload(), at)
            .unwrap();
        let claim = outbox
            .claim_next_for(NotificationBackendKind::Relay, at)
            .unwrap()
            .unwrap();
        outbox
            .complete_delivery(
                &claim,
                DeliveryResult::Accepted(TransportAcceptance {
                    stage: TransportAcceptanceStage::Relay,
                    transport_message_id: Some(notification_id.into()),
                    provider_message_id: None,
                    accepted_at: Some(at + 1),
                }),
                at + 1,
            )
            .unwrap();
        crate::notification::outbox::RelayReconciliationCandidate {
            outbox_id: claim.id,
            notification_id: notification_id.into(),
        }
    }

    fn seeded_result_action(
        server: &MockServer,
        request_timeout: Duration,
    ) -> (
        tempfile::TempDir,
        RelayRuntimeState,
        OutboxRepository,
        String,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = RelayRuntimeState::start(directory.path());
        Arc::get_mut(&mut runtime.inner).unwrap().client = Some(
            RelayHttpClient::new_for_test(Url::parse(&server.uri()).unwrap(), request_timeout)
                .unwrap(),
        );
        let profile =
            RelayConnectionProfile::new(server.uri(), TEST_DEVICE_TOKEN.into(), 1).unwrap();
        runtime.inner.store.save_profile(&profile).unwrap();
        let outbox = OutboxRepository::new(Arc::new(Db::open_in_memory().unwrap()));
        *runtime.inner.outbox.write().unwrap() = Some(outbox.clone());
        let candidate = accepted_relay_candidate(&outbox, "result-action", "old-notice", 100);
        let source_hash = "a".repeat(64);
        outbox
            .register_result(&crate::relay::result_store::ResultPublicationRecord {
                outbox_id: candidate.outbox_id.clone(),
                source_hash: source_hash.clone(),
                result_revision: 1,
                destination_identity: destination_identity(&profile),
                request_digest: "b".repeat(64),
                accepted_at: None,
                page_state: None,
                page_expires_at: None,
                notification_id: None,
                notification_status: None,
                updated_at: None,
            })
            .unwrap();
        outbox
            .apply_result_receipt(
                &candidate.outbox_id,
                &crate::relay::result_protocol::RelayResultReceipt {
                    schema_version: 1,
                    result_id: candidate.outbox_id.clone(),
                    source_hash,
                    accepted_at: 100,
                    updated_at: 100,
                    page_state: crate::relay::result_protocol::RelayResultPageState::Available,
                    page_expires_at: 10_000,
                    notification_id: "old-notice".into(),
                    notification_status: "provider_accepted".into(),
                },
                false,
            )
            .unwrap();
        (directory, runtime, outbox, candidate.outbox_id)
    }

    #[tokio::test]
    async fn result_action_lost_response_reuses_durable_request_id_and_polls_new_notice() {
        let server = MockServer::start().await;
        let (_directory, runtime, outbox, outbox_id) =
            seeded_result_action(&server, Duration::from_millis(25));
        let first_request_id = "11111111-1111-4111-8111-111111111111";
        let retry_request_id = "22222222-2222-4222-8222-222222222222";
        let action_path = format!("/v1/results/{outbox_id}/resend");
        Mock::given(method("POST"))
            .and(path(action_path.as_str()))
            .and(body_json(
                serde_json::json!({"requestId": first_request_id}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(100))
                    .set_body_json(serde_json::json!({"discarded":"late response"})),
            )
            .mount(&server)
            .await;

        let error = runtime
            .result_action(&outbox_id, "resend", first_request_id)
            .await
            .unwrap_err();
        assert_eq!(error.code, "RELAY_TIMEOUT");
        tokio::time::sleep(Duration::from_millis(120)).await;
        server.reset().await;

        Mock::given(method("POST"))
            .and(path(action_path.as_str()))
            .and(body_json(
                serde_json::json!({"requestId": first_request_id}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "schemaVersion": 1,
                "resultId": outbox_id,
                "sourceHash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "acceptedAt": 100,
                "updatedAt": 101,
                "pageState": "available",
                "pageExpiresAt": 10000,
                "notificationId": "new-notice",
                "notificationStatus": "pending_channel"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/results/{outbox_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "schemaVersion": 1,
                "resultId": outbox_id,
                "sourceHash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "acceptedAt": 100,
                "updatedAt": 102,
                "pageState": "available",
                "pageExpiresAt": 10000,
                "notificationId": "new-notice",
                "notificationStatus": "provider_accepted"
            })))
            .mount(&server)
            .await;

        let receipt = runtime
            .result_action(&outbox_id, "resend", retry_request_id)
            .await
            .unwrap();
        assert_eq!(receipt.notification_id, "new-notice");
        let reconciled = runtime
            .refresh_remote_notification_statuses()
            .await
            .unwrap();
        assert_eq!(reconciled.updated, 1);
        let record = outbox.result_publication(&outbox_id).unwrap().unwrap();
        assert_eq!(record.notification_id.as_deref(), Some("new-notice"));
        assert_eq!(
            record.notification_status.as_deref(),
            Some("provider_accepted")
        );
    }

    #[tokio::test]
    async fn same_device_token_rotation_preserves_pending_and_history() {
        let server = MockServer::start().await;
        mount_probe_response(&server, ResponseTemplate::new(200).set_body_json(serde_json::json!({"apiVersion":1,"serverVersion":"fixture","features":["notifications","device_scopes_v1"],"wechatProtocolReference":"fixture"}))).await;
        let dir = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(dir.path());
        use base64::Engine as _;
        let old_token = format!(
            "pdv2.123e4567-e89b-12d3-a456-426614174000.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([1u8; 32])
        );
        runtime
            .inner
            .store
            .save_profile(
                &RelayConnectionProfile::new(format!("{}/", server.uri()), old_token, 1).unwrap(),
            )
            .unwrap();
        let outbox = OutboxRepository::new(Arc::new(Db::open_in_memory().unwrap()));
        *runtime.inner.outbox.write().unwrap() = Some(outbox.clone());
        let accepted = accepted_relay_candidate(&outbox, "history", "history-id", 100);
        outbox
            .enqueue_control(
                NotificationEventKind::Test,
                "pending",
                &relay_test_payload(),
                now_ms(),
            )
            .unwrap();
        runtime
            .configure(server.uri(), Zeroizing::new(TEST_DEVICE_TOKEN.into()))
            .await
            .unwrap();
        assert_eq!(
            runtime
                .inner
                .store
                .load_profile()
                .unwrap()
                .unwrap()
                .device_token,
            TEST_DEVICE_TOKEN
        );
        let history = outbox.history_page(10, None).unwrap();
        assert_eq!(history.items.len(), 2);
        assert!(history
            .items
            .iter()
            .any(|item| item.id == accepted.outbox_id && item.status == "delivered"));
    }

    #[test]
    fn receipt_scope_and_network_failures_do_not_erase_submit_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(dir.path());
        runtime.replace_status(RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            can_submit: Some(true),
            ..RelayStatus::default()
        });
        runtime.operational_http_error(&RelayHttpError::InsufficientScope);
        assert_eq!(runtime.status().can_read_own, Some(false));
        assert_eq!(runtime.status().can_submit, Some(true));
        runtime.operational_http_error(&RelayHttpError::Timeout);
        assert!(runtime.status().reachable);
        assert_eq!(runtime.status().can_submit, Some(true));
    }

    #[test]
    fn published_status_is_versioned_and_callback_can_read_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(dir.path());
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = observed.clone();
        let reader = runtime.clone();
        runtime.on_status_changed(Arc::new(move |status| {
            assert_eq!(reader.status(), status);
            capture.lock().unwrap().push(status);
        }));
        runtime.replace_status(RelayStatus {
            configured: true,
            ..RelayStatus::default()
        });
        runtime.replace_status(RelayStatus {
            configured: true,
            last_error_code: Some("RELAY_AUTH_FAILED".into()),
            ..RelayStatus::default()
        });
        let values = observed.lock().unwrap();
        assert_eq!(values[0].state, "checking");
        assert_eq!(values[1].state, "auth_failed");
        assert_eq!(values[1].revision, values[0].revision + 1);
        assert!(!values[0].runtime_epoch.is_empty());
        assert_eq!(values[0].runtime_epoch, values[1].runtime_epoch);
        assert_eq!(values[0].can_submit, None);
    }

    #[tokio::test]
    async fn slow_receipts_are_bounded_local_history_is_independent_and_cursor_advances() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(dir.path());
        runtime
            .inner
            .store
            .save_profile(
                &RelayConnectionProfile::new(server.uri(), TEST_DEVICE_TOKEN.into(), 1).unwrap(),
            )
            .unwrap();
        let outbox = OutboxRepository::new(Arc::new(Db::open_in_memory().unwrap()));
        *runtime.inner.outbox.write().unwrap() = Some(outbox.clone());
        for index in 0..25 {
            accepted_relay_candidate(
                &outbox,
                &format!("item-{index}"),
                &format!("notification-{index}"),
                100 + index * 10,
            );
        }
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
            .mount(&server)
            .await;
        let first = outbox
            .relay_reconciliation_candidates_after(1, None)
            .unwrap()
            .remove(0);
        let started = Instant::now();
        let refresh = runtime.refresh_remote_notification_statuses();
        let local = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let local_start = Instant::now();
            assert_eq!(outbox.history_page(50, None).unwrap().items.len(), 25);
            assert!(local_start.elapsed() < Duration::from_millis(200));
            assert!(runtime.inner.operation_lock.try_lock().is_ok());
        };
        let (result, ()) = tokio::join!(refresh, local);
        assert_eq!(result.unwrap_err().code, "RELAY_RECEIPT_BUDGET");
        assert!(started.elapsed() < Duration::from_secs(7));
        assert_eq!(
            runtime.inner.receipt_cursor.read().unwrap().as_deref(),
            Some(first.outbox_id.as_str())
        );
        let later = outbox
            .relay_reconciliation_candidates_after(25, Some(&first.outbox_id))
            .unwrap();
        assert_eq!(later.len(), 24);
    }

    #[tokio::test]
    async fn remote_reconciliation_is_fail_soft_per_item_and_preserves_old_values() {
        let server = MockServer::start().await;
        let directory = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(directory.path());
        runtime
            .inner
            .store
            .save_profile(
                &RelayConnectionProfile::new(server.uri(), TEST_DEVICE_TOKEN.into(), 1).unwrap(),
            )
            .unwrap();
        let outbox = OutboxRepository::new(Arc::new(Db::open_in_memory().unwrap()));
        *runtime
            .inner
            .outbox
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(outbox.clone());
        let good = accepted_relay_candidate(&outbox, "good", "notification-good", 100);
        let bad = accepted_relay_candidate(&outbox, "bad", "notification-bad", 110);
        assert!(outbox
            .apply_relay_remote_status(RelayRemoteStatusUpdate {
                outbox_id: &bad.outbox_id,
                notification_id: &bad.notification_id,
                status: "pending_channel",
                updated_at: 120,
                provider_message_id: None,
            })
            .unwrap());

        Mock::given(method("GET"))
            .and(path("/v1/notifications/notification-good"))
            .and(header(
                "authorization",
                format!("Bearer {TEST_DEVICE_TOKEN}"),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "notificationId": "notification-good",
                "status": "provider_accepted",
                "attemptCount": 1,
                "lastErrorCode": null,
                "providerMessageId": "provider-good",
                "updatedAt": 130,
                "providerAcceptedAt": 130
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/notifications/notification-bad"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "notificationId": "wrong-id",
                "status": "provider_accepted",
                "attemptCount": 1,
                "lastErrorCode": null,
                "providerMessageId": "must-not-persist",
                "updatedAt": 140,
                "providerAcceptedAt": 140
            })))
            .mount(&server)
            .await;

        let result = runtime
            .refresh_remote_notification_statuses()
            .await
            .unwrap();
        assert_eq!(result.examined, 2);
        assert_eq!(result.updated, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(
            result.error_code,
            Some("RELAY_RECONCILIATION_PARTIAL_FAILURE")
        );
        let diagnostics = runtime
            .inner
            .receipt_sync
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert!(diagnostics.last_attempt_at.is_some());
        assert!(diagnostics.last_success_at.is_none());
        assert_eq!(
            diagnostics.last_error_code.as_deref(),
            Some("RELAY_RECONCILIATION_PARTIAL_FAILURE")
        );
        assert_eq!(diagnostics.total_examined, 2);
        assert_eq!(diagnostics.total_updated, 1);
        assert_eq!(diagnostics.total_failed, 1);
        let items = outbox.history_page(10, None).unwrap().items;
        let good_item = items.iter().find(|item| item.id == good.outbox_id).unwrap();
        assert_eq!(
            good_item.remote_status.as_deref(),
            Some("provider_accepted")
        );
        assert_eq!(
            good_item.remote_provider_message_id.as_deref(),
            Some("provider-good")
        );
        let bad_item = items.iter().find(|item| item.id == bad.outbox_id).unwrap();
        assert_eq!(bad_item.remote_status.as_deref(), Some("pending_channel"));
        assert_eq!(bad_item.remote_updated_at, Some(120));
        assert!(bad_item.remote_provider_message_id.is_none());
    }

    #[tokio::test]
    async fn shutdown_cancels_receipt_refresh_and_records_only_safe_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(directory.path());
        runtime.begin_shutdown();

        let error = runtime
            .refresh_remote_notification_statuses()
            .await
            .unwrap_err();
        assert_eq!(error.code, "RELAY_RECONCILIATION_CANCELLED");
        let diagnostics = runtime
            .inner
            .receipt_sync
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert!(diagnostics.last_attempt_at.is_some());
        assert!(diagnostics.last_success_at.is_none());
        assert_eq!(
            diagnostics.last_error_code.as_deref(),
            Some("RELAY_RECONCILIATION_CANCELLED")
        );
        assert_eq!(diagnostics.total_examined, 0);
        assert_eq!(diagnostics.total_updated, 0);
        assert_eq!(diagnostics.total_failed, 1);
        let serialized = serde_json::to_string(&diagnostics).unwrap();
        for forbidden in ["notificationId", "providerMessageId", "endpoint", "token"] {
            assert!(!serialized.contains(forbidden));
        }
    }

    #[test]
    fn relay_status_is_safe_and_capabilities_come_only_from_features() {
        let status = status_from_probe(
            RelayProbeResult::from_server_info(RelayServerInfo {
                api_version: RELAY_API_VERSION,
                server_version: "3.0.0".into(),
                features: vec![
                    "device_scopes_v1".into(),
                    "notifications".into(),
                    "future".into(),
                ],
            }),
            Some("https://relay.example.test".into()),
            123,
        );
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["configured"], true);
        assert_eq!(json["notificationCapable"], true);
        assert_eq!(json["scopedAuthCapable"], true);
        assert!(json.get("remoteGatewayCapable").is_none());
        assert!(json.get("cloudWechatStatusReadable").is_none());
        assert!(json.get("deviceToken").is_none());
        assert!(json.get("deviceId").is_none());
    }

    #[test]
    fn endpoint_hint_never_contains_path_query_or_credentials() {
        assert_eq!(
            endpoint_hint("https://relay.example.test:8443/private?token=secret"),
            Some("https://relay.example.test:8443".into())
        );
    }

    #[test]
    fn invalid_server_info_never_claims_authentication() {
        let status = status_from_probe_error(
            &RelayHttpError::InvalidJson(RelayEndpoint::ServerInfo),
            Some("https://relay.example.test".into()),
            123,
        );
        assert!(status.reachable);
        assert!(!status.authenticated);
        assert_eq!(status.last_checked_at, Some(123));
    }

    #[test]
    fn invalid_profile_data_requires_reconfiguration() {
        assert!(profile_error_requires_reconfiguration(
            &RelayCredentialError::InvalidData("device_token")
        ));
    }

    #[test]
    fn insufficient_scope_preserves_authenticated_reachable_status() {
        let status = status_from_probe_error(
            &RelayHttpError::InsufficientScope,
            Some("https://relay.example.test".into()),
            123,
        );
        assert!(status.reachable);
        assert!(status.authenticated);
        assert_eq!(
            status.last_error_code.as_deref(),
            Some("RELAY_INSUFFICIENT_SCOPE")
        );
    }

    #[test]
    fn test_notification_preserves_scope_failure_code() {
        assert_eq!(
            relay_terminal_delivery_error(Some("RELAY_INSUFFICIENT_SCOPE")).code,
            "RELAY_INSUFFICIENT_SCOPE"
        );
        assert_eq!(
            relay_terminal_delivery_error(Some("IDEMPOTENCY_CONFLICT")).code,
            "RELAY_SEND_FAILED"
        );
    }

    #[tokio::test]
    async fn configure_rejects_missing_scoped_auth_feature_without_saving_candidate() {
        let server = MockServer::start().await;
        let directory = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(directory.path());
        let initial = runtime.status();
        mount_probe_response(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "apiVersion": 1,
                "serverVersion": "test",
                "features": ["notifications", "wechat_login", "wechat_channel"],
                "wechatProtocolReference": "2.4.6"
            })),
        )
        .await;

        let error = runtime
            .configure(server.uri(), Zeroizing::new(TEST_DEVICE_TOKEN.into()))
            .await
            .unwrap_err();

        assert_eq!(error.code, "RELAY_SCOPED_AUTH_REQUIRED");
        assert!(!runtime.inner.store.is_configured());
        assert_eq!(runtime.status(), initial);

        runtime
            .inner
            .store
            .save_profile(
                &RelayConnectionProfile::new(server.uri(), TEST_DEVICE_TOKEN.into(), 1).unwrap(),
            )
            .unwrap();
        let error = runtime.test_connection().await.unwrap_err();
        assert_eq!(error.code, "RELAY_SCOPED_AUTH_REQUIRED");
        let status = runtime.status();
        assert!(status.configured);
        assert!(status.authenticated);
        assert!(!status.scoped_auth_capable);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn rejected_authenticated_candidates_preserve_verified_saved_profile_and_status() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(directory.path());
        let saved = RelayConnectionProfile::new(
            "https://saved.example.test/".into(),
            TEST_DEVICE_TOKEN.into(),
            1,
        )
        .unwrap();
        runtime.inner.store.save_profile(&saved).unwrap();
        let verified = RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            api_version: Some(1),
            server_version: Some("saved".into()),
            features: vec!["device_scopes_v1".into(), "notifications".into()],
            notification_capable: true,
            scoped_auth_capable: true,
            endpoint_hint: Some("https://saved.example.test".into()),
            last_checked_at: Some(100),
            ..RelayStatus::default()
        };
        runtime.replace_status(verified.clone());
        let verified = runtime.status();

        for (response, code) in [
            (ResponseTemplate::new(401), "RELAY_AUTH_FAILED"),
            (
                ResponseTemplate::new(403).set_body_json(serde_json::json!({
                    "error": {"code": "INSUFFICIENT_SCOPE", "message": "secret"}
                })),
                "RELAY_INSUFFICIENT_SCOPE",
            ),
            (
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "apiVersion": 1,
                    "serverVersion": "candidate",
                    "features": ["notifications"],
                    "wechatProtocolReference": "2.4.6"
                })),
                "RELAY_SCOPED_AUTH_REQUIRED",
            ),
        ] {
            let server = MockServer::start().await;
            mount_probe_response(&server, response).await;

            let error = runtime
                .configure(server.uri(), Zeroizing::new(TEST_DEVICE_TOKEN.into()))
                .await
                .unwrap_err();

            assert_eq!(error.code, code);
            assert_eq!(runtime.status(), verified);
            assert_eq!(
                runtime
                    .inner
                    .store
                    .load_profile()
                    .unwrap()
                    .unwrap()
                    .base_url,
                saved.base_url
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn legacy_profile_and_failed_candidate_remain_reconfigurable() {
        let directory = tempfile::tempdir().unwrap();
        let mut legacy = RelayConnectionProfile::new(
            "https://legacy.example.test/".into(),
            TEST_DEVICE_TOKEN.into(),
            1,
        )
        .unwrap();
        legacy.device_token = legacy.device_token.replacen("pdv2.", "pdv1.", 1);
        write_encrypted_json(
            &directory.path().join("relay-connection.dpapi"),
            &legacy,
            "TEST_WRITE_FAILED",
            "test write failed",
        )
        .unwrap();
        let runtime = RelayRuntimeState::start(directory.path());
        let reconfigurable = runtime.status();
        assert!(!reconfigurable.configured);

        let server = MockServer::start().await;
        mount_probe_response(&server, ResponseTemplate::new(401)).await;
        let error = runtime
            .configure(server.uri(), Zeroizing::new(TEST_DEVICE_TOKEN.into()))
            .await
            .unwrap_err();

        assert_eq!(error.code, "RELAY_AUTH_FAILED");
        assert_eq!(runtime.status(), reconfigurable);
        assert!(matches!(
            runtime.inner.store.load_profile(),
            Err(RelayCredentialError::InvalidData("device_token"))
        ));
    }

    #[test]
    fn scoped_auth_capability_gates_only_notification_delivery() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(directory.path());
        runtime.replace_status(RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            notification_capable: true,
            scoped_auth_capable: false,
            features: vec!["notifications".into()],
            ..RelayStatus::default()
        });

        assert!(!runtime.notification_delivery_ready());
    }

    #[test]
    fn rejected_candidate_does_not_pollute_the_saved_profile_status() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = RelayRuntimeState::start(directory.path());
        let existing = RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            api_version: Some(1),
            server_version: Some("existing".into()),
            endpoint_hint: Some("https://existing.example.test".into()),
            last_checked_at: Some(100),
            ..RelayStatus::default()
        };
        runtime.replace_status(existing.clone());
        let existing = runtime.status();

        let error = tauri::async_runtime::block_on(runtime.configure(
            " https://candidate.example.test".into(),
            Zeroizing::new(TEST_DEVICE_TOKEN.into()),
        ))
        .unwrap_err();

        assert_eq!(error.code, "RELAY_ENDPOINT_INVALID");
        assert_eq!(runtime.status(), existing);
    }

    #[cfg(windows)]
    #[test]
    fn corrupt_profile_is_fail_soft_and_never_triggers_network_at_startup() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("relay-connection.dpapi"), b"corrupt").unwrap();

        let runtime = RelayRuntimeState::start(directory.path());
        let status = runtime.status();
        assert!(status.configured);
        assert!(!status.reachable);
        assert!(!status.authenticated);
        assert_eq!(
            status.last_error_code.as_deref(),
            Some("RELAY_PROFILE_UNREADABLE")
        );
    }

    fn blocked_result_pages_fixture(
        outbox: &OutboxRepository,
        dedupe_key: &str,
        created_at: i64,
        expires_at: i64,
    ) {
        outbox
            .enqueue_control(
                NotificationEventKind::Test,
                dedupe_key,
                &relay_test_payload(),
                created_at,
            )
            .unwrap();
        outbox
            .db()
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE notification_outbox
                     SET status='blocked_reconnect',
                         last_error_code='RELAY_RESULT_PAGES_UNAVAILABLE',
                         expires_at=?,
                         updated_at=?
                     WHERE dedupe_key=?",
                    rusqlite::params![expires_at, created_at, dedupe_key],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn count_blocked_for_result_pages(outbox: &OutboxRepository) -> i64 {
        outbox
            .db()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM notification_outbox
                     WHERE status='blocked_reconnect'
                       AND last_error_code='RELAY_RESULT_PAGES_UNAVAILABLE'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap()
    }

    fn count_retry_wait(outbox: &OutboxRepository) -> i64 {
        outbox
            .db()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM notification_outbox WHERE status='retry_wait'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap()
    }

    #[test]
    fn t01_result_pages_later_enabled_recovers_blocked_rows() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = OutboxRepository::new(db);
        let future_expires = now_ms() + 3_600_000;
        blocked_result_pages_fixture(&outbox, "rp-t01", 1000, future_expires);
        let previous = RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            notification_capable: true,
            scoped_auth_capable: true,
            can_submit: Some(true),
            features: vec!["notifications".into(), "device_scopes_v1".into()],
            ..RelayStatus::default()
        };
        let current = RelayStatus {
            features: vec![
                "notifications".into(),
                "device_scopes_v1".into(),
                "result_pages_v1".into(),
            ],
            ..previous.clone()
        };
        let recovered = resume_blocked_delivery(&outbox, &previous, &current, false).unwrap();
        assert_eq!(recovered, 1);
        assert_eq!(count_blocked_for_result_pages(&outbox), 0);
        assert_eq!(count_retry_wait(&outbox), 1);
    }

    #[test]
    fn t02_result_pages_still_absent_does_not_recover() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = OutboxRepository::new(db);
        blocked_result_pages_fixture(&outbox, "rp-t02", 1000, 1_000_000);
        let previous = RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            notification_capable: true,
            scoped_auth_capable: true,
            can_submit: Some(true),
            features: vec!["notifications".into(), "device_scopes_v1".into()],
            ..RelayStatus::default()
        };
        let current = previous.clone();
        let recovered = resume_blocked_delivery(&outbox, &previous, &current, false).unwrap();
        assert_eq!(recovered, 0);
        assert_eq!(count_blocked_for_result_pages(&outbox), 1);
    }

    #[test]
    fn t03_idempotent_recovery_when_both_prev_and_current_have_feature() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = OutboxRepository::new(db);
        let future_expires = now_ms() + 3_600_000;
        blocked_result_pages_fixture(&outbox, "rp-t03-a", 1000, future_expires);
        blocked_result_pages_fixture(&outbox, "rp-t03-b", 1000, future_expires);
        let capable = RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            notification_capable: true,
            scoped_auth_capable: true,
            can_submit: Some(true),
            features: vec![
                "notifications".into(),
                "device_scopes_v1".into(),
                "result_pages_v1".into(),
            ],
            ..RelayStatus::default()
        };
        let first = resume_blocked_delivery(&outbox, &capable, &capable, false).unwrap();
        assert_eq!(first, 2);
        assert_eq!(count_blocked_for_result_pages(&outbox), 0);
        let second = resume_blocked_delivery(&outbox, &capable, &capable, false).unwrap();
        assert_eq!(second, 0);
    }

    #[test]
    fn t06_repeated_probes_converge_to_zero_recoveries() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = OutboxRepository::new(db);
        let future_expires = now_ms() + 3_600_000;
        blocked_result_pages_fixture(&outbox, "rp-t06", 1000, future_expires);
        let capable = RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            notification_capable: true,
            scoped_auth_capable: true,
            can_submit: Some(true),
            features: vec![
                "notifications".into(),
                "device_scopes_v1".into(),
                "result_pages_v1".into(),
            ],
            ..RelayStatus::default()
        };
        let mut total = 0;
        for _ in 0..5 {
            total += resume_blocked_delivery(&outbox, &capable, &capable, false).unwrap();
        }
        assert_eq!(total, 1);
        let status = outbox
            .db()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT dedupe_key, status FROM notification_outbox WHERE dedupe_key='rp-t06'",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(status.0, "rp-t06");
        assert_eq!(status.1, "retry_wait");
    }

    #[test]
    fn t07_expired_records_are_not_revived() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = OutboxRepository::new(db);
        blocked_result_pages_fixture(&outbox, "rp-t07-expired", 1000, 5000);
        blocked_result_pages_fixture(&outbox, "rp-t07-alive", 1000, 1_000_000);
        let now = 10_000i64;
        let recovered = outbox
            .retry_blocked_for_relay(now, false, true, false)
            .unwrap();
        assert_eq!(recovered, 1);
        let expired = outbox
            .db()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT status, last_error_code FROM notification_outbox
                     WHERE dedupe_key='rp-t07-expired'",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(expired.0, "expired");
        assert_eq!(expired.1, "NOTIFICATION_EXPIRED");
        let alive = outbox
            .db()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT status FROM notification_outbox WHERE dedupe_key='rp-t07-alive'",
                    [],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .unwrap();
        assert_eq!(alive, "retry_wait");
    }

    #[test]
    fn t08_stale_credentials_do_not_unlock_result_pages_blocks() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = OutboxRepository::new(db);
        blocked_result_pages_fixture(&outbox, "rp-t08", 1000, 1_000_000);
        let previous = RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            notification_capable: true,
            scoped_auth_capable: true,
            can_submit: Some(true),
            features: vec![
                "notifications".into(),
                "device_scopes_v1".into(),
                "result_pages_v1".into(),
            ],
            ..RelayStatus::default()
        };
        let unauthenticated = RelayStatus {
            authenticated: false,
            ..previous.clone()
        };
        let recovered =
            resume_blocked_delivery(&outbox, &previous, &unauthenticated, false).unwrap();
        assert_eq!(recovered, 0);
        assert_eq!(count_blocked_for_result_pages(&outbox), 1);
        let no_submit = RelayStatus {
            can_submit: Some(false),
            ..previous.clone()
        };
        let recovered = resume_blocked_delivery(&outbox, &previous, &no_submit, false).unwrap();
        assert_eq!(recovered, 0);
        assert_eq!(count_blocked_for_result_pages(&outbox), 1);
    }

    #[test]
    fn t09_mixed_blocked_causes_only_recover_matching_rows() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = OutboxRepository::new(db);
        let future_expires = now_ms() + 3_600_000;
        blocked_result_pages_fixture(&outbox, "rp-t09-result", 1000, future_expires);
        outbox
            .enqueue_control(
                NotificationEventKind::Test,
                "rp-t09-creds",
                &relay_test_payload(),
                1000,
            )
            .unwrap();
        outbox
            .db()
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE notification_outbox
                     SET status='blocked_reconnect',
                         last_error_code='RELAY_CREDENTIALS_REQUIRED',
                         expires_at=?
                     WHERE dedupe_key='rp-t09-creds'",
                    rusqlite::params![future_expires],
                )?;
                Ok(())
            })
            .unwrap();
        let previous = RelayStatus {
            configured: true,
            reachable: true,
            authenticated: true,
            notification_capable: true,
            scoped_auth_capable: true,
            can_submit: Some(true),
            features: vec!["notifications".into(), "device_scopes_v1".into()],
            ..RelayStatus::default()
        };
        let current = RelayStatus {
            features: vec![
                "notifications".into(),
                "device_scopes_v1".into(),
                "result_pages_v1".into(),
            ],
            ..previous.clone()
        };
        let recovered = resume_blocked_delivery(&outbox, &previous, &current, false).unwrap();
        assert_eq!(recovered, 1);
        assert_eq!(count_blocked_for_result_pages(&outbox), 0);
        let creds = outbox
            .db()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT status, last_error_code FROM notification_outbox
                     WHERE dedupe_key='rp-t09-creds'",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(creds.0, "blocked_reconnect");
        assert_eq!(creds.1, "RELAY_CREDENTIALS_REQUIRED");
    }
}
