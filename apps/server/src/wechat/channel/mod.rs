use std::sync::Arc;

use async_trait::async_trait;
use relay_provider_wechat::{
    credentials::ConnectionBundle,
    http_client::{WechatBusinessErrorCategory, WechatHttpClient, WechatHttpError},
    protocol::{SendMessageRequest, SendMessageResponse},
};
use subtle::ConstantTimeEq as _;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::outbox::{ChannelMessage, ChannelOutcome, NotificationChannel, RetryClass};

use super::account_fingerprint;
use super::monitor::WechatMonitorHandle;

const BOT_AGENT: &str = concat!("PromptDockRelay/", env!("CARGO_PKG_VERSION"));
const MAX_PROVIDER_MESSAGE_ID_BYTES: usize = 512;

/// A point-in-time copy of the active connection. The revision changes whenever
/// monitor-owned session state changes, allowing a sender to distinguish a
/// genuinely rejected context from one that was superseded while the request
/// was in flight.
pub struct ConnectionSnapshot {
    pub bundle: ConnectionBundle,
    pub revision: u64,
}

pub trait WechatConnectionSource: Send + Sync + 'static {
    fn connection_snapshot(&self) -> Option<ConnectionSnapshot>;

    fn has_connection(&self) -> bool;

    fn revision(&self) -> u64;

    fn mark_reconnect_required(&self, _expected_revision: u64, _error_code: &'static str) -> bool {
        false
    }

    fn mark_activation_required(&self, _expected_revision: u64, _error_code: &'static str) -> bool {
        false
    }
}

impl WechatConnectionSource for WechatMonitorHandle {
    fn connection_snapshot(&self) -> Option<ConnectionSnapshot> {
        if !self.safe_status().activated {
            return None;
        }
        WechatMonitorHandle::connection_snapshot(self).map(|snapshot| ConnectionSnapshot {
            bundle: snapshot.bundle,
            revision: snapshot.revision,
        })
    }

    fn has_connection(&self) -> bool {
        self.snapshot_bundle().is_some()
    }

    fn revision(&self) -> u64 {
        WechatMonitorHandle::revision(self)
    }

    fn mark_reconnect_required(&self, expected_revision: u64, error_code: &'static str) -> bool {
        self.mark_reconnect_required_if_revision(expected_revision, error_code)
    }

    fn mark_activation_required(&self, expected_revision: u64, error_code: &'static str) -> bool {
        self.mark_activation_required_if_revision(expected_revision, error_code)
    }
}

#[async_trait]
pub trait WechatMessageSender: Send + Sync + 'static {
    async fn send_message(
        &self,
        base_url: &Url,
        token: &str,
        body: &SendMessageRequest,
        cancellation: &CancellationToken,
    ) -> Result<SendMessageResponse, WechatHttpError>;
}

#[async_trait]
impl WechatMessageSender for WechatHttpClient {
    async fn send_message(
        &self,
        base_url: &Url,
        token: &str,
        body: &SendMessageRequest,
        cancellation: &CancellationToken,
    ) -> Result<SendMessageResponse, WechatHttpError> {
        WechatHttpClient::send_message(self, base_url, token, body, cancellation).await
    }
}

pub struct WechatChannel {
    connection: Arc<dyn WechatConnectionSource>,
    sender: Arc<dyn WechatMessageSender>,
}

impl WechatChannel {
    pub fn production(
        connection: Arc<dyn WechatConnectionSource>,
    ) -> Result<Self, WechatHttpError> {
        Ok(Self::new(
            connection,
            Arc::new(WechatHttpClient::production(BOT_AGENT)?),
        ))
    }

    pub fn new(
        connection: Arc<dyn WechatConnectionSource>,
        sender: Arc<dyn WechatMessageSender>,
    ) -> Self {
        Self { connection, sender }
    }
}

