use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use relay_provider_wechat::{
    credentials::{
        ConnectionBundle, WECHAT_SECRET_SCHEMA_VERSION, WechatCredentials, WechatSessionSecrets,
    },
    http_client::{TransportErrorKind, WechatBusinessErrorCategory, WechatHttpError},
    protocol::GetUpdatesResponse,
};
use serde_json::json;
use tokio::{sync::Notify, time::timeout};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::inbound::{
    EphemeralInboundText, InboundAcceptOutcome, InboundMessageError, InboundMessageSink,
};
use crate::wechat::secret_store::{SecretStore, SecretStoreError};

use super::{
    poll::{
        DEGRADED_AFTER_FAILURES, error_code, finish_monitor, is_terminal_error, set_active,
        set_last_poll, set_retry_failure, set_terminal_failure, set_worker_failure,
    },
    runtime::WechatMonitorRuntime,
    *,
};

struct MemoryStore {
    bundle: Mutex<Option<ConnectionBundle>>,
    saves: Mutex<Vec<ConnectionBundle>>,
    fail_saves: AtomicUsize,
}

impl MemoryStore {
    fn new(bundle: Option<ConnectionBundle>) -> Self {
        Self {
            bundle: Mutex::new(bundle),
            saves: Mutex::new(Vec::new()),
            fail_saves: AtomicUsize::new(0),
        }
    }

    fn fail_next_saves(&self, count: usize) {
        self.fail_saves.store(count, Ordering::SeqCst);
    }

    fn saved(&self) -> Vec<ConnectionBundle> {
        self.saves.lock().expect("saves").clone()
    }

    fn current(&self) -> Option<ConnectionBundle> {
        self.bundle.lock().expect("bundle").clone()
    }
}

#[async_trait]
impl SecretStore for MemoryStore {
    async fn load(&self) -> Result<Option<ConnectionBundle>, SecretStoreError> {
        Ok(self.bundle.lock().expect("bundle").clone())
    }

    async fn save(&self, bundle: &ConnectionBundle) -> Result<(), SecretStoreError> {
        if self
            .fail_saves
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(SecretStoreError::Io(std::io::Error::other(
                "injected save failure",
            )));
        }
        self.saves.lock().expect("saves").push(bundle.clone());
        *self.bundle.lock().expect("bundle") = Some(bundle.clone());
        Ok(())
    }

    async fn clear(&self) -> Result<(), SecretStoreError> {
        *self.bundle.lock().expect("bundle") = None;
        Ok(())
    }
}

#[derive(Default)]
struct RecordingSink {
    committed: Mutex<HashMap<String, (String, String)>>,
    attempts: Mutex<Vec<String>>,
    bodies: Mutex<Vec<String>>,
    fail_accepts: AtomicUsize,
    fail_on_attempt: AtomicUsize,
    cancel_after_commit: AtomicBool,
}

impl RecordingSink {
    fn fail_next_accepts(&self, count: usize) {
        self.fail_accepts.store(count, Ordering::SeqCst);
    }

    fn fail_on_attempt(&self, attempt: usize) {
        self.fail_on_attempt.store(attempt, Ordering::SeqCst);
    }

    fn cancel_once_after_commit(&self) {
        self.cancel_after_commit.store(true, Ordering::SeqCst);
    }

    fn attempts(&self) -> Vec<String> {
        self.attempts.lock().expect("attempts").clone()
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("bodies").clone()
    }
}

