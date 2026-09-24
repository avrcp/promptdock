use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::model::NotificationBackendKind;
use crate::notification::hold::{HttpStartError, SendPermit};
use crate::notification::outbox::{
    DeliveryCancellation, DeliveryResult, DeliveryTransport, OutboxClaim, TransportAcceptance,
    TransportAcceptanceStage, TransportBlock, TransportBlockKind, TransportFailure, TransportRetry,
    TransportRetryPolicy,
};

use super::credentials::{RelayConnectionProfile, RelayCredentialStore};
use super::http_client::{RelayEndpoint, RelayHttpClient, RelayHttpError};
use super::protocol::RelayNotificationV1;
use super::result_protocol::RelayResultPublication;

type HttpResultObserver =
    Arc<dyn Fn(&RelayConnectionProfile, Option<&RelayHttpError>) + Send + Sync>;

pub(crate) struct RelaySender {
    client: RelayHttpClient,
    store: Arc<RelayCredentialStore>,
    runtime: Handle,
    on_http_result: HttpResultObserver,
    result_pages_capable: Arc<dyn Fn() -> bool + Send + Sync>,
    outbox: crate::notification::outbox::OutboxRepository,
}

impl RelaySender {
    pub(crate) fn new(
        client: RelayHttpClient,
        store: Arc<RelayCredentialStore>,
        runtime: Handle,
        on_http_result: HttpResultObserver,
        result_pages_capable: Arc<dyn Fn() -> bool + Send + Sync>,
        outbox: crate::notification::outbox::OutboxRepository,
    ) -> Self {
        Self {
            client,
            store,
            runtime,
            on_http_result,
            result_pages_capable,
            outbox,
        }
    }