#[async_trait]
impl NotificationChannel for WechatChannel {
    async fn send(
        &self,
        message: &ChannelMessage,
        cancellation: CancellationToken,
    ) -> ChannelOutcome {
        let Some(snapshot) = self.connection.connection_snapshot() else {
            return if self.connection.has_connection() {
                ChannelOutcome::BlockedActivation
            } else {
                ChannelOutcome::BlockedReconnect
            };
        };
        let bundle = snapshot.bundle;
        if let Some(target) = message.target_account_fingerprint.as_deref() {
            let current = account_fingerprint(&bundle.credentials.user_id);
            if current.as_bytes().ct_eq(target.as_bytes()).unwrap_u8() != 1 {
                return ChannelOutcome::BlockedTargetChanged;
            }
        }
        let Some(context_token) = bundle.session.context_token.clone() else {
            return ChannelOutcome::BlockedActivation;
        };
        if bundle.session.context_token_user_id.as_deref()
            != Some(bundle.credentials.user_id.as_str())
        {
            return ChannelOutcome::BlockedActivation;
        }
        let Ok(base_url) = Url::parse(&bundle.credentials.base_url) else {
            return ChannelOutcome::BlockedReconnect;
        };
        let request = SendMessageRequest::text(
            bundle.credentials.user_id.clone(),
            message.body.clone(),
            message.client_id.clone(),
            Some(context_token),
            message.correlation_key.clone(),
            BOT_AGENT,
        );
        match self
            .sender
            .send_message(
                &base_url,
                &bundle.credentials.bot_token,
                &request,
                &cancellation,
            )
            .await
        {
            Ok(response) => ChannelOutcome::Accepted {
                provider_message_id: provider_message_id(response.message_id.clone()),
            },
            Err(error) => {
                let mut outcome =
                    map_send_error(&error, snapshot.revision != self.connection.revision());
                if matches!(outcome, ChannelOutcome::BlockedReconnect)
                    && !self.connection.mark_reconnect_required(
                        snapshot.revision,
                        "WECHAT_SEND_RECONNECT_REQUIRED",
                    )
                {
                    outcome = ChannelOutcome::Retryable {
                        class: RetryClass::ContextChanged,
                    };
                }
                if matches!(
                    outcome,
                    ChannelOutcome::Retryable {
                        class: RetryClass::ContextRejected | RetryClass::AmbiguousMinusTwo
                    }
                ) && !self
                    .connection
                    .mark_activation_required(snapshot.revision, "WECHAT_SEND_ACTIVATION_REQUIRED")
                {
                    outcome = ChannelOutcome::Retryable {
                        class: RetryClass::ContextChanged,
                    };
                }
                outcome
            }
        }
    }
}

fn provider_message_id(value: Option<serde_json::Value>) -> Option<String> {
    let value = match value? {
        serde_json::Value::String(value) => value,
        serde_json::Value::Number(value) => value.to_string(),
        _ => return None,
    };
    (!value.is_empty() && value.len() <= MAX_PROVIDER_MESSAGE_ID_BYTES).then_some(value)
}

