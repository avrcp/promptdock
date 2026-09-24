use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{
        HeaderMap, Method, Request, StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE},
    },
};
use promptdock_server::{
    api,
    auth::DeviceScope,
    config::{DatabaseConfig, ResultsConfig, ServerConfig},
    db,
    inbound::{
        EphemeralInboundText, InboundAcceptOutcome, InboundMessageError, InboundMessageSink,
    },
    outbox::{BundleContentCipher, OutboxService},
    results::ResultService,
    secret_store::{SecretStore, SecretStoreError},
    shutdown::TaskSupervisor,
    state::AppState,
    wechat_monitor::{NoopMonitorActivationHook, WechatMonitorRuntime},
};
use relay_provider_wechat::{
    credentials::{
        ConnectionBundle, WECHAT_SECRET_SCHEMA_VERSION, WechatCredentials, WechatSessionSecrets,
    },
    http_client::WechatHttpClient,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

struct TestSecretStore;

#[async_trait]
impl SecretStore for TestSecretStore {
    async fn load(&self) -> Result<Option<ConnectionBundle>, SecretStoreError> {
        Ok(None)
    }

    async fn save(&self, _bundle: &ConnectionBundle) -> Result<(), SecretStoreError> {
        Ok(())
    }

    async fn clear(&self) -> Result<(), SecretStoreError> {
        Ok(())
    }
}

struct TestInboundSink;

#[async_trait]
impl InboundMessageSink for TestInboundSink {
    async fn accept(
        &self,
        _message: EphemeralInboundText,
        _cancellation: &CancellationToken,
    ) -> Result<InboundAcceptOutcome, InboundMessageError> {
        Ok(InboundAcceptOutcome::Discarded)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis()
        .try_into()
        .expect("milliseconds fit i64")
}

fn publication(id: &str, body: String) -> Value {
    let now = now_ms();
    json!({
        "schemaVersion": 1,
        "resultId": id,
        "dedupeKey": format!("dedupe-{id}"),
        "kind": "run_completed",
        "contentMode": "full_final",
        "source": "codex_stop",
        "correlationKey": format!("run-{id}"),
        "resultRevision": "1",
        "title": "Codex final result",
        "body": body,
        "sourceHash": "",
        "createdAt": now,
        "notificationExpiresAt": now + 60_000,
        "startedAt": now - 1_000,
        "completedAt": now,
        "durationMs": 1_000,
    })
}

fn with_hash(mut publication: Value) -> Value {
    let body = publication["body"].as_str().expect("body");
    publication["sourceHash"] = json!(format!("{:x}", Sha256::digest(body.as_bytes())));
    publication
}

fn authenticated(method: Method, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("JSON request")))
        .expect("request")
}

async fn response(app: Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 512 * 1024)
        .await
        .expect("response body");
    (status, headers, body.to_vec())
}

async fn json_response(app: Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let (status, headers, body) = response(app, request).await;
    let value = serde_json::from_slice(&body).expect("JSON response");
    (status, headers, value)
}