    async fn deliver_async(
        &self,
        claim: &OutboxClaim,
        cancellation: &DeliveryCancellation,
    ) -> DeliveryResult {
        if claim.delivery_backend != NotificationBackendKind::Relay {
            return DeliveryResult::PolicyRevoked;
        }
        let profile = match self.store.load_profile() {
            Ok(Some(profile)) => profile,
            Ok(None) | Err(_) => return relay_credentials_block(),
        };
        if claim.payload.content_mode == crate::model::ResultContentMode::FullFinal
            && claim.payload.source_hash.is_some()
        {
            if !(self.result_pages_capable)() {
                return DeliveryResult::Blocked(TransportBlock {
                    kind: TransportBlockKind::Credentials,
                    error_code: "RELAY_RESULT_PAGES_UNAVAILABLE",
                    error_message: "Relay has not enabled result pages",
                });
            }
            let (Some(run_key), Some(revision), Some(source_hash)) = (
                claim.agent_run_key.as_ref(),
                claim.payload.result_revision,
                claim.payload.source_hash.as_ref(),
            ) else {
                return permanent_failure(
                    "RESULT_METADATA_INVALID",
                    "Full-final metadata is unavailable",
                );
            };
            let Ok(revision) = u64::try_from(revision) else {
                return permanent_failure(
                    "RESULT_METADATA_INVALID",
                    "Full-final revision is invalid",
                );
            };
            let result_page = RelayResultPublication {
                schema_version: 1,
                result_id: claim.id.clone(),
                dedupe_key: claim.dedupe_key.clone(),
                kind: "run_completed".into(),
                content_mode: "full_final".into(),
                source: "codex_stop".into(),
                correlation_key: run_key.clone(),
                result_revision: revision.to_string(),
                title: claim.payload.title.clone(),
                body: claim.payload.body.clone(),
                source_hash: source_hash.clone(),
                created_at: claim.created_at.max(0),
                notification_expires_at: claim.expires_at,
                started_at: claim.payload.started_at,
                completed_at: claim.payload.completed_at,
                duration_ms: claim.payload.duration_ms,
            };
            let request_digest = match serde_json::to_string(&result_page) {
                Ok(value) => crate::agent::source_hash_sha256(&value),
                Err(_) => {
                    return permanent_failure(
                        "RESULT_METADATA_INVALID",
                        "Full-final request cannot be encoded",
                    )
                }
            };
            let destination_identity =
                match serde_json::to_string(&(&profile.base_url, &profile.device_id)) {
                    Ok(value) => crate::agent::source_hash_sha256(&value),
                    Err(_) => {
                        return permanent_failure(
                            "RESULT_METADATA_INVALID",
                            "Relay destination cannot be encoded",
                        )
                    }
                };
            if let Err(error) =
                self.outbox
                    .register_result(&super::result_store::ResultPublicationRecord {
                        outbox_id: claim.id.clone(),
                        source_hash: source_hash.clone(),
                        result_revision: i64::try_from(revision).unwrap_or(i64::MAX),
                        destination_identity,
                        request_digest,
                        accepted_at: None,
                        page_state: None,
                        page_expires_at: None,
                        notification_id: None,
                        notification_status: None,
                        updated_at: None,
                    })
            {
                return if error.code == "RESULT_IDEMPOTENCY_CONFLICT" {
                    permanent_failure(
                        "RESULT_IDEMPOTENCY_CONFLICT",
                        "Full-final identity conflicts with a previous submission",
                    )
                } else {
                    DeliveryResult::Retryable(TransportRetry {
                        policy: TransportRetryPolicy::Limited {
                            attempts: 8,
                            delay_ms: 5000,
                        },
                        error_code: "RESULT_METADATA_UNAVAILABLE",
                        error_message: "Cannot persist full-final submission identity",
                    })
                };
            }
            let request_cancellation = CancellationToken::new();
            let permit = match self
                .outbox
                .hold_controller()
                .acquire_send_permit(crate::db::now_ms().unwrap_or(0))
            {
                Ok(Some(permit)) => permit,
                Ok(None) | Err(_) => return DeliveryResult::Held,
            };
            let result = match send_guarded(
                permit,
                self.client.create_result(
                    &profile.base_url,
                    &profile.device_token,
                    &result_page,
                    &request_cancellation,
                ),
                || cancellation.is_cancelled(),
                wait_for_cancellation(cancellation),
            )
            .await
            {
                Ok(result) => result,
                Err(result) => {
                    if matches!(result, DeliveryResult::Cancelled) {
                        request_cancellation.cancel();
                    }
                    return result;
                }
            };
            (self.on_http_result)(&profile, result.as_ref().err());
            return match result {
                Ok(accepted) => match self
                    .outbox
                    .apply_result_receipt(&claim.id, &accepted, false)
                {
                    Ok(
                        super::result_store::ResultReceiptApply::Applied
                        | super::result_store::ResultReceiptApply::AlreadyApplied,
                    ) => DeliveryResult::Accepted(TransportAcceptance {
                        stage: TransportAcceptanceStage::Relay,
                        transport_message_id: Some(accepted.notification_id),
                        provider_message_id: None,
                        accepted_at: Some(accepted.accepted_at),
                    }),
                    Ok(super::result_store::ResultReceiptApply::Stale) => {
                        DeliveryResult::Retryable(TransportRetry {
                            policy: TransportRetryPolicy::Limited {
                                attempts: 8,
                                delay_ms: 5000,
                            },
                            error_code: "RESULT_RECEIPT_STALE",
                            error_message: "Relay returned an older result receipt",
                        })
                    }
                    Err(_) => DeliveryResult::Retryable(TransportRetry {
                        policy: TransportRetryPolicy::Limited {
                            attempts: 8,
                            delay_ms: 5000,
                        },
                        error_code: "RESULT_RECEIPT_UNSAVED",
                        error_message: "Cannot persist result publication receipt",
                    }),
                },
                Err(error) => map_error(&error),
            };
        }
        let request = RelayNotificationV1 {
            schema_version: 1,
            notification_id: claim.id.clone(),
            dedupe_key: claim.dedupe_key.clone(),
            kind: match claim.event_kind.as_str() {
                "activation" => "generic".to_owned(),
                kind => kind.to_owned(),
            },
            priority: claim.priority.clamp(0, 255),
            title: claim.payload.title.clone(),
            body: claim.payload.body.clone(),
            correlation_key: claim.agent_run_key.clone(),
            created_at: claim.created_at.max(0),
            expires_at: claim.expires_at,
        };
        let request_cancellation = CancellationToken::new();
        let permit = match self
            .outbox
            .hold_controller()
            .acquire_send_permit(crate::db::now_ms().unwrap_or(0))
        {
            Ok(Some(permit)) => permit,
            Ok(None) | Err(_) => return DeliveryResult::Held,
        };
        let result = match send_guarded(
            permit,
            self.client.create_notification(
                &profile.base_url,
                &profile.device_token,
                &request,
                &request_cancellation,
            ),
            || cancellation.is_cancelled(),
            wait_for_cancellation(cancellation),
        )
        .await
        {
            Ok(result) => result,
            Err(result) => {
                if matches!(result, DeliveryResult::Cancelled) {
                    request_cancellation.cancel();
                }
                return result;
            }
        };
        (self.on_http_result)(&profile, result.as_ref().err());
        match result {
            Ok(accepted) => DeliveryResult::Accepted(TransportAcceptance {
                stage: TransportAcceptanceStage::Relay,
                transport_message_id: Some(accepted.notification_id),
                provider_message_id: None,
                accepted_at: Some(accepted.accepted_at),
            }),
            Err(error) => map_error(&error),
        }
    }
}