fn map_send_error(error: &WechatHttpError, context_changed: bool) -> ChannelOutcome {
    match error {
        WechatHttpError::Cancelled => ChannelOutcome::Cancelled,
        WechatHttpError::Timeout
        | WechatHttpError::Transport(_)
        | WechatHttpError::ResponseTooLarge
        | WechatHttpError::InvalidJson
        | WechatHttpError::InvalidResponse => ChannelOutcome::Retryable {
            class: RetryClass::Network,
        },
        WechatHttpError::HttpStatus(429) => ChannelOutcome::Retryable {
            class: RetryClass::RateLimited,
        },
        WechatHttpError::HttpStatus(401 | 403) => ChannelOutcome::BlockedReconnect,
        WechatHttpError::HttpStatus(status) if *status >= 500 => ChannelOutcome::Retryable {
            class: RetryClass::Network,
        },
        WechatHttpError::ApiRejected { category, .. } => match category {
            WechatBusinessErrorCategory::AuthenticationExpired => ChannelOutcome::BlockedReconnect,
            WechatBusinessErrorCategory::ContextRejected if context_changed => {
                ChannelOutcome::Retryable {
                    class: RetryClass::ContextChanged,
                }
            }
            WechatBusinessErrorCategory::ContextRejected => ChannelOutcome::Retryable {
                class: RetryClass::ContextRejected,
            },
            WechatBusinessErrorCategory::RateLimited => ChannelOutcome::Retryable {
                class: RetryClass::RateLimited,
            },
            WechatBusinessErrorCategory::AmbiguousMinusTwo => ChannelOutcome::Retryable {
                class: RetryClass::AmbiguousMinusTwo,
            },
            WechatBusinessErrorCategory::PolicyRejected
            | WechatBusinessErrorCategory::InvalidRequest
            | WechatBusinessErrorCategory::Unknown => ChannelOutcome::PermanentFailure,
        },
        WechatHttpError::InvalidEndpoint
        | WechatHttpError::InvalidHeader
        | WechatHttpError::ClientConfiguration
        | WechatHttpError::HttpStatus(_) => ChannelOutcome::PermanentFailure,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    use relay_provider_wechat::{
        credentials::{
            ConnectionBundle, WECHAT_SECRET_SCHEMA_VERSION, WechatCredentials, WechatSessionSecrets,
        },
        http_client::TransportErrorKind,
    };

    use super::*;

    struct Source {
        bundle: Mutex<Option<ConnectionBundle>>,
        revision: Arc<AtomicU64>,
        advance_after_revision_read: AtomicBool,
        activation_required: AtomicBool,
        reconnect_required: AtomicBool,
    }

    impl WechatConnectionSource for Source {
        fn connection_snapshot(&self) -> Option<ConnectionSnapshot> {
            self.bundle
                .lock()
                .expect("bundle")
                .clone()
                .map(|bundle| ConnectionSnapshot {
                    bundle,
                    revision: self.revision.load(Ordering::Acquire),
                })
        }

        fn revision(&self) -> u64 {
            let revision = self.revision.load(Ordering::Acquire);
            if self
                .advance_after_revision_read
                .swap(false, Ordering::AcqRel)
            {
                self.revision.fetch_add(1, Ordering::AcqRel);
            }
            revision
        }

        fn has_connection(&self) -> bool {
            self.bundle.lock().expect("bundle").is_some()
        }

        fn mark_reconnect_required(
            &self,
            expected_revision: u64,
            _error_code: &'static str,
        ) -> bool {
            if self.revision.load(Ordering::Acquire) != expected_revision {
                return false;
            }
            self.reconnect_required.store(true, Ordering::Release);
            true
        }

        fn mark_activation_required(
            &self,
            expected_revision: u64,
            _error_code: &'static str,
        ) -> bool {
            if self.revision.load(Ordering::Acquire) != expected_revision {
                return false;
            }
            self.activation_required.store(true, Ordering::Release);
            true
        }
    }

    struct Sender {
        result: Mutex<Option<Result<SendMessageResponse, WechatHttpError>>>,
        request: Mutex<Option<SendMessageRequest>>,
    }

    #[async_trait]
    impl WechatMessageSender for Sender {
        async fn send_message(
            &self,
            _base_url: &Url,
            _token: &str,
            body: &SendMessageRequest,
            _cancellation: &CancellationToken,
        ) -> Result<SendMessageResponse, WechatHttpError> {
            *self.request.lock().expect("request") = Some(body.clone());
            self.result
                .lock()
                .expect("result")
                .take()
                .expect("one send")
        }
    }

    fn bundle(context: bool) -> ConnectionBundle {
        let mut session = WechatSessionSecrets::default();
        if context {
            session.context_token = Some("context-secret".into());
            session.context_token_user_id = Some("user@im.wechat".into());
            session.context_token_updated_at = Some(2);
        }
        ConnectionBundle::new(
            WechatCredentials {
                schema_version: WECHAT_SECRET_SCHEMA_VERSION,
                bot_token: "bot-secret".into(),
                account_id: "bot@im.bot".into(),
                user_id: "user@im.wechat".into(),
                base_url: "https://ilinkai.weixin.qq.com".into(),
                saved_at: 1,
            },
            session,
        )
        .expect("bundle")
    }

    #[tokio::test]
    async fn sends_bound_text_with_context_and_preserves_provider_id() {
        let source = Arc::new(Source {
            bundle: Mutex::new(Some(bundle(true))),
            revision: Arc::new(AtomicU64::new(7)),
            advance_after_revision_read: AtomicBool::new(false),
            activation_required: AtomicBool::new(false),
            reconnect_required: AtomicBool::new(false),
        });
        let sender = Arc::new(Sender {
            result: Mutex::new(Some(Ok(SendMessageResponse {
                ret: Some(0),
                errcode: None,
                errmsg: None,
                message_id: Some(serde_json::json!(42)),
            }))),
            request: Mutex::new(None),
        });
        let channel = WechatChannel::new(source, sender.clone());
        let outcome = channel
            .send(
                &ChannelMessage {
                    client_id: "stable-client".into(),
                    body: "hello".into(),
                    correlation_key: Some("run-1".into()),
                    target_account_fingerprint: Some(account_fingerprint("user@im.wechat")),
                },
                CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(outcome, ChannelOutcome::Accepted { provider_message_id: Some(ref id) } if id == "42")
        );
        let request = sender.request.lock().expect("request");
        let request = request.as_ref().expect("captured");
        assert_eq!(request.msg.to_user_id, "user@im.wechat");
        assert_eq!(request.msg.client_id, "stable-client");
        assert_eq!(request.msg.context_token.as_deref(), Some("context-secret"));
        assert_eq!(request.msg.run_id.as_deref(), Some("run-1"));
    }

    #[tokio::test]
    async fn system_target_account_mismatch_fails_before_provider_send() {
        let source = Arc::new(Source {
            bundle: Mutex::new(Some(bundle(true))),
            revision: Arc::new(AtomicU64::new(7)),
            advance_after_revision_read: AtomicBool::new(false),
            activation_required: AtomicBool::new(false),
            reconnect_required: AtomicBool::new(false),
        });
        let sender = Arc::new(Sender {
            result: Mutex::new(None),
            request: Mutex::new(None),
        });
        let channel = WechatChannel::new(source, sender.clone());
        let outcome = channel
            .send(
                &ChannelMessage {
                    client_id: "stable-client".into(),
                    body: "must not cross accounts".into(),
                    correlation_key: None,
                    target_account_fingerprint: Some(account_fingerprint("rebound@im.wechat")),
                },
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(outcome, ChannelOutcome::BlockedTargetChanged));
        assert!(sender.request.lock().expect("request").is_none());
    }

    #[tokio::test]
    async fn blocks_without_connection_or_activation_context() {
        let source = Arc::new(Source {
            bundle: Mutex::new(None),
            revision: Arc::new(AtomicU64::new(0)),
            advance_after_revision_read: AtomicBool::new(false),
            activation_required: AtomicBool::new(false),
            reconnect_required: AtomicBool::new(false),
        });
        let sender = Arc::new(Sender {
            result: Mutex::new(None),
            request: Mutex::new(None),
        });
        let channel = WechatChannel::new(source.clone(), sender.clone());
        assert!(matches!(
            channel
                .send(
                    &ChannelMessage {
                        client_id: "c".into(),
                        body: "b".into(),
                        correlation_key: None,
                        target_account_fingerprint: None
                    },
                    CancellationToken::new()
                )
                .await,
            ChannelOutcome::BlockedReconnect
        ));
        *source.bundle.lock().expect("bundle") = Some(bundle(false));
        assert!(matches!(
            channel
                .send(
                    &ChannelMessage {
                        client_id: "c".into(),
                        body: "b".into(),
                        correlation_key: None,
                        target_account_fingerprint: None
                    },
                    CancellationToken::new()
                )
                .await,
            ChannelOutcome::BlockedActivation
        ));
    }

    #[test]
    fn classifies_provider_failures_without_exposing_business_text() {
        assert!(matches!(
            map_send_error(
                &WechatHttpError::Transport(TransportErrorKind::Connect),
                false
            ),
            ChannelOutcome::Retryable {
                class: RetryClass::Network
            }
        ));
        assert!(matches!(
            map_send_error(&WechatHttpError::HttpStatus(429), false),
            ChannelOutcome::Retryable {
                class: RetryClass::RateLimited
            }
        ));
        assert!(matches!(
            map_send_error(
                &WechatHttpError::ApiRejected {
                    ret: Some(-14),
                    errcode: None,
                    category: WechatBusinessErrorCategory::AuthenticationExpired,
                    safe_message: None
                },
                false
            ),
            ChannelOutcome::BlockedReconnect
        ));
        assert!(matches!(
            map_send_error(
                &WechatHttpError::ApiRejected {
                    ret: Some(-2),
                    errcode: None,
                    category: WechatBusinessErrorCategory::ContextRejected,
                    safe_message: None
                },
                true
            ),
            ChannelOutcome::Retryable {
                class: RetryClass::ContextChanged
            }
        ));
        assert!(matches!(
            map_send_error(
                &WechatHttpError::ApiRejected {
                    ret: Some(-2),
                    errcode: None,
                    category: WechatBusinessErrorCategory::AmbiguousMinusTwo,
                    safe_message: None
                },
                false
            ),
            ChannelOutcome::Retryable {
                class: RetryClass::AmbiguousMinusTwo
            }
        ));
    }

    #[tokio::test]
    async fn provider_rejections_fail_closed_in_the_shared_channel_state() {
        let source = Arc::new(Source {
            bundle: Mutex::new(Some(bundle(true))),
            revision: Arc::new(AtomicU64::new(1)),
            advance_after_revision_read: AtomicBool::new(false),
            activation_required: AtomicBool::new(false),
            reconnect_required: AtomicBool::new(false),
        });
        let sender = Arc::new(Sender {
            result: Mutex::new(Some(Err(WechatHttpError::ApiRejected {
                ret: Some(-2),
                errcode: None,
                category: WechatBusinessErrorCategory::ContextRejected,
                safe_message: None,
            }))),
            request: Mutex::new(None),
        });
        let channel = WechatChannel::new(source.clone(), sender);
        let outcome = channel
            .send(
                &ChannelMessage {
                    client_id: "stable-client".into(),
                    body: "hello".into(),
                    correlation_key: None,
                    target_account_fingerprint: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            outcome,
            ChannelOutcome::Retryable {
                class: RetryClass::ContextRejected
            }
        ));
        assert!(source.activation_required.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn stale_send_failures_do_not_invalidate_a_replaced_bundle() {
        for category in [
            WechatBusinessErrorCategory::AuthenticationExpired,
            WechatBusinessErrorCategory::ContextRejected,
        ] {
            let source = Arc::new(Source {
                bundle: Mutex::new(Some(bundle(true))),
                revision: Arc::new(AtomicU64::new(7)),
                advance_after_revision_read: AtomicBool::new(true),
                activation_required: AtomicBool::new(false),
                reconnect_required: AtomicBool::new(false),
            });
            let sender = Arc::new(Sender {
                result: Mutex::new(Some(Err(WechatHttpError::ApiRejected {
                    ret: Some(-2),
                    errcode: None,
                    category,
                    safe_message: None,
                }))),
                request: Mutex::new(None),
            });
            let outcome = WechatChannel::new(source.clone(), sender)
                .send(
                    &ChannelMessage {
                        client_id: "stable-client".into(),
                        body: "hello".into(),
                        correlation_key: None,
                        target_account_fingerprint: None,
                    },
                    CancellationToken::new(),
                )
                .await;

            assert!(matches!(
                outcome,
                ChannelOutcome::Retryable {
                    class: RetryClass::ContextChanged
                }
            ));
            assert!(!source.activation_required.load(Ordering::Acquire));
            assert!(!source.reconnect_required.load(Ordering::Acquire));
        }
    }
}