async fn app_with_target() -> (
    tempfile::TempDir,
    AppState,
    Router,
    String,
    String,
    SqlitePool,
) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pool = db::open(&DatabaseConfig {
        path: directory.path().join("relay.db"),
        ..DatabaseConfig::default()
    })
    .await
    .expect("database");
    let supervisor = TaskSupervisor::new();
    let bundle = ConnectionBundle::new(
        WechatCredentials {
            schema_version: WECHAT_SECRET_SCHEMA_VERSION,
            bot_token: "test-bot-token".into(),
            account_id: "bot@im.bot".into(),
            user_id: "result-target@im.wechat".into(),
            base_url: "https://ilinkai.weixin.qq.com".into(),
            saved_at: 1,
        },
        WechatSessionSecrets::default(),
    )
    .expect("connection bundle");
    let monitor = WechatMonitorRuntime::new_with_bundle(
        WechatHttpClient::production("PromptDockRelay/result-pages-http").expect("client"),
        Arc::new(TestSecretStore),
        Arc::new(TestInboundSink),
        Arc::new(NoopMonitorActivationHook),
        Some(bundle),
    )
    .expect("monitor");
    let cipher = BundleContentCipher::new_for_test([73; 32]);
    let outbox = OutboxService::new(pool.clone()).with_bundle_cipher(cipher.clone());
    let mut state = AppState::new_with_services(
        pool.clone(),
        supervisor,
        outbox,
        None,
        Some(monitor.handle()),
    );
    state.results = ResultService::new(
        pool.clone(),
        ResultsConfig {
            enabled: true,
            public_origin: Some("https://results.example.test".into()),
            ..ResultsConfig::default()
        },
        Some(cipher),
    );
    state.mark_ready();
    let first = state
        .device_auth
        .create_device("RESULT OWNER", &DeviceScope::ALL)
        .await
        .expect("owner")
        .token
        .expose()
        .to_owned();
    let second = state
        .device_auth
        .create_device("OTHER DEVICE", &DeviceScope::ALL)
        .await
        .expect("other")
        .token
        .expose()
        .to_owned();
    let app = api::router(state.clone(), &ServerConfig::default());
    (directory, state, app, first, second, pool)
}