async fn send_guarded<F, C, W>(
    permit: SendPermit,
    request: F,
    is_cancelled: C,
    cancellation_wait: W,
) -> Result<F::Output, DeliveryResult>
where
    F: std::future::Future,
    C: Fn() -> bool,
    W: std::future::Future<Output = ()>,
{
    // The permit branch is intentionally polled first. Its first poll checks
    // durable hold before cancellation; if hold is clear, it then checks
    // cancellation without starting HTTP. Once HTTP has returned Pending, a
    // later cancellation is allowed to win normally.
    tokio::select! {
        biased;
        result = permit.start_http(request, is_cancelled) => match result {
            Ok(result) => Ok(result),
            Err(HttpStartError::Held) => Err(DeliveryResult::Held),
            Err(HttpStartError::Cancelled) => Err(DeliveryResult::Cancelled),
        },
        _ = cancellation_wait => Err(DeliveryResult::Cancelled),
    }
}

impl DeliveryTransport for RelaySender {
    fn deliver(&self, claim: &OutboxClaim, cancellation: &DeliveryCancellation) -> DeliveryResult {
        if cancellation.is_cancelled() {
            if !matches!(
                self.outbox
                    .hold_controller()
                    .status_at(crate::db::now_ms().unwrap_or(0)),
                Ok(status) if status.state == crate::notification::hold::UserHoldState::Inactive
            ) {
                return DeliveryResult::Held;
            }
            return DeliveryResult::Cancelled;
        }
        self.runtime
            .block_on(self.deliver_async(claim, cancellation))
    }
}

fn map_error(error: &RelayHttpError) -> DeliveryResult {
    match error {
        RelayHttpError::TargetUnavailable => DeliveryResult::Blocked(TransportBlock {
            kind: TransportBlockKind::Credentials,
            error_code: "RELAY_TARGET_UNAVAILABLE",
            error_message: "Connect the notification target on Relay before retry",
        }),
        RelayHttpError::Cancelled => DeliveryResult::Cancelled,
        RelayHttpError::RetryAfter { delay_ms } => DeliveryResult::Retryable(TransportRetry {
            policy: TransportRetryPolicy::Limited {
                attempts: 8,
                delay_ms: *delay_ms,
            },
            error_code: "RELAY_RETRY_AFTER",
            error_message: "Relay requested a retry delay",
        }),
        RelayHttpError::Timeout | RelayHttpError::Transport(_) => relay_network_retry(),
        RelayHttpError::AuthenticationRejected(401 | 403) | RelayHttpError::InvalidDeviceToken => {
            relay_credentials_block()
        }
        RelayHttpError::InsufficientScope => permanent_failure(
            "RELAY_INSUFFICIENT_SCOPE",
            "Relay device token lacks notification permission",
        ),
        RelayHttpError::HttpStatus {
            endpoint: RelayEndpoint::Notifications | RelayEndpoint::Results,
            status: 409,
        } => permanent_failure(
            "IDEMPOTENCY_CONFLICT",
            "Relay rejected the notification idempotency key",
        ),
        RelayHttpError::HttpStatus {
            endpoint: RelayEndpoint::Notifications | RelayEndpoint::Results,
            status: 429,
        } => DeliveryResult::Retryable(TransportRetry {
            policy: TransportRetryPolicy::RateLimitBackoff,
            error_code: "RELAY_RATE_LIMITED",
            error_message: "Relay rate limited the notification",
        }),
        RelayHttpError::HttpStatus {
            status: 408 | 500..=599,
            ..
        }
        | RelayHttpError::NotReady(_)
        | RelayHttpError::ClientConfiguration => relay_network_retry(),
        RelayHttpError::InvalidEndpoint => relay_credentials_block(),
        RelayHttpError::AuthenticationRejected(_) => permanent_failure(
            "RELAY_AUTH_REJECTED",
            "Relay rejected the client authentication request",
        ),
        RelayHttpError::HttpStatus { .. }
        | RelayHttpError::ResponseTooLarge
        | RelayHttpError::InvalidJson(_)
        | RelayHttpError::InvalidResponse(_)
        | RelayHttpError::UnsupportedApiVersion(_) => permanent_failure(
            "RELAY_DELIVERY_REJECTED",
            "Relay permanently rejected the notification",
        ),
    }
}