#[async_trait]
impl InboundMessageSink for RecordingSink {
    async fn accept(
        &self,
        message: EphemeralInboundText,
        cancellation: &CancellationToken,
    ) -> Result<InboundAcceptOutcome, InboundMessageError> {
        if cancellation.is_cancelled() {
            return Err(InboundMessageError::Cancelled);
        }
        let attempt = {
            let mut attempts = self.attempts.lock().expect("attempts");
            attempts.push(message.message_key.clone());
            attempts.len()
        };
        let fail_on_attempt = self.fail_on_attempt.load(Ordering::SeqCst);
        if (fail_on_attempt != 0
            && attempt == fail_on_attempt
            && self
                .fail_on_attempt
                .compare_exchange(fail_on_attempt, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok())
            || self
                .fail_accepts
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
        {
            return Err(InboundMessageError::Database);
        }

        let payload_hash = blake3::hash(message.text.as_bytes()).to_hex().to_string();
        let identity = (message.sender_fingerprint.clone(), payload_hash);
        let outcome = {
            let mut committed = self.committed.lock().expect("committed");
            match committed.get(&message.message_key) {
                Some(existing) if existing == &identity => InboundAcceptOutcome::ExactReplay,
                Some(_) => return Err(InboundMessageError::IdempotencyConflict),
                None => {
                    committed.insert(message.message_key.clone(), identity);
                    self.bodies
                        .lock()
                        .expect("bodies")
                        .push(message.text.to_string());
                    InboundAcceptOutcome::Fresh
                }
            }
        };
        if self.cancel_after_commit.swap(false, Ordering::SeqCst) {
            cancellation.cancel();
        }
        Ok(outcome)
    }
}

struct CancellationSink {
    started: Notify,
}

#[async_trait]
impl InboundMessageSink for CancellationSink {
    async fn accept(
        &self,
        _message: EphemeralInboundText,
        cancellation: &CancellationToken,
    ) -> Result<InboundAcceptOutcome, InboundMessageError> {
        self.started.notify_one();
        cancellation.cancelled().await;
        Err(InboundMessageError::Cancelled)
    }
}

enum PollAction {
    Response(GetUpdatesResponse),
    BlockedResponse(Arc<Notify>, GetUpdatesResponse),
    Error(WechatHttpError),
    Panic,
}

#[derive(Default)]
struct FakeTransport {
    polls: Mutex<VecDeque<PollAction>>,
    starts: AtomicUsize,
    stops: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl FakeTransport {
    fn with_polls(polls: impl IntoIterator<Item = PollAction>) -> Self {
        Self {
            polls: Mutex::new(polls.into_iter().collect()),
            ..Self::default()
        }
    }

    fn observe_active(&self, value: usize) {
        self.max_active.fetch_max(value, Ordering::SeqCst);
    }
}

#[async_trait]
impl WechatMonitorTransport for FakeTransport {
    async fn notify_start(
        &self,
        _base_url: &Url,
        _token: &str,
        _cancellation: &CancellationToken,
    ) -> Result<(), WechatHttpError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.observe_active(active);
        Ok(())
    }

    async fn get_updates(
        &self,
        _base_url: &Url,
        _token: &str,
        _get_updates_buf: &str,
        cancellation: &CancellationToken,
    ) -> Result<GetUpdatesResponse, WechatHttpError> {
        let action = self.polls.lock().expect("polls").pop_front();
        match action {
            Some(PollAction::Response(response)) => Ok(response),
            Some(PollAction::BlockedResponse(release, response)) => {
                tokio::select! {
                    () = release.notified() => Ok(response),
                    () = cancellation.cancelled() => Err(WechatHttpError::Cancelled),
                }
            }
            Some(PollAction::Error(error)) => Err(error),
            Some(PollAction::Panic) => panic!("monitor transport panic sentinel"),
            None => {
                cancellation.cancelled().await;
                Err(WechatHttpError::Cancelled)
            }
        }
    }

    async fn notify_stop(
        &self,
        _base_url: &Url,
        _token: &str,
        _cancellation: &CancellationToken,
    ) -> Result<(), WechatHttpError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        let _ = self
            .active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                Some(value.saturating_sub(1))
            });
        Ok(())
    }
}

#[derive(Default)]
struct RecordingHook {
    activations: AtomicUsize,
    reconnects: AtomicUsize,
}

#[async_trait]
impl MonitorActivationHook for RecordingHook {
    async fn activated(&self) {
        self.activations.fetch_add(1, Ordering::SeqCst);
    }

    async fn reconnected(&self) {
        self.reconnects.fetch_add(1, Ordering::SeqCst);
    }
}

fn bundle(user: &str, context: Option<&str>) -> ConnectionBundle {
    let credentials = WechatCredentials {
        schema_version: WECHAT_SECRET_SCHEMA_VERSION,
        bot_token: format!("token-{user}"),
        account_id: "bot@im.bot".into(),
        user_id: user.into(),
        base_url: "https://ilinkai.weixin.qq.com/".into(),
        saved_at: 1_700_000_000_000,
    };
    let mut session = WechatSessionSecrets::default();
    session.get_updates_buf = "cursor-1".into();
    if let Some(context) = context {
        session.context_token = Some(context.into());
        session.context_token_user_id = Some(user.into());
        session.context_token_updated_at = Some(1_700_000_000_100);
    }
    ConnectionBundle::new(credentials, session).expect("bundle")
}

fn response(value: serde_json::Value) -> GetUpdatesResponse {
    serde_json::from_value(value).expect("getupdates response")
}

fn runtime(
    transport: Arc<FakeTransport>,
    store: Arc<MemoryStore>,
    hook: Arc<RecordingHook>,
) -> WechatMonitorRuntime {
    runtime_with_sink(transport, store, Arc::new(RecordingSink::default()), hook)
}