#[tokio::test]
async fn post_boundary_and_idempotent_replay_keep_the_original_receipt() {
    let (_directory, state, app, owner, _other, pool) = app_with_target().await;
    let body = "x".repeat(64 * 1024 - 1_024);
    let request = with_hash(publication("http-large-1", body));
    let (status, _, receipt) = json_response(
        app.clone(),
        authenticated(Method::POST, "/v1/results", &owner, request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(receipt["pageState"], "available");
    assert_eq!(receipt["sourceHash"], request["sourceHash"]);

    let mut no_target = AppState::new(pool.clone(), TaskSupervisor::new());
    no_target.results = state.results.clone();
    no_target.mark_ready();
    let no_target_router = api::router(no_target, &ServerConfig::default());
    let (replay_status, _, replay) = json_response(
        no_target_router,
        authenticated(Method::POST, "/v1/results", &owner, request),
    )
    .await;
    assert_eq!(replay_status, StatusCode::ACCEPTED);
    assert_eq!(replay, receipt, "replay must precede current target lookup");

    let too_large = with_hash(publication("http-too-large-1", "x".repeat(262_145)));
    let (status, _, error) = json_response(
        app,
        authenticated(Method::POST, "/v1/results", &owner, too_large),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"]["code"], "VALIDATION_FAILED");
}

#[tokio::test]
async fn public_page_raw_head_and_errors_do_not_leak_or_execute_html() {
    let (_directory, _state, app, owner, other, pool) = app_with_target().await;
    let raw = "line one\r\n<script>alert('no')</script>\r\n[x](javascript:bad)\r\n中文🙂";
    let request = with_hash(publication("http-public-1", raw.to_owned()));
    let (status, _, _) = json_response(
        app.clone(),
        authenticated(Method::POST, "/v1/results", &owner, request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, headers, link) = json_response(
        app.clone(),
        authenticated(
            Method::GET,
            "/v1/results/http-public-1/link",
            &owner,
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[CACHE_CONTROL], "no-store");
    let path = link["url"]
        .as_str()
        .expect("link URL")
        .strip_prefix("https://results.example.test")
        .expect("configured public origin");

    let (status, headers, bytes) = response(
        app.clone(),
        Request::builder()
            .method(Method::GET)
            .uri(format!("{path}/raw"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        request["sourceHash"].as_str().unwrap()
    );
    assert_eq!(bytes, raw.as_bytes());

    let (raw_head_status, raw_head_headers, raw_head_body) = response(
        app.clone(),
        Request::builder()
            .method(Method::HEAD)
            .uri(format!("{path}/raw"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(raw_head_status, StatusCode::OK);
    assert_eq!(raw_head_headers[CONTENT_TYPE], "text/plain; charset=utf-8");
    assert!(raw_head_body.is_empty());

    let (status, headers, page) = response(
        app.clone(),
        Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[CONTENT_SECURITY_POLICY],
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'none'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    let page = String::from_utf8(page).expect("page UTF-8");
    assert!(page.contains("&lt;script&gt;"));
    assert!(!page.contains("href=\"javascript:"));

    let (head_status, head_headers, head_body) = response(
        app.clone(),
        Request::builder()
            .method(Method::HEAD)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(head_status, StatusCode::OK);
    assert_eq!(head_headers[CACHE_CONTROL], "no-store");
    assert!(head_body.is_empty());

    let (cross_status, _, cross_body) = json_response(
        app.clone(),
        authenticated(
            Method::GET,
            "/v1/results/http-public-1",
            &other,
            Value::Null,
        ),
    )
    .await;
    assert_eq!(cross_status, StatusCode::NOT_FOUND);
    assert_eq!(cross_body["error"]["code"], "NOT_FOUND");
    sqlx::query("UPDATE devices SET enabled=0 WHERE name='OTHER DEVICE'")
        .execute(&pool)
        .await
        .expect("disable isolated test device");
    let (disabled_status, _, _) = response(
        app.clone(),
        authenticated(
            Method::GET,
            "/v1/results/http-public-1",
            &other,
            Value::Null,
        ),
    )
    .await;
    assert_eq!(disabled_status, StatusCode::FORBIDDEN);
    let (bad_status, _, bad_body) = response(
        app,
        Request::builder()
            .method(Method::GET)
            .uri("/r/not-a-valid-share-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(bad_status, StatusCode::NOT_FOUND);
    let bad_body = String::from_utf8(bad_body).expect("public error UTF-8");
    assert!(!bad_body.contains("http-public-1"));
    assert!(!bad_body.contains(raw));
}

#[tokio::test]
async fn revoke_and_expiry_make_public_content_inaccessible() {
    let (_directory, _state, app, owner, _other, pool) = app_with_target().await;
    let request = with_hash(publication(
        "http-revoke-1",
        "retain this only briefly".into(),
    ));
    let (status, _, _) = json_response(
        app.clone(),
        authenticated(Method::POST, "/v1/results", &owner, request),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, _, link) = json_response(
        app.clone(),
        authenticated(
            Method::GET,
            "/v1/results/http-revoke-1/link",
            &owner,
            Value::Null,
        ),
    )
    .await;
    let path = link["url"]
        .as_str()
        .unwrap()
        .strip_prefix("https://results.example.test")
        .unwrap();
    let (status, _, receipt) = json_response(
        app.clone(),
        authenticated(
            Method::POST,
            "/v1/results/http-revoke-1/revoke",
            &owner,
            json!({"requestId":"revoke-http-1"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["pageState"], "revoked");
    let (public_status, _, _) = response(
        app.clone(),
        Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(public_status, StatusCode::NOT_FOUND);

    let request = with_hash(publication("http-expired-1", "expired body".into()));
    let (status, _, _) = json_response(
        app.clone(),
        authenticated(Method::POST, "/v1/results", &owner, request),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, _, link) = json_response(
        app.clone(),
        authenticated(
            Method::GET,
            "/v1/results/http-expired-1/link",
            &owner,
            Value::Null,
        ),
    )
    .await;
    let path = link["url"]
        .as_str()
        .unwrap()
        .strip_prefix("https://results.example.test")
        .unwrap()
        .to_owned();
    sqlx::query("UPDATE results SET page_expires_at=?1 WHERE result_id='http-expired-1'")
        .bind(now_ms() - 1)
        .execute(&pool)
        .await
        .expect("expire result in isolated SQLite");
    let (status, _, receipt) = json_response(
        app.clone(),
        authenticated(
            Method::GET,
            "/v1/results/http-expired-1",
            &owner,
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["pageState"], "expired");
    let (public_status, _, _) = response(
        app,
        Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(public_status, StatusCode::NOT_FOUND);
}