fn relay_network_retry() -> DeliveryResult {
    DeliveryResult::Retryable(TransportRetry {
        policy: TransportRetryPolicy::NetworkBackoff,
        error_code: "RELAY_TRANSPORT_RETRY",
        error_message: "Relay transport is temporarily unavailable",
    })
}

fn relay_credentials_block() -> DeliveryResult {
    DeliveryResult::Blocked(TransportBlock {
        kind: TransportBlockKind::Credentials,
        error_code: "RELAY_CREDENTIALS_REQUIRED",
        error_message: "Relay credentials must be refreshed before retry",
    })
}

fn permanent_failure(error_code: &'static str, error_message: &'static str) -> DeliveryResult {
    DeliveryResult::PermanentFailure(TransportFailure {
        error_code,
        error_message,
    })
}

async fn wait_for_cancellation(cancellation: &DeliveryCancellation) {
    while !cancellation.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::Poll;

    #[test]
    fn notification_conflict_is_an_immediate_permanent_failure() {
        assert_eq!(
            map_error(&RelayHttpError::HttpStatus {
                endpoint: RelayEndpoint::Notifications,
                status: 409,
            }),
            DeliveryResult::PermanentFailure(TransportFailure {
                error_code: "IDEMPOTENCY_CONFLICT",
                error_message: "Relay rejected the notification idempotency key",
            })
        );
    }

    #[test]
    fn authentication_errors_use_relay_credentials_block_semantics() {
        for status in [401, 403] {
            let result = map_error(&RelayHttpError::AuthenticationRejected(status));
            let DeliveryResult::Blocked(block) = result else {
                panic!("authentication rejection must block on Relay credentials");
            };
            assert_eq!(block.kind, TransportBlockKind::Credentials);
            assert_eq!(block.error_code, "RELAY_CREDENTIALS_REQUIRED");
            assert!(!block.error_code.contains("WECHAT"));
            assert!(!block.error_code.contains("RECONNECT"));
        }
    }

    #[test]
    fn insufficient_scope_is_permanent_without_blocking_credentials() {
        assert_eq!(
            map_error(&RelayHttpError::InsufficientScope),
            DeliveryResult::PermanentFailure(TransportFailure {
                error_code: "RELAY_INSUFFICIENT_SCOPE",
                error_message: "Relay device token lacks notification permission",
            })
        );
    }

    #[test]
    fn network_and_server_failures_remain_retryable() {
        for error in [
            RelayHttpError::Timeout,
            RelayHttpError::HttpStatus {
                endpoint: RelayEndpoint::Notifications,
                status: 503,
            },
        ] {
            assert!(matches!(
                map_error(&error),
                DeliveryResult::Retryable(TransportRetry {
                    policy: TransportRetryPolicy::NetworkBackoff,
                    ..
                })
            ));
        }
    }

    #[tokio::test]
    async fn guarded_select_prefers_committed_hold_over_simultaneous_cancel_before_http_poll() {
        let db = Arc::new(crate::db::Db::open_in_memory().unwrap());
        let outbox = crate::notification::outbox::OutboxRepository::new(Arc::clone(&db));
        let hold = outbox.hold_controller();
        let now = crate::db::now_ms().unwrap();
        let permit = hold.acquire_send_permit(now).unwrap().unwrap();
        hold.hold(
            0,
            crate::notification::hold::HoldDuration::Minutes15,
            now + 1,
        )
        .unwrap();
        let cancelled = AtomicBool::new(true);
        let polls = AtomicUsize::new(0);
        let result = send_guarded(
            permit,
            std::future::poll_fn(|_| {
                polls.fetch_add(1, Ordering::SeqCst);
                Poll::<()>::Ready(())
            }),
            || cancelled.load(Ordering::SeqCst),
            std::future::ready(()),
        )
        .await;
        assert!(matches!(result, Err(DeliveryResult::Held)));
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }
}