fn runtime_with_sink(
    transport: Arc<FakeTransport>,
    store: Arc<MemoryStore>,
    sink: Arc<dyn InboundMessageSink>,
    hook: Arc<RecordingHook>,
) -> WechatMonitorRuntime {
    WechatMonitorRuntime::with_transport_and_timing(
        transport,
        store,
        sink,
        hook,
        Duration::from_millis(2),
        Duration::from_millis(8),
    )
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("condition");
}

fn authentication_expired() -> WechatHttpError {
    WechatHttpError::ApiRejected {
        ret: Some(-14),
        errcode: Some(-14),
        category: WechatBusinessErrorCategory::AuthenticationExpired,
        safe_message: None,
    }
}

#[test]
fn failure_classification_separates_reconnect_required_from_transient_errors() {
    for error in [
        WechatHttpError::InvalidEndpoint,
        WechatHttpError::HttpStatus(401),
        WechatHttpError::HttpStatus(403),
        authentication_expired(),
    ] {
        assert!(is_terminal_error(&error), "not terminal: {error:?}");
    }
    for error in [
        WechatHttpError::Transport(TransportErrorKind::Connect),
        WechatHttpError::Timeout,
        WechatHttpError::HttpStatus(429),
        WechatHttpError::HttpStatus(500),
        WechatHttpError::InvalidJson,
    ] {
        assert!(!is_terminal_error(&error), "unexpected terminal: {error:?}");
    }
    assert_eq!(
        error_code(&WechatHttpError::HttpStatus(401)),
        ERROR_AUTHENTICATION_EXPIRED
    );
    assert_eq!(
        error_code(&WechatHttpError::HttpStatus(403)),
        ERROR_AUTHENTICATION_EXPIRED
    );
    assert_eq!(
        error_code(&WechatHttpError::HttpStatus(429)),
        ERROR_MONITOR_REJECTED
    );
}

#[test]
fn reconnect_required_is_the_single_terminal_truth_and_wins_races() {
    let runtime = runtime(
        Arc::new(FakeTransport::default()),
        Arc::new(MemoryStore::new(None)),
        Arc::new(RecordingHook::default()),
    );
    let shared = &runtime.shared;
    let active_bundle = bundle("owner", Some("context"));

    set_retry_failure(shared, 1, ERROR_MONITOR_NETWORK);
    assert_eq!(
        shared.lock_state().status.monitor,
        MonitorState::Reconnecting
    );
    set_retry_failure(shared, DEGRADED_AFTER_FAILURES, ERROR_MONITOR_NETWORK);
    assert_eq!(shared.lock_state().status.monitor, MonitorState::Degraded);
    assert_eq!(set_active(shared, &active_bundle), Some(true));
    assert_eq!(shared.lock_state().status.monitor, MonitorState::Active);

    set_terminal_failure(shared, ERROR_AUTHENTICATION_EXPIRED);
    assert_eq!(
        shared.lock_state().status.monitor,
        MonitorState::ReconnectRequired
    );
    set_retry_failure(shared, 99, ERROR_MONITOR_NETWORK);
    set_last_poll(shared, 42);
    set_worker_failure(shared, ERROR_MONITOR_WORKER_EXITED);
    finish_monitor(shared);
    assert_eq!(set_active(shared, &active_bundle), None);
    let status = shared.lock_state().status;
    assert_eq!(status.monitor, MonitorState::ReconnectRequired);
    assert_eq!(status.error_code, Some(ERROR_AUTHENTICATION_EXPIRED));
    assert!(!status.activated);
}

#[test]
fn worker_failure_is_degraded_and_never_requests_new_credentials() {
    let runtime = runtime(
        Arc::new(FakeTransport::default()),
        Arc::new(MemoryStore::new(None)),
        Arc::new(RecordingHook::default()),
    );
    set_worker_failure(&runtime.shared, ERROR_MONITOR_WORKER_EXITED);
    assert_eq!(
        runtime.handle().safe_status(),
        MonitorStatus {
            activated: false,
            monitor: MonitorState::Degraded,
            last_inbound_at: None,
            last_poll_at: None,
            error_code: Some(ERROR_MONITOR_WORKER_EXITED),
        }
    );
}

#[tokio::test]
async fn restores_cursor_context_and_dedupe_but_never_persists_inbound_body() {
    let mut initial = bundle("owner@im.wechat", Some("context-old"));
    assert!(initial.session.remember_message_key("mid:1"));
    let store = Arc::new(MemoryStore::new(Some(initial)));
    let transport = Arc::new(FakeTransport::with_polls([PollAction::Response(response(
        json!({
            "ret": 0,
            "get_updates_buf": "cursor-2",
            "msgs": [
                {"message_id": 1, "from_user_id": "owner@im.wechat", "message_type": 1, "message_state": 2, "context_token": "duplicate-context", "item_list": [{"type": 1, "is_completed": true, "text_item": {"text": "duplicate-body-secret"}}]},
                {"message_id": 2, "from_user_id": "stranger@im.wechat", "message_type": 1, "message_state": 2, "context_token": "foreign-context-secret", "item_list": [{"type": 1, "is_completed": true, "text_item": {"text": "foreign-body-secret"}}]},
                {"message_id": 3, "from_user_id": "owner@im.wechat", "message_type": 1, "message_state": 2, "context_token": "context-new", "item_list": [{"type": 1, "is_completed": true, "text_item": {"text": "inbound-body-secret"}}]}
            ]
        }),
    ))]));
    let hook = Arc::new(RecordingHook::default());
    let runtime = runtime(transport, Arc::clone(&store), Arc::clone(&hook));

    assert!(runtime.start_loaded().await.expect("start"));
    wait_until(|| !store.saved().is_empty()).await;
    let saved = store.saved().pop().expect("saved bundle");
    assert_eq!(saved.session.get_updates_buf, "cursor-2");
    assert_eq!(saved.session.context_token.as_deref(), Some("context-new"));
    assert_eq!(
        saved.session.context_token_user_id.as_deref(),
        Some("owner@im.wechat")
    );
    assert!(saved.session.context_token_updated_at.is_some());
    assert_eq!(saved.session.recent_message_keys, ["mid:1", "mid:3"]);
    let persisted = serde_json::to_string(&saved).expect("serialize bundle");
    for body in [
        "duplicate-body-secret",
        "foreign-body-secret",
        "inbound-body-secret",
    ] {
        assert!(!persisted.contains(body));
    }
    let status = runtime.handle().safe_status();
    assert!(status.activated);
    assert_eq!(status.monitor, MonitorState::Active);
    assert!(status.last_inbound_at.is_some());
    assert!(status.last_poll_at.is_some());
    assert_eq!(hook.activations.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.handle().revision(), 2);
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn foreign_messages_only_advance_cursor_and_never_activate() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    let transport = Arc::new(FakeTransport::with_polls([PollAction::Response(response(
        json!({
            "ret": 0,
            "get_updates_buf": "cursor-foreign",
            "msgs": [{"message_id": 7, "from_user_id": "other", "context_token": "foreign"}]
        }),
    ))]));
    let hook = Arc::new(RecordingHook::default());
    let runtime = runtime(transport, Arc::clone(&store), Arc::clone(&hook));

    runtime.start_loaded().await.expect("start");
    wait_until(|| !store.saved().is_empty()).await;
    let saved = store.saved().pop().expect("saved");
    assert_eq!(saved.session.get_updates_buf, "cursor-foreign");
    assert!(saved.session.context_token.is_none());
    assert!(saved.session.recent_message_keys.is_empty());
    let status = runtime.handle().safe_status();
    assert!(!status.activated);
    assert!(status.last_inbound_at.is_none());
    assert_eq!(hook.activations.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.handle().revision(), 1);
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn transient_failure_reconnects_and_unblocks_after_owned_context() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    let transport = Arc::new(FakeTransport::with_polls([
        PollAction::Error(WechatHttpError::Transport(TransportErrorKind::Request)),
        PollAction::Error(WechatHttpError::HttpStatus(429)),
        PollAction::Error(WechatHttpError::HttpStatus(500)),
        PollAction::Response(response(json!({
            "ret": 0,
            "get_updates_buf": "cursor-after-reconnect",
            "msgs": [{"message_id": 9, "from_user_id": "owner", "context_token": "context-after-reconnect"}]
        }))),
    ]));
    let hook = Arc::new(RecordingHook::default());
    let runtime = runtime(
        Arc::clone(&transport),
        Arc::clone(&store),
        Arc::clone(&hook),
    );

    runtime.start_loaded().await.expect("start");
    wait_until(|| hook.reconnects.load(Ordering::SeqCst) == 1).await;
    wait_until(|| runtime.handle().safe_status().activated).await;
    assert!(transport.starts.load(Ordering::SeqCst) >= 4);
    assert_eq!(hook.activations.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.handle().revision(), 2);
    assert_eq!(runtime.handle().safe_status().error_code, None);
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn panicking_worker_projects_degraded_and_preserves_the_bundle() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", Some("context")))));
    let transport = Arc::new(FakeTransport::with_polls([PollAction::Panic]));
    let runtime = runtime(
        transport,
        Arc::clone(&store),
        Arc::new(RecordingHook::default()),
    );

    runtime.start_loaded().await.expect("start");
    wait_until(|| runtime.handle().safe_status().error_code == Some(ERROR_MONITOR_WORKER_EXITED))
        .await;
    assert_eq!(
        runtime.handle().safe_status().monitor,
        MonitorState::Degraded
    );
    assert!(runtime.handle().snapshot_bundle().is_some());
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn replacement_joins_the_old_owner_before_starting_the_new_one() {
    let first = bundle("first", None);
    let second = bundle("second", None);
    let store = Arc::new(MemoryStore::new(Some(first)));
    let transport = Arc::new(FakeTransport::default());
    let hook = Arc::new(RecordingHook::default());
    let runtime = runtime(
        Arc::clone(&transport),
        Arc::clone(&store),
        Arc::clone(&hook),
    );

    runtime.start_loaded().await.expect("first start");
    wait_until(|| transport.starts.load(Ordering::SeqCst) == 1).await;
    runtime.replace_bundle(second).await.expect("replace");
    wait_until(|| transport.starts.load(Ordering::SeqCst) == 2).await;
    assert_eq!(transport.max_active.load(Ordering::SeqCst), 1);
    let snapshot = runtime.handle().connection_snapshot().expect("snapshot");
    assert_eq!(snapshot.bundle.credentials.user_id, "second");
    assert!(snapshot.revision >= 2);
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn stop_cancels_a_monitor_save_waiting_behind_a_staged_cutover() {
    let directory = tempfile::tempdir().expect("directory");
    let store = crate::wechat::secret_store::EncryptedFileSecretStore::new_for_test(
        directory.path().join("wechat-connection.enc"),
        [9; 32],
    );
    let initial = bundle("owner", None);
    store.save(&initial).await.expect("initial save");
    let staged = store
        .stage(&bundle("candidate", None))
        .await
        .expect("stage candidate");
    let transport = Arc::new(FakeTransport::with_polls([PollAction::Response(response(
        json!({"ret":0,"get_updates_buf":"cursor-blocked-behind-stage","msgs":[]}),
    ))]));
    let runtime = WechatMonitorRuntime::with_transport_and_timing(
        transport,
        Arc::new(store.clone()),
        Arc::new(RecordingSink::default()),
        Arc::new(RecordingHook::default()),
        Duration::from_millis(2),
        Duration::from_millis(8),
    );

    runtime
        .replace_bundle(initial)
        .await
        .expect("start monitor");
    wait_until(|| runtime.handle().safe_status().last_poll_at.is_some()).await;
    tokio::time::timeout(Duration::from_secs(1), runtime.stop())
        .await
        .expect("stop must not deadlock behind staged writer")
        .expect("stop monitor");
    store.discard(staged).await.expect("discard candidate");
}

#[tokio::test]
async fn captures_only_bound_finished_text_with_stable_fallback_keys_and_limits() {
    let oversized = "sensitive-oversized-body".repeat(200);
    assert!(oversized.chars().count() > 4_096);
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    let sink = Arc::new(RecordingSink::default());
    let transport = Arc::new(FakeTransport::with_polls([PollAction::Response(response(
        json!({
            "ret": 0,
            "get_updates_buf": "cursor-filtered",
            "msgs": [
                {"message_id": 10, "from_user_id": "owner", "message_type": 1, "message_state": 2, "item_list": [
                    {"type": 1, "is_completed": true, "text_item": {"text": "first"}},
                    {"type": 7, "is_completed": true, "text_item": {"text": "non-text-secret"}},
                    {"type": 1, "text_item": {"text": "second"}}
                ]},
                {"message_id": 11, "from_user_id": "foreign", "message_type": 1, "message_state": 2, "item_list": [{"type": 1, "text_item": {"text": "foreign-secret"}}]},
                {"message_id": 12, "from_user_id": "owner", "message_type": 2, "message_state": 2, "item_list": [{"type": 1, "text_item": {"text": "bot-echo-secret"}}]},
                {"message_id": 13, "from_user_id": "owner", "message_type": 1, "message_state": 1, "item_list": [{"type": 1, "text_item": {"text": "unfinished-message-secret"}}]},
                {"message_id": 14, "from_user_id": "owner", "message_type": 1, "message_state": 2, "item_list": [{"type": 1, "is_completed": false, "text_item": {"text": "unfinished-item-secret"}}]},
                {"client_id": "client-fallback", "from_user_id": "owner", "message_type": 1, "message_state": 2, "item_list": [{"type": 1, "text_item": {"text": "client body"}}]},
                {"seq": 77, "from_user_id": "owner", "message_type": 1, "message_state": 2, "item_list": [{"type": 1, "text_item": {"text": "seq body"}}]},
                {"message_id": 15, "from_user_id": "owner", "message_type": 1, "message_state": 2, "item_list": [{"type": 1, "text_item": {"text": oversized}}]},
                {"message_id": 17, "from_user_id": "owner", "message_type": 1, "message_state": 2, "item_list": [
                    {"type": 1, "text_item": {"text": "one"}}, {"type": 1, "text_item": {"text": "two"}},
                    {"type": 1, "text_item": {"text": "three"}}, {"type": 1, "text_item": {"text": "four"}},
                    {"type": 1, "text_item": {"text": "five"}}
                ]},
                {"message_id": 16, "from_user_id": "owner", "message_type": 1, "message_state": 2, "item_list": []}
            ]
        }),
    ))]));
    let runtime = runtime_with_sink(
        transport,
        Arc::clone(&store),
        sink.clone(),
        Arc::new(RecordingHook::default()),
    );

    runtime.start_loaded().await.expect("start");
    wait_until(|| !store.saved().is_empty()).await;
    assert_eq!(
        sink.attempts(),
        [
            "mid:10",
            "cid:client-fallback",
            "seq:77",
            "mid:15",
            "mid:17"
        ]
    );
    let bodies = sink.bodies();
    assert_eq!(&bodies[..3], ["first\nsecond", "client body", "seq body"]);
    assert!(bodies[3].chars().count() > 4_096);
    assert!(!bodies[3].contains("sensitive-oversized-body"));
    assert!(bodies[4].chars().count() > 4_096);
    assert!(!bodies[4].contains("one"));
    let saved = store.saved().pop().expect("saved");
    assert_eq!(saved.session.get_updates_buf, "cursor-filtered");
    assert_eq!(
        saved.session.recent_message_keys,
        [
            "mid:10",
            "cid:client-fallback",
            "seq:77",
            "mid:15",
            "mid:17"
        ]
    );
    let persisted = serde_json::to_string(&saved).expect("serialize");
    for secret in [
        "foreign-secret",
        "bot-echo-secret",
        "unfinished-message-secret",
        "unfinished-item-secret",
        "non-text-secret",
        "sensitive-oversized-body",
    ] {
        assert!(!persisted.contains(secret));
    }
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn sink_failure_leaves_cursor_context_and_recent_keys_replayable() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    let sink = Arc::new(RecordingSink::default());
    sink.fail_next_accepts(1);
    let transport = Arc::new(FakeTransport::with_polls([PollAction::Response(response(
        json!({
            "ret": 0,
            "get_updates_buf": "cursor-must-not-save",
            "msgs": [{"message_id": 20, "from_user_id": "owner", "message_type": 1, "message_state": 2, "context_token": "context-must-not-save", "item_list": [{"type": 1, "text_item": {"text": "帮助"}}]}]
        }),
    ))]));
    let runtime = runtime_with_sink(
        transport,
        Arc::clone(&store),
        sink.clone(),
        Arc::new(RecordingHook::default()),
    );

    runtime.start_loaded().await.expect("start");
    wait_until(|| sink.attempts().len() == 1).await;
    assert!(store.saved().is_empty());
    let durable = store.current().expect("initial bundle");
    assert_eq!(durable.session.get_updates_buf, "cursor-1");
    assert!(durable.session.context_token.is_none());
    assert!(durable.session.recent_message_keys.is_empty());
    wait_until(|| runtime.handle().safe_status().error_code == Some(ERROR_MONITOR_CAPTURE_FAILED))
        .await;
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn later_sink_failure_replays_the_entire_batch_without_duplicate_commit() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    let sink = Arc::new(RecordingSink::default());
    sink.fail_on_attempt(2);
    let batch = || {
        response(json!({
            "ret": 0,
            "get_updates_buf": "cursor-after-full-batch",
            "msgs": [
                {"message_id": 21, "from_user_id": "owner", "message_type": 1, "message_state": 2, "context_token": "context-first", "item_list": [{"type": 1, "text_item": {"text": "help"}}]},
                {"message_id": 22, "from_user_id": "owner", "message_type": 1, "message_state": 2, "context_token": "context-second", "item_list": [{"type": 1, "text_item": {"text": "设备"}}]}
            ]
        }))
    };
    let release_replay = Arc::new(Notify::new());
    let transport = Arc::new(FakeTransport::with_polls([
        PollAction::Response(batch()),
        PollAction::BlockedResponse(Arc::clone(&release_replay), batch()),
    ]));
    let runtime = runtime_with_sink(
        transport,
        Arc::clone(&store),
        sink.clone(),
        Arc::new(RecordingHook::default()),
    );

    runtime.start_loaded().await.expect("start");
    wait_until(|| sink.attempts().len() == 2).await;
    assert!(store.saved().is_empty());
    let before_replay = store.current().expect("initial bundle");
    assert_eq!(before_replay.session.get_updates_buf, "cursor-1");
    assert!(before_replay.session.context_token.is_none());
    assert!(before_replay.session.recent_message_keys.is_empty());

    release_replay.notify_one();
    wait_until(|| !store.saved().is_empty()).await;
    assert_eq!(sink.attempts(), ["mid:21", "mid:22", "mid:21", "mid:22"]);
    assert_eq!(sink.bodies(), ["help", "设备"]);
    let durable = store.current().expect("saved batch");
    assert_eq!(durable.session.get_updates_buf, "cursor-after-full-batch");
    assert_eq!(
        durable.session.context_token.as_deref(),
        Some("context-second")
    );
    assert_eq!(durable.session.recent_message_keys, ["mid:21", "mid:22"]);
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn sink_commit_then_secret_save_failure_replays_exact_without_duplicate() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    store.fail_next_saves(1);
    let sink = Arc::new(RecordingSink::default());
    let replay = || {
        PollAction::Response(response(json!({
            "ret": 0,
            "get_updates_buf": "cursor-after-exact-replay",
            "msgs": [{"message_id": 30, "from_user_id": "owner", "message_type": 1, "message_state": 2, "context_token": "context-after-replay", "item_list": [{"type": 1, "text_item": {"text": "设备"}}]}]
        })))
    };
    let transport = Arc::new(FakeTransport::with_polls([replay(), replay()]));
    let runtime = runtime_with_sink(
        transport,
        Arc::clone(&store),
        sink.clone(),
        Arc::new(RecordingHook::default()),
    );

    runtime.start_loaded().await.expect("start");
    wait_until(|| !store.saved().is_empty()).await;
    assert_eq!(sink.attempts(), ["mid:30", "mid:30"]);
    assert_eq!(sink.bodies(), ["设备"]);
    let saved = store.current().expect("saved bundle");
    assert_eq!(saved.session.get_updates_buf, "cursor-after-exact-replay");
    assert_eq!(
        saved.session.context_token.as_deref(),
        Some("context-after-replay")
    );
    assert_eq!(saved.session.recent_message_keys, ["mid:30"]);
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn commit_crash_window_replays_exact_after_restart() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    let sink = Arc::new(RecordingSink::default());
    sink.cancel_once_after_commit();
    let replay = || {
        PollAction::Response(response(json!({
            "ret": 0,
            "get_updates_buf": "cursor-after-crash-replay",
            "msgs": [{"message_id": 40, "from_user_id": "owner", "message_type": 1, "message_state": 2, "item_list": [{"type": 1, "text_item": {"text": "help"}}]}]
        })))
    };
    let transport = Arc::new(FakeTransport::with_polls([replay(), replay()]));
    let runtime = runtime_with_sink(
        transport,
        Arc::clone(&store),
        sink.clone(),
        Arc::new(RecordingHook::default()),
    );

    runtime.start_loaded().await.expect("first start");
    wait_until(|| sink.attempts().len() == 1).await;
    assert!(store.saved().is_empty());
    assert_eq!(
        store.current().expect("initial").session.get_updates_buf,
        "cursor-1"
    );

    runtime.start_loaded().await.expect("restart");
    wait_until(|| !store.saved().is_empty()).await;
    assert_eq!(sink.attempts(), ["mid:40", "mid:40"]);
    assert_eq!(sink.bodies(), ["help"]);
    assert_eq!(
        store.current().expect("saved").session.get_updates_buf,
        "cursor-after-crash-replay"
    );
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn cancellation_during_sink_accept_does_not_save_bundle_progress() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    let sink = Arc::new(CancellationSink {
        started: Notify::new(),
    });
    let transport = Arc::new(FakeTransport::with_polls([PollAction::Response(response(
        json!({
            "ret": 0,
            "get_updates_buf": "cursor-cancelled",
            "msgs": [{"message_id": 50, "from_user_id": "owner", "message_type": 1, "message_state": 2, "context_token": "context-cancelled", "item_list": [{"type": 1, "text_item": {"text": "help"}}]}]
        }),
    ))]));
    let runtime = runtime_with_sink(
        transport,
        Arc::clone(&store),
        sink.clone(),
        Arc::new(RecordingHook::default()),
    );

    runtime.start_loaded().await.expect("start");
    timeout(Duration::from_secs(1), sink.started.notified())
        .await
        .expect("sink started");
    timeout(Duration::from_secs(1), runtime.stop())
        .await
        .expect("stop did not deadlock")
        .expect("stop");
    assert!(store.saved().is_empty());
    let durable = store.current().expect("initial bundle");
    assert_eq!(durable.session.get_updates_buf, "cursor-1");
    assert!(durable.session.context_token.is_none());
    assert!(durable.session.recent_message_keys.is_empty());
}

#[tokio::test]
async fn recent_key_replay_is_validated_and_conflict_cannot_advance_cursor() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    let sink = Arc::new(RecordingSink::default());
    let oversized_a = "oversized-a".repeat(500);
    let oversized_b = "oversized-b".repeat(500);
    let transport = Arc::new(FakeTransport::with_polls([
        PollAction::Response(response(json!({
            "ret": 0,
            "get_updates_buf": "cursor-committed",
            "msgs": [{"message_id": 60, "from_user_id": "owner", "message_type": 1, "message_state": 2, "context_token": "context-committed", "item_list": [{"type": 1, "text_item": {"text": oversized_a}}]}]
        }))),
        PollAction::Response(response(json!({
            "ret": 0,
            "get_updates_buf": "cursor-conflicting",
            "msgs": [{"message_id": 60, "from_user_id": "owner", "message_type": 1, "message_state": 2, "context_token": "context-conflicting", "item_list": [{"type": 1, "text_item": {"text": oversized_b}}]}]
        }))),
    ]));
    let runtime = runtime_with_sink(
        transport,
        Arc::clone(&store),
        sink.clone(),
        Arc::new(RecordingHook::default()),
    );

    runtime.start_loaded().await.expect("start");
    wait_until(|| sink.attempts().len() == 2).await;
    assert_eq!(store.saved().len(), 1);
    let durable = store.current().expect("first commit");
    assert_eq!(durable.session.get_updates_buf, "cursor-committed");
    assert_eq!(
        durable.session.context_token.as_deref(),
        Some("context-committed")
    );
    assert_eq!(durable.session.recent_message_keys, ["mid:60"]);
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn exact_recent_key_replay_is_accepted_before_cursor_advances() {
    let store = Arc::new(MemoryStore::new(Some(bundle("owner", None))));
    let sink = Arc::new(RecordingSink::default());
    let message = |cursor: &str| {
        PollAction::Response(response(json!({
            "ret": 0,
            "get_updates_buf": cursor,
            "msgs": [{"message_id": 61, "from_user_id": "owner", "message_type": 1, "message_state": 2, "item_list": [{"type": 1, "text_item": {"text": "help"}}]}]
        })))
    };
    let transport = Arc::new(FakeTransport::with_polls([
        message("cursor-first"),
        message("cursor-exact-replay"),
    ]));
    let runtime = runtime_with_sink(
        transport,
        Arc::clone(&store),
        sink.clone(),
        Arc::new(RecordingHook::default()),
    );

    runtime.start_loaded().await.expect("start");
    wait_until(|| store.saved().len() == 2).await;
    assert_eq!(sink.attempts(), ["mid:61", "mid:61"]);
    assert_eq!(sink.bodies(), ["help"]);
    let durable = store.current().expect("exact replay save");
    assert_eq!(durable.session.get_updates_buf, "cursor-exact-replay");
    assert_eq!(durable.session.recent_message_keys, ["mid:61"]);
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn sender_can_fail_closed_without_deleting_the_bundle() {
    let store = Arc::new(MemoryStore::new(None));
    let transport = Arc::new(FakeTransport::default());
    let hook = Arc::new(RecordingHook::default());
    let runtime = runtime(transport, store, hook);
    runtime
        .replace_bundle(bundle("owner", Some("context")))
        .await
        .expect("start");
    wait_until(|| runtime.handle().safe_status().activated).await;

    let handle = runtime.handle();
    handle.mark_reconnect_required(ERROR_AUTHENTICATION_EXPIRED);
    assert_eq!(
        handle.safe_status(),
        MonitorStatus {
            activated: false,
            monitor: MonitorState::ReconnectRequired,
            last_inbound_at: None,
            last_poll_at: None,
            error_code: Some(ERROR_AUTHENTICATION_EXPIRED),
        }
    );
    assert!(handle.snapshot_bundle().is_some());
    runtime.stop().await.expect("stop");
}
