use std::{
    convert::Infallible,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    http::{Method, Request, StatusCode, header::ACCESS_CONTROL_ALLOW_ORIGIN},
    middleware,
    routing::{get, post},
};
use relay_provider_wechat::protocol::{
    GetBotQrCodeResponse, GetQrCodeStatusResponse, QrCodeStatus,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::{
    config::{AdminConfig, AdminMode, Config, DatabaseConfig, ResultsConfig, RetentionConfig},
    db,
    outbox::BundleContentCipher,
    qr_login::{LoginState, PollInput, QrLoginProvider, QrLoginProviderError},
    results::{ResultPublicationV1, ResultService},
    retention::RetentionService,
    shutdown::TaskSupervisor,
    state::AppState,
    wechat::secret_store::EncryptedFileSecretStore,
    wechat_login::WechatLoginService,
};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::*;
use crate::admin::{RuntimeHealthRegistry, SafeAdminRuntimeConfig, WorkerState};

async fn test_state(mode: AdminMode) -> (tempfile::TempDir, AdminState) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = DatabaseConfig {
        path: directory.path().join("relay.db"),
        ..DatabaseConfig::default()
    };
    let pool = db::open(&database).await.expect("database");
    let retention = RetentionService::new(pool.clone(), RetentionConfig::default());
    let app = AppState::new(pool, TaskSupervisor::new());
    let config = AdminConfig {
        mode,
        ..AdminConfig::default()
    };
    let full_config = Config {
        admin: config,
        ..Config::default()
    };
    let state = AdminState::new(
        app,
        &full_config,
        retention,
        RuntimeHealthRegistry::default(),
    )
    .expect("admin state");
    (directory, state)
}

struct UnusedProvider;

#[async_trait]
impl QrLoginProvider for UnusedProvider {
    async fn fetch_qr(
        &self,
        _local_token_list: &[String],
        _cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<GetBotQrCodeResponse, QrLoginProviderError> {
        Ok(GetBotQrCodeResponse {
            qrcode: "ADMIN_SENTINEL_QR_TOKEN_DO_NOT_LOG".to_owned(),
            qrcode_img_content: "ADMIN_SENTINEL_QR_CONTENT".to_owned(),
        })
    }

    async fn poll_status(
        &self,
        _input: &PollInput,
    ) -> Result<GetQrCodeStatusResponse, QrLoginProviderError> {
        Ok(GetQrCodeStatusResponse {
            status: QrCodeStatus::NeedVerifyCode,
            bot_token: None,
            ilink_bot_id: None,
            baseurl: None,
            ilink_user_id: None,
            redirect_host: None,
        })
    }
}

fn enable_wechat(state: &mut AdminState, directory: &tempfile::TempDir) {
    let supervisor = TaskSupervisor::new();
    state.app.wechat_login = Some(WechatLoginService::for_test(
        EncryptedFileSecretStore::new_for_test(
            directory.path().join("admin-wechat-connection.enc"),
            [8; 32],
        ),
        None,
        state.app.device_auth.clone(),
        supervisor,
        Arc::new(UnusedProvider),
    ));
    state.config = Arc::new(SafeAdminRuntimeConfig {
        wechat_enabled: true,
        ..(*state.config).clone()
    });
}

async fn response_json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

async fn publish_result_fixture(state: &mut AdminState, result_id: &str, body: &str) -> Uuid {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock after Unix epoch")
            .as_millis(),
    )
    .expect("test clock fits i64");
    let owner = Uuid::new_v4();
    sqlx::query("INSERT INTO devices(id,name,token_hash,enabled,created_at) VALUES(?1,'RESULT-OWNER',?2,1,?3)")
        .bind(owner.to_string())
        .bind(vec![9_u8; 32])
        .bind(1_i64)
        .execute(&state.app.db)
        .await
        .expect("result owner");
    state.app.results = ResultService::new(
        state.app.db.clone(),
        ResultsConfig {
            enabled: true,
            public_origin: Some("https://relay.example.test".to_owned()),
            ..ResultsConfig::default()
        },
        Some(BundleContentCipher::new([5; 32])),
    );
    state
        .app
        .results
        .publish(
            owner,
            ResultPublicationV1 {
                schema_version: 1,
                result_id: result_id.to_owned(),
                dedupe_key: format!("dedupe-{result_id}"),
                kind: "run_completed".to_owned(),
                content_mode: "full_final".to_owned(),
                source: "codex_stop".to_owned(),
                correlation_key: "safe-correlation".to_owned(),
                result_revision: "1".to_owned(),
                title: "Codex 最终回答".to_owned(),
                body: body.to_owned(),
                source_hash: format!("{:x}", Sha256::digest(body.as_bytes())),
                created_at: now,
                notification_expires_at: now + 86_400_000,
                started_at: Some(now - 1),
                completed_at: Some(now),
                duration_ms: Some(1),
            },
            Some(format!("wx:{}", "a".repeat(64))),
        )
        .await
        .expect("result publish");
    owner
}

#[tokio::test]
async fn read_endpoints_are_strict_paginated_and_restart_local() {
    let (_directory, state) = test_state(AdminMode::ReadOnly).await;
    let first_id = "00000000-0000-4000-8000-000000000001";
    let second_id = "00000000-0000-4000-8000-000000000002";
    for (id, name, created_at) in [(first_id, "first", 20_i64), (second_id, "second", 10_i64)] {
        sqlx::query(
            "INSERT INTO devices(id, name, token_hash, enabled, credential_version, created_at) \
             VALUES (?1, ?2, ?3, 1, 2, ?4)",
        )
        .bind(id)
        .bind(name)
        .bind(vec![7_u8; 32])
        .bind(created_at)
        .execute(&state.app.db)
        .await
        .expect("device fixture");
    }
    let app = router(state.clone());
    for path in [
        "/admin/api/v2/overview",
        "/admin/api/v2/wechat/status",
        "/admin/api/v2/wechat/events",
        "/admin/api/v2/deliveries",
        "/admin/api/v2/interactive-replies",
        "/admin/api/v2/inbound-commands",
        "/admin/api/v2/system",
        "/admin/api/v2/devices/00000000-0000-4000-8000-000000000001",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body = response_json(response).await;
        let serialized = serde_json::to_string(&body).expect("response JSON");
        for forbidden in [
            "token_hash",
            "07070707",
            "message_key",
            "provider_message_id",
        ] {
            assert!(!serialized.contains(forbidden), "{path} leaked {forbidden}");
        }
    }

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/devices?limit=1")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(first.status(), StatusCode::OK);
    let first = response_json(first).await;
    assert_eq!(first["items"][0]["id"], first_id);
    let cursor = first["nextCursor"].as_str().expect("next cursor");
    assert!(!cursor.contains(first_id));

    let second = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/api/v2/devices?limit=1&cursor={cursor}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(second.status(), StatusCode::OK);
    let second = response_json(second).await;
    assert_eq!(second["items"][0]["id"], second_id);
    assert!(second["nextCursor"].is_null());

    let config = Config::default();
    let restarted = AdminState::new(
        state.app.clone(),
        &config,
        RetentionService::new(state.app.db.clone(), RetentionConfig::default()),
        RuntimeHealthRegistry::default(),
    )
    .expect("restarted admin state");
    let response = router(restarted)
        .oneshot(
            Request::builder()
                .uri(format!("/admin/api/v2/devices?cursor={cursor}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(response).await["code"],
        "ADMIN_CURSOR_INVALID"
    );
}

#[tokio::test]
async fn meta_and_errors_have_safe_headers_and_server_request_ids() {
    let (_directory, state) = test_state(AdminMode::ReadOnly).await;
    let app = router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/meta")
                .header(X_REQUEST_ID.clone(), "client-request-id")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store, max-age=0");
    assert_eq!(response.headers()[PRAGMA], "no-cache");
    assert_eq!(response.headers()[VARY], "Authorization");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(
        response
            .headers()
            .get(ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
    let response_id = response.headers()[X_REQUEST_ID.clone()]
        .to_str()
        .expect("request id")
        .to_owned();
    assert_ne!(response_id, "client-request-id");
    Uuid::parse_str(&response_id).expect("UUID request id");
    let meta = response_json(response).await;
    assert_eq!(meta["schemaVersion"], 2);
    assert_eq!(meta["adminApiVersion"], 2);
    assert_eq!(meta["relayVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        meta["capabilities"],
        json!(["admin_read_v2", "admin_wechat_status_v2"])
    );
    assert!(meta["generatedAt"].as_i64().is_some_and(|value| value > 0));

    let not_found = app
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/missing?sentinel=not-logged")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(not_found.status(), StatusCode::NOT_FOUND);
    let response_id = not_found.headers()[X_REQUEST_ID.clone()]
        .to_str()
        .expect("request id")
        .to_owned();
    let error = response_json(not_found).await;
    assert_eq!(
        error,
        json!({
            "code": "ADMIN_NOT_FOUND",
            "message": "resource not found",
            "requestId": response_id,
        })
    );
    assert!(error.get("error").is_none());
}

#[tokio::test]
async fn framework_failures_are_normalized_without_changing_public_contracts() {
    let (_directory, state) = test_state(AdminMode::Operator).await;
    let test_routes = Router::new()
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                StatusCode::NO_CONTENT
            }),
        )
        .route("/panic", get(panic_handler))
        .route(
            "/echo",
            post(|_body: String| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/admin/api/v2/ordinary-at-limit",
            get(|| async { streaming_response(&[ADMIN_RESPONSE_BODY_BYTES]) }),
        )
        .route(
            "/admin/api/v2/ordinary-stream-over",
            get(|| async { streaming_response(&[ADMIN_RESPONSE_BODY_BYTES, 1]) }),
        )
        .route(
            "/admin/api/v2/wechat/login",
            get(|| async { streaming_response(&[ADMIN_RESPONSE_BODY_BYTES + 1]) }),
        )
        .route(
            "/admin/api/v2/wechat/login/{login_id}",
            get(|| async { streaming_response(&[ADMIN_QR_RESPONSE_BODY_BYTES]) }),
        )
        .route(
            "/admin/api/v2/wechat/login/{login_id}/verify",
            post(|| async { streaming_response(&[ADMIN_QR_RESPONSE_BODY_BYTES, 1]) }),
        )
        .route(
            "/admin/api/v2/wechat/login/{login_id}/unexpected",
            get(|| async { streaming_response(&[ADMIN_RESPONSE_BODY_BYTES, 1]) }),
        )
        .route(
            "/body-slow",
            get(|| async { delayed_streaming_response(Duration::from_secs(2)) }),
        )
        .fallback(not_found)
        .with_state(state.clone());
    let mut short_timeout_state = state;
    short_timeout_state.config = std::sync::Arc::new(SafeAdminRuntimeConfig {
        route_timeout_seconds: 1,
        ..(*short_timeout_state.config).clone()
    });
    let app = apply_http_layers(test_routes, short_timeout_state);

    let too_large = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/echo")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(vec![b'x'; ADMIN_REQUEST_BODY_BYTES + 1]))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response_json(too_large).await["code"],
        "ADMIN_BODY_TOO_LARGE"
    );

    let ordinary_at_limit = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/ordinary-at-limit")
                .body(Body::empty())
                .expect("ordinary limit request"),
        )
        .await
        .expect("ordinary limit response");
    assert_eq!(ordinary_at_limit.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(ordinary_at_limit.into_body(), ADMIN_RESPONSE_BODY_BYTES + 1)
            .await
            .expect("ordinary response bytes")
            .len(),
        ADMIN_RESPONSE_BODY_BYTES
    );

    let qr_between_limits = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/wechat/login")
                .body(Body::empty())
                .expect("QR response request"),
        )
        .await
        .expect("QR response");
    assert_eq!(qr_between_limits.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(qr_between_limits.into_body(), ADMIN_QR_RESPONSE_BODY_BYTES)
            .await
            .expect("QR response bytes")
            .len(),
        ADMIN_RESPONSE_BODY_BYTES + 1
    );

    let qr_at_limit = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/wechat/login/00000000-0000-0000-0000-000000000001")
                .body(Body::empty())
                .expect("QR limit request"),
        )
        .await
        .expect("QR limit response");
    assert_eq!(qr_at_limit.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(qr_at_limit.into_body(), ADMIN_QR_RESPONSE_BODY_BYTES + 1)
            .await
            .expect("QR limit bytes")
            .len(),
        ADMIN_QR_RESPONSE_BODY_BYTES
    );

    for (method, path) in [
        (Method::GET, "/admin/api/v2/ordinary-stream-over"),
        (
            Method::POST,
            "/admin/api/v2/wechat/login/00000000-0000-0000-0000-000000000001/verify",
        ),
        (
            Method::GET,
            "/admin/api/v2/wechat/login/00000000-0000-0000-0000-000000000001/unexpected",
        ),
    ] {
        let oversized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .expect("oversized response request"),
            )
            .await
            .expect("oversized response");
        assert_eq!(oversized.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(oversized.headers()[CACHE_CONTROL], "no-store, max-age=0");
        assert_eq!(oversized.headers()[PRAGMA], "no-cache");
        assert_eq!(oversized.headers()[VARY], "Authorization");
        assert_eq!(oversized.headers()["x-content-type-options"], "nosniff");
        let header_request_id = oversized.headers()[X_REQUEST_ID.clone()]
            .to_str()
            .expect("response request id")
            .to_owned();
        Uuid::parse_str(&header_request_id).expect("UUID response request id");
        let error = response_json(oversized).await;
        assert_eq!(error["code"], "ADMIN_INTERNAL");
        assert_eq!(error["requestId"], header_request_id);
    }

    tokio::time::pause();
    let slow = tokio::spawn(
        app.clone().oneshot(
            Request::builder()
                .uri("/slow")
                .body(Body::empty())
                .expect("request"),
        ),
    );
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    let timeout = slow.await.expect("task").expect("response");
    assert_eq!(timeout.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(response_json(timeout).await["code"], "ADMIN_TIMEOUT");

    let slow_body = tokio::spawn(
        app.clone().oneshot(
            Request::builder()
                .uri("/body-slow")
                .body(Body::empty())
                .expect("request"),
        ),
    );
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    let timeout = slow_body.await.expect("task").expect("response");
    assert_eq!(timeout.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(response_json(timeout).await["code"], "ADMIN_TIMEOUT");

    let panic = app
        .oneshot(
            Request::builder()
                .uri("/panic")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(panic.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response_json(panic).await["code"], "ADMIN_INTERNAL");
}

async fn panic_handler() -> StatusCode {
    panic!("sentinel panic")
}

fn streaming_response(chunk_lengths: &[usize]) -> Response {
    let chunks = chunk_lengths
        .iter()
        .map(|length| Ok::<_, Infallible>(Bytes::from(vec![b'x'; *length])))
        .collect::<Vec<_>>();
    Response::builder()
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from_stream(futures_util::stream::iter(chunks)))
        .expect("streaming response")
}

fn delayed_streaming_response(delay: Duration) -> Response {
    let body = futures_util::stream::once(async move {
        tokio::time::sleep(delay).await;
        Ok::<_, Infallible>(Bytes::from_static(b"{}"))
    });
    Response::builder()
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from_stream(body))
        .expect("delayed streaming response")
}

fn mutation_app(state: AdminState) -> Router {
    Router::new()
        .route("/read", get(|| async { StatusCode::NO_CONTENT }))
        .route("/write", post(|| async { StatusCode::NO_CONTENT }))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_mutation_boundary,
        ))
        .with_state(state)
}

fn mutation_request() -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/write")
        .header(ORIGIN, "http://127.0.0.1:5173")
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header(X_ADMIN_ACTION.clone(), "1")
        .header(SEC_FETCH_SITE.clone(), "same-origin")
        .body(Body::from("{}"))
        .expect("request")
}

fn admin_mutation(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(ORIGIN, "http://127.0.0.1:5173")
        .header(CONTENT_TYPE, "application/json")
        .header(X_ADMIN_ACTION.clone(), "1")
        .header(SEC_FETCH_SITE.clone(), "same-origin")
        .body(Body::from(serde_json::to_vec(&body).expect("request JSON")))
        .expect("request")
}

fn admin_delete(path: &str, body: Option<Value>) -> Request<Body> {
    let mut request = Request::builder()
        .method(Method::DELETE)
        .uri(path)
        .header(ORIGIN, "http://127.0.0.1:5173")
        .header(X_ADMIN_ACTION.clone(), "1")
        .header(SEC_FETCH_SITE.clone(), "same-origin");
    if body.is_some() {
        request = request.header(CONTENT_TYPE, "application/json");
    }
    request
        .body(Body::from(
            body.map(|value| serde_json::to_vec(&value).expect("request JSON"))
                .unwrap_or_default(),
        ))
        .expect("request")
}

#[tokio::test]
async fn operator_device_and_retention_routes_are_typed_and_read_only_never_mutates() {
    let (_directory, read_only) = test_state(AdminMode::ReadOnly).await;
    let create_body = json!({
        "name": "OFFICE-PC",
        "scopes": [
            "notify:write",
            "notify:read_own",
            "channel:read",
            "channel:manage",
            "gateway:connect",
            "job:query"
        ]
    });
    let response = router(read_only.clone())
        .oneshot(admin_mutation("/admin/api/v2/devices", create_body.clone()))
        .await
        .expect("read-only response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response_json(response).await["code"], "ADMIN_READ_ONLY");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
        .fetch_one(&read_only.app.db)
        .await
        .expect("device count");
    assert_eq!(count, 0);

    let (_directory, operator) = test_state(AdminMode::Operator).await;
    let app = router(operator.clone());
    let meta = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/meta")
                .body(Body::empty())
                .expect("meta request"),
        )
        .await
        .expect("meta response");
    assert_eq!(
        response_json(meta).await["capabilities"],
        json!([
            "admin_read_v2",
            "admin_device_manage_v2",
            "admin_wechat_status_v2",
            "admin_maintenance_v2",
            "admin_results_manage_v2"
        ])
    );

    let created = app
        .clone()
        .oneshot(admin_mutation("/admin/api/v2/devices", create_body.clone()))
        .await
        .expect("create response");
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(created.headers()[CACHE_CONTROL], "no-store, max-age=0");
    let created = response_json(created).await;
    let device_id = created["deviceId"]
        .as_str()
        .expect("created device id")
        .to_owned();
    let first_token = created["oneTimeToken"]
        .as_str()
        .expect("one-time token")
        .to_owned();
    assert!(first_token.starts_with("pdv2."));
    assert_eq!(created["action"], "create");
    assert_eq!(created.as_object().expect("receipt object").len(), 5);
    assert!(created.get("scopes").is_none());

    let rotated = app
        .clone()
        .oneshot(admin_mutation(
            &format!("/admin/api/v2/devices/{device_id}/rotate"),
            json!({}),
        ))
        .await
        .expect("rotate response");
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated = response_json(rotated).await;
    assert_eq!(rotated["action"], "rotate");
    assert_ne!(rotated["oneTimeToken"], first_token);

    for (suffix, action) in [
        ("disable", "device.disable"),
        ("enable", "device.enable"),
        ("revoke", "device.revoke"),
    ] {
        let response = app
            .clone()
            .oneshot(admin_mutation(
                &format!("/admin/api/v2/devices/{device_id}/{suffix}"),
                json!({}),
            ))
            .await
            .expect("device action response");
        assert_eq!(response.status(), StatusCode::OK, "{suffix}");
        let receipt = response_json(response).await;
        assert_eq!(receipt["action"], action);
        assert_eq!(receipt.as_object().expect("action receipt").len(), 3);
    }

    let conflict = app
        .clone()
        .oneshot(admin_mutation(
            &format!("/admin/api/v2/devices/{device_id}/enable"),
            json!({}),
        ))
        .await
        .expect("conflict response");
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(conflict).await["code"],
        "ADMIN_DEVICE_STATE_CONFLICT"
    );

    let retention = app
        .clone()
        .oneshot(admin_mutation(
            "/admin/api/v2/maintenance/retention",
            json!({}),
        ))
        .await
        .expect("retention response");
    assert_eq!(retention.status(), StatusCode::OK);
    let retention = response_json(retention).await;
    assert_eq!(retention["action"], "maintenance.retention");
    for field in [
        "outboxDeleted",
        "inboundDeleted",
        "selectionDeleted",
        "startedAt",
        "completedAt",
    ] {
        assert!(retention[field].as_i64().is_some(), "missing {field}");
    }
    let system = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/system")
                .body(Body::empty())
                .expect("system request"),
        )
        .await
        .expect("system response");
    let system = response_json(system).await;
    assert_eq!(system["retention"]["lastResult"], "success");
    assert!(system["retention"]["lastRunAt"].as_i64().is_some());
    assert_eq!(
        system["database"]["lastRetentionPassAt"],
        system["retention"]["lastRunAt"]
    );

    operator
        .health
        .fail(WorkerKey::Retention, "RETENTION_WORKER_STOPPED");
    operator
        .health
        .set_state(WorkerKey::PublicHttp, WorkerState::Running);
    operator
        .health
        .set_state(WorkerKey::Outbox, WorkerState::Running);
    let failed_system = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/system")
                .body(Body::empty())
                .expect("failed system request"),
        )
        .await
        .expect("failed system response");
    let failed_system = response_json(failed_system).await;
    assert_eq!(failed_system["retention"]["lastResult"], "failed");
    assert_eq!(
        failed_system["database"]["lastRetentionPassAt"],
        failed_system["retention"]["lastRunAt"]
    );
    let workers = failed_system["workers"].as_array().expect("workers");
    let worker = |name: &str| {
        workers
            .iter()
            .find(|worker| worker["name"] == name)
            .expect("named worker")
    };
    assert_eq!(worker("outbox-worker")["detail"], "heartbeat_not_observed");
    assert!(worker("public-http")["detail"].is_null());

    let unknown = app
        .clone()
        .oneshot(admin_mutation(
            "/admin/api/v2/devices",
            json!({ "name": "unsafe", "scopes": ["root"], "secret": "sentinel" }),
        ))
        .await
        .expect("invalid response");
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(unknown).await["code"],
        "ADMIN_VALIDATION_FAILED"
    );
}

#[tokio::test]
async fn interrupted_retention_is_not_reported_as_a_successful_pass() {
    let (_directory, state) = test_state(AdminMode::Operator).await;
    state.app.shutdown.cancel();
    let response = router(state.clone())
        .oneshot(admin_mutation(
            "/admin/api/v2/maintenance/retention",
            json!({}),
        ))
        .await
        .expect("interrupted retention response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_json(response).await["code"],
        "ADMIN_MAINTENANCE_UNAVAILABLE"
    );
    assert!(
        state
            .health
            .snapshot()
            .get(&WorkerKey::Retention)
            .and_then(|health| health.last_tick_at)
            .is_none()
    );
    assert!(state.events.snapshot().await.is_empty());
}

#[tokio::test]
async fn wechat_test_is_durable_safe_and_disconnect_fails_closed() {
    let (_directory, read_only) = test_state(AdminMode::ReadOnly).await;
    let response = router(read_only.clone())
        .oneshot(admin_mutation("/admin/api/v2/wechat/test", json!({})))
        .await
        .expect("read-only test response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response_json(response).await["code"], "ADMIN_READ_ONLY");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM notification_outbox WHERE origin_kind='admin'",
        )
        .fetch_one(&read_only.app.db)
        .await
        .expect("read-only outbox count"),
        0
    );

    let (_directory, disabled_operator) = test_state(AdminMode::Operator).await;
    let disabled_app = router(disabled_operator.clone());
    let disabled = disabled_app
        .clone()
        .oneshot(admin_mutation("/admin/api/v2/wechat/test", json!({})))
        .await
        .expect("disabled operator response");
    assert_eq!(disabled.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_json(disabled).await["code"],
        "ADMIN_WECHAT_UNAVAILABLE"
    );
    let disabled_disconnect = disabled_app
        .oneshot(admin_mutation(
            "/admin/api/v2/wechat/disconnect",
            json!({"reason": "operator requested"}),
        ))
        .await
        .expect("disabled disconnect response");
    assert_eq!(
        disabled_disconnect.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        response_json(disabled_disconnect).await["code"],
        "ADMIN_WECHAT_UNAVAILABLE"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM notification_outbox WHERE origin_kind='admin'",
        )
        .fetch_one(&disabled_operator.app.db)
        .await
        .expect("disabled operator outbox count"),
        0
    );
    assert!(disabled_operator.events.snapshot().await.is_empty());

    let (directory, mut operator) = test_state(AdminMode::Operator).await;
    enable_wechat(&mut operator, &directory);
    let app = router(operator.clone());
    let response = app
        .clone()
        .oneshot(admin_mutation("/admin/api/v2/wechat/test", json!({})))
        .await
        .expect("test response");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let receipt = response_json(response).await;
    assert_eq!(receipt["state"], "accepted_by_relay");
    assert!(
        receipt["receiptId"]
            .as_str()
            .is_some_and(|value| value.starts_with("wechat_test_") && value.len() == 76)
    );
    assert!(receipt["issuedAt"].as_i64().is_some_and(|value| value > 0));
    assert_eq!(receipt.as_object().expect("test receipt").len(), 3);

    let row = sqlx::query(
        "SELECT notification_id, origin_key, origin_device_id, kind,
                target_account_fingerprint, title, body
         FROM notification_outbox WHERE origin_kind='admin'",
    )
    .fetch_one(&operator.app.db)
    .await
    .expect("durable Admin test");
    let notification_id = sqlx::Row::get::<String, _>(&row, "notification_id");
    let serialized = serde_json::to_string(&receipt).expect("receipt JSON");
    assert!(!serialized.contains(&notification_id));
    assert_eq!(
        sqlx::Row::get::<String, _>(&row, "origin_key"),
        "admin:singleton"
    );
    assert_eq!(
        sqlx::Row::get::<Option<String>, _>(&row, "origin_device_id"),
        None
    );
    assert_eq!(sqlx::Row::get::<String, _>(&row, "kind"), "test");
    assert_eq!(
        sqlx::Row::get::<Option<String>, _>(&row, "target_account_fingerprint"),
        None
    );

    let deliveries = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/deliveries?origin=admin")
                .body(Body::empty())
                .expect("delivery request"),
        )
        .await
        .expect("delivery response");
    assert_eq!(deliveries.status(), StatusCode::OK);
    let deliveries = response_json(deliveries).await;
    assert_eq!(deliveries["items"][0]["origin"], "admin");
    assert_eq!(deliveries["items"][0]["originLabel"], "admin");
    assert!(
        !serde_json::to_string(&deliveries)
            .expect("delivery JSON")
            .contains(&notification_id)
    );

    let events = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/wechat/events?kinds=test")
                .body(Body::empty())
                .expect("events request"),
        )
        .await
        .expect("events response");
    let events = response_json(events).await;
    assert_eq!(events["items"][0]["kind"], "test");
    assert_eq!(
        events["items"][0]["safeMessage"],
        "WeChat test notification was queued"
    );

    let strict_test = app
        .clone()
        .oneshot(admin_mutation(
            "/admin/api/v2/wechat/test",
            json!({"body": "SENTINEL_CALLER_CONTROLLED_BODY"}),
        ))
        .await
        .expect("strict test response");
    assert_eq!(strict_test.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(strict_test).await["code"],
        "ADMIN_VALIDATION_FAILED"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM notification_outbox WHERE origin_kind='admin'",
        )
        .fetch_one(&operator.app.db)
        .await
        .expect("strict outbox count"),
        1
    );

    for reason in ["".to_owned(), "界".repeat(121)] {
        let invalid = app
            .clone()
            .oneshot(admin_mutation(
                "/admin/api/v2/wechat/disconnect",
                json!({"reason": reason}),
            ))
            .await
            .expect("invalid disconnect response");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(invalid).await["code"],
            "ADMIN_VALIDATION_FAILED"
        );
    }

    let private_reason = "SENTINEL_PRIVATE_DISCONNECT_REASON";
    let disconnected = app
        .clone()
        .oneshot(admin_mutation(
            "/admin/api/v2/wechat/disconnect",
            json!({"reason": private_reason}),
        ))
        .await
        .expect("disconnect response");
    assert_eq!(disconnected.status(), StatusCode::OK);
    let disconnected = response_json(disconnected).await;
    assert_eq!(disconnected["action"], "wechat.disconnect");
    assert!(
        disconnected["receiptId"]
            .as_str()
            .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok())
    );
    assert!(
        disconnected["completedAt"]
            .as_i64()
            .is_some_and(|value| value > 0)
    );
    assert_eq!(disconnected.as_object().expect("action receipt").len(), 3);
    assert!(
        !serde_json::to_string(&disconnected)
            .expect("receipt JSON")
            .contains(private_reason)
    );

    let events = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/wechat/events?kinds=disconnect")
                .body(Body::empty())
                .expect("disconnect events request"),
        )
        .await
        .expect("disconnect events response");
    let events = response_json(events).await;
    assert_eq!(events["items"][0]["kind"], "disconnect");
    assert_eq!(events["items"][0]["safeMessage"], "WeChat was disconnected");
    assert!(
        !serde_json::to_string(&events)
            .expect("events JSON")
            .contains(private_reason)
    );

    for query in [
        "kinds=test&kinds=disconnect",
        "kinds=test,disconnect",
        "kinds=test,disconnect&kinds=reconnect",
    ] {
        let events = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/admin/api/v2/wechat/events?{query}"))
                    .body(Body::empty())
                    .expect("multi-kind events request"),
            )
            .await
            .expect("multi-kind events response");
        assert_eq!(events.status(), StatusCode::OK);
        let events = response_json(events).await;
        let kinds = events["items"]
            .as_array()
            .expect("event items")
            .iter()
            .map(|event| event["kind"].as_str().expect("event kind"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            kinds,
            std::collections::BTreeSet::from(["disconnect", "test"])
        );
    }

    for query in ["kinds=unknown", "limit=1&limit=2", "unexpected=value"] {
        let invalid = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/admin/api/v2/wechat/events?{query}"))
                    .body(Body::empty())
                    .expect("invalid events request"),
            )
            .await
            .expect("invalid events response");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(invalid).await["code"],
            "ADMIN_VALIDATION_FAILED"
        );
    }
}

#[tokio::test]
async fn meta_advertises_wechat_management_only_for_enabled_operator_mode() {
    let (directory, mut operator) = test_state(AdminMode::Operator).await;
    enable_wechat(&mut operator, &directory);
    let meta = router(operator)
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/meta")
                .body(Body::empty())
                .expect("meta request"),
        )
        .await
        .expect("meta response");
    assert_eq!(
        response_json(meta).await["capabilities"],
        json!([
            "admin_read_v2",
            "admin_device_manage_v2",
            "admin_wechat_status_v2",
            "admin_wechat_manage_v2",
            "admin_wechat_login_v2",
            "admin_maintenance_v2",
            "admin_results_manage_v2"
        ])
    );
}

#[tokio::test]
async fn admin_login_is_direct_process_owned_and_independent_of_device_lifecycle() {
    let (directory, mut state) = test_state(AdminMode::Operator).await;
    enable_wechat(&mut state, &directory);
    let app = router(state.clone());
    let response = app
        .clone()
        .oneshot(admin_mutation(
            "/admin/api/v2/wechat/login",
            json!({"forceFresh": false}),
        ))
        .await
        .expect("start response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store, max-age=0");
    let snapshot = response_json(response).await;
    let serialized = snapshot.to_string();
    assert!(!serialized.contains("owner"));
    let login_id = snapshot["loginId"].as_str().expect("login id");

    let repeated_start = app
        .clone()
        .oneshot(admin_mutation(
            "/admin/api/v2/wechat/login",
            json!({"forceFresh": false}),
        ))
        .await
        .expect("repeat start response");
    assert_eq!(repeated_start.status(), StatusCode::OK);
    assert_eq!(response_json(repeated_start).await["loginId"], login_id);

    let poll = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/api/v2/wechat/login/{login_id}"))
                .body(Body::empty())
                .expect("poll request"),
        )
        .await
        .expect("poll response");
    assert_eq!(poll.status(), StatusCode::OK);
    let polled = response_json(poll).await;
    assert_eq!(polled["loginId"], login_id);

    let invalid_code = app
        .clone()
        .oneshot(admin_mutation(
            &format!("/admin/api/v2/wechat/login/{login_id}/verify"),
            json!({"code":"12-secret-34"}),
        ))
        .await
        .expect("invalid verify response");
    assert_eq!(invalid_code.status(), StatusCode::BAD_REQUEST);
    assert!(
        !response_json(invalid_code)
            .await
            .to_string()
            .contains("12-secret-34")
    );

    let device = state
        .app
        .device_auth
        .create_device("UNRELATED DEVICE", &crate::auth::DeviceScope::ALL)
        .await
        .expect("device");
    let rotate = app
        .clone()
        .oneshot(admin_mutation(
            &format!("/admin/api/v2/devices/{}/rotate", device.id),
            json!({}),
        ))
        .await
        .expect("rotate response");
    assert_eq!(rotate.status(), StatusCode::OK);
    assert_ne!(
        state
            .app
            .wechat_login
            .as_ref()
            .expect("login service")
            .operator_snapshot(Uuid::parse_str(login_id).expect("login UUID"))
            .expect("operator snapshot")
            .state,
        LoginState::Cancelled
    );
    let after_rotate = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/api/v2/wechat/login/{login_id}"))
                .body(Body::empty())
                .expect("post-rotate poll"),
        )
        .await
        .expect("post-rotate response");
    assert_eq!(after_rotate.status(), StatusCode::OK);
}

#[tokio::test]
async fn unavailable_admin_login_returns_a_closed_error() {
    let (_directory, state) = test_state(AdminMode::Operator).await;
    let response = router(state)
        .oneshot(admin_mutation(
            "/admin/api/v2/wechat/login",
            json!({"forceFresh": false}),
        ))
        .await
        .expect("unavailable consume response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response_json(response).await;
    assert_eq!(body["code"], "ADMIN_WECHAT_UNAVAILABLE");
}

#[tokio::test]
async fn admin_login_events_are_complete_private_and_cancel_body_is_strict() {
    let (directory, mut state) = test_state(AdminMode::Operator).await;
    enable_wechat(&mut state, &directory);
    let app = router(state.clone());
    let response = app
        .clone()
        .oneshot(admin_mutation(
            "/admin/api/v2/wechat/login",
            json!({"forceFresh": false}),
        ))
        .await
        .expect("start response");
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = response_json(response).await;
    let login_id = snapshot["loginId"].as_str().expect("login id");
    let login_uuid = Uuid::parse_str(login_id).expect("login UUID");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = state
                .app
                .wechat_login
                .as_ref()
                .expect("login service")
                .operator_snapshot(login_uuid)
                .expect("login snapshot");
            if snapshot.state == LoginState::VerifyCodeRequired {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("verify-code-required state");

    let verify_code = "9182736455463728";
    let verified = app
        .clone()
        .oneshot(admin_mutation(
            &format!("/admin/api/v2/wechat/login/{login_id}/verify"),
            json!({"code": verify_code}),
        ))
        .await
        .expect("verify response");
    assert_eq!(verified.status(), StatusCode::OK);
    assert!(
        !response_json(verified)
            .await
            .to_string()
            .contains(verify_code)
    );
    let path = format!("/admin/api/v2/wechat/login/{login_id}");

    for request in [
        admin_delete(&path, None),
        admin_delete(&path, Some(json!({"unexpected": true}))),
    ] {
        let rejected = app
            .clone()
            .oneshot(request)
            .await
            .expect("strict cancel response");
        assert!(rejected.status().is_client_error());
    }
    assert_eq!(state.events.snapshot().await.len(), 2);
    let cancelled = app
        .clone()
        .oneshot(admin_delete(&path, Some(json!({}))))
        .await
        .expect("cancel response");
    assert_eq!(cancelled.status(), StatusCode::OK);
    assert_eq!(response_json(cancelled).await["state"], "cancelled");
    let events = state.events.snapshot().await;
    assert_eq!(events.len(), 3);
    assert_eq!(
        events.iter().map(|event| event.kind).collect::<Vec<_>>(),
        vec![
            OperationalEventKind::WechatLoginStarted,
            OperationalEventKind::WechatVerifySubmitted,
            OperationalEventKind::WechatLoginCancelled,
        ]
    );
    for event in &events {
        Uuid::parse_str(&event.id).expect("event UUID");
        assert!(event.occurred_at > 0);
    }

    let event_dump = format!("{events:?}");
    let sensitive = [
        login_id.to_owned(),
        verify_code.to_owned(),
        "ADMIN_SENTINEL_QR_TOKEN_DO_NOT_LOG".to_owned(),
        "ADMIN_SENTINEL_QR_CONTENT".to_owned(),
    ];
    for sentinel in &sensitive {
        assert!(!event_dump.contains(sentinel), "event leaked {sentinel}");
    }

    let projected = app
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/wechat/events?kinds=login&kinds=login_cancelled&limit=10")
                .body(Body::empty())
                .expect("events request"),
        )
        .await
        .expect("events response");
    assert_eq!(projected.status(), StatusCode::OK);
    let projected = response_json(projected).await;
    assert_eq!(projected["items"].as_array().expect("event items").len(), 3);
    let projected_dump = projected.to_string();
    for sentinel in &sensitive {
        assert!(
            !projected_dump.contains(sentinel),
            "projection leaked {sentinel}"
        );
    }
}

#[tokio::test]
async fn mutation_boundary_is_exact_and_read_only_fails_closed() {
    let (_directory, read_only) = test_state(AdminMode::ReadOnly).await;
    let response = mutation_app(read_only)
        .oneshot(mutation_request())
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response_json(response).await["code"], "ADMIN_READ_ONLY");

    let (_directory, operator) = test_state(AdminMode::Operator).await;
    let app = mutation_app(operator);
    assert_eq!(
        app.clone()
            .oneshot(mutation_request())
            .await
            .expect("valid mutation")
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/read")
                    .body(Body::empty())
                    .expect("read request")
            )
            .await
            .expect("read response")
            .status(),
        StatusCode::NO_CONTENT
    );

    for (header, bad_value) in [
        (ORIGIN, "https://wrong.example.com"),
        (CONTENT_TYPE, "text/plain"),
        (X_ADMIN_ACTION.clone(), "0"),
        (SEC_FETCH_SITE.clone(), "cross-site"),
    ] {
        let mut request = mutation_request();
        request.headers_mut().insert(
            header,
            HeaderValue::from_str(bad_value).expect("header value"),
        );
        let response = app
            .clone()
            .oneshot(request)
            .await
            .expect("rejected mutation");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response_json(response).await["code"],
            "ADMIN_MUTATION_FORBIDDEN"
        );
    }
}

#[tokio::test]
async fn result_admin_projection_is_secret_safe_and_operator_revoke_is_owner_independent() {
    let secret_body = "ADMIN_RESULT_BODY_MUST_NOT_LEAK https://relay.example.test/r/secret-token";
    let (_directory, mut read_only) = test_state(AdminMode::ReadOnly).await;
    let owner = publish_result_fixture(&mut read_only, "result-admin-safe", secret_body).await;
    let read_only_app = router(read_only.clone());
    let list = read_only_app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/results")
                .body(Body::empty())
                .expect("list request"),
        )
        .await
        .expect("list response");
    assert_eq!(list.status(), StatusCode::OK);
    let list_json = response_json(list).await;
    let projection = list_json.to_string();
    for secret in [secret_body, "secret-token", "https://relay.example.test/r/"] {
        assert!(!projection.contains(secret), "list leaked {secret}");
    }
    assert_eq!(list_json["items"][0]["ownerDeviceId"], owner.to_string());
    let safe_row_id = list_json["items"][0]["resultRowId"]
        .as_str()
        .expect("safe result row ID")
        .to_owned();
    let detail = read_only_app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/api/v2/results/{safe_row_id}"))
                .body(Body::empty())
                .expect("detail request"),
        )
        .await
        .expect("detail response");
    assert_eq!(detail.status(), StatusCode::OK);
    let detail_json = response_json(detail).await;
    assert!(!detail_json.to_string().contains(secret_body));
    let rejected = read_only_app
        .oneshot(admin_mutation(
            &format!("/admin/api/v2/results/{safe_row_id}/revoke"),
            json!({"requestId":"readonly-revoke"}),
        ))
        .await
        .expect("readonly revoke");
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert_eq!(response_json(rejected).await["code"], "ADMIN_READ_ONLY");

    let (_directory, mut operator) = test_state(AdminMode::Operator).await;
    let first_owner =
        publish_result_fixture(&mut operator, "result-admin-cross-owner", secret_body).await;
    let second_owner =
        publish_result_fixture(&mut operator, "result-admin-cross-owner", secret_body).await;
    let app = router(operator.clone());
    let operator_list = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/results")
                .body(Body::empty())
                .expect("operator list request"),
        )
        .await
        .expect("operator list response");
    assert_eq!(operator_list.status(), StatusCode::OK);
    let operator_list = response_json(operator_list).await;
    let row_id_for = |owner: Uuid| {
        operator_list["items"]
            .as_array()
            .expect("operator result items")
            .iter()
            .find(|item| item["ownerDeviceId"] == owner.to_string())
            .and_then(|item| item["resultRowId"].as_str())
            .expect("globally unique result row ID")
            .to_owned()
    };
    let first_row_id = row_id_for(first_owner);
    let second_row_id = row_id_for(second_owner);
    assert_ne!(first_row_id, second_row_id);
    let unauthenticated = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/admin/api/v2/results/{first_row_id}/revoke"))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"requestId":"missing-boundary"}"#))
                .expect("unauthenticated request"),
        )
        .await
        .expect("unauthenticated response");
    assert_eq!(unauthenticated.status(), StatusCode::FORBIDDEN);
    let revoked = app
        .clone()
        .oneshot(admin_mutation(
            &format!("/admin/api/v2/results/{first_row_id}/revoke"),
            json!({"requestId":"operator-revoke"}),
        ))
        .await
        .expect("operator revoke");
    assert_eq!(revoked.status(), StatusCode::OK);
    let receipt = response_json(revoked).await;
    assert_eq!(receipt["resultId"], "result-admin-cross-owner");
    assert_eq!(receipt["pageState"], "revoked");
    let first_detail = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/api/v2/results/{first_row_id}"))
                .body(Body::empty())
                .expect("first result detail"),
        )
        .await
        .expect("first result detail response");
    assert_eq!(response_json(first_detail).await["pageState"], "revoked");
    let second_detail = app
        .oneshot(
            Request::builder()
                .uri(format!("/admin/api/v2/results/{second_row_id}"))
                .body(Body::empty())
                .expect("second result detail"),
        )
        .await
        .expect("second result detail response");
    assert_eq!(response_json(second_detail).await["pageState"], "available");
}

#[tokio::test]
async fn results_are_filtered_in_sql_and_paginated_by_accepted_at_and_row_id() {
    let (_directory, mut state) = test_state(AdminMode::ReadOnly).await;
    publish_result_fixture(&mut state, "result-page-newest", "newest body").await;
    publish_result_fixture(&mut state, "result-page-middle", "middle body").await;
    publish_result_fixture(&mut state, "result-page-oldest-revoked", "oldest body").await;
    sqlx::query(
        "UPDATE results SET accepted_at = CASE result_id \
         WHEN 'result-page-newest' THEN 300 \
         WHEN 'result-page-middle' THEN 200 \
         WHEN 'result-page-oldest-revoked' THEN 100 END, \
         revoked_at = CASE WHEN result_id = 'result-page-oldest-revoked' THEN 1 ELSE NULL END",
    )
    .execute(&state.app.db)
    .await
    .expect("set result pagination ordering and filter state");
    let app = router(state);

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/results?limit=2")
                .body(Body::empty())
                .expect("first results page"),
        )
        .await
        .expect("first results page response");
    assert_eq!(first.status(), StatusCode::OK);
    let first = response_json(first).await;
    assert_eq!(first["items"][0]["resultId"], "result-page-newest");
    assert_eq!(first["items"][1]["resultId"], "result-page-middle");
    let cursor = first["nextCursor"].as_str().expect("results next cursor");

    let second = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/api/v2/results?limit=2&cursor={cursor}"))
                .body(Body::empty())
                .expect("second results page"),
        )
        .await
        .expect("second results page response");
    assert_eq!(second.status(), StatusCode::OK);
    let second = response_json(second).await;
    assert_eq!(second["items"][0]["resultId"], "result-page-oldest-revoked");
    assert!(second["nextCursor"].is_null());

    let filtered = app
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/results?limit=1&pageState=revoked")
                .body(Body::empty())
                .expect("filtered results page"),
        )
        .await
        .expect("filtered results page response");
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered = response_json(filtered).await;
    assert_eq!(
        filtered["items"].as_array().expect("filtered items").len(),
        1
    );
    assert_eq!(
        filtered["items"][0]["resultId"],
        "result-page-oldest-revoked"
    );
}

#[tokio::test]
async fn expired_results_remain_expired_after_body_purge_in_lists_and_detail() {
    let (_directory, mut state) = test_state(AdminMode::ReadOnly).await;
    publish_result_fixture(&mut state, "result-expired-purged", "expired body").await;
    publish_result_fixture(&mut state, "result-current-purged", "current body").await;
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock after Unix epoch")
            .as_millis(),
    )
    .expect("test clock fits i64");
    sqlx::query(
        "UPDATE results SET body_purged_at=1, body_ciphertext=NULL, body_nonce=NULL,
             accepted_at=CASE result_id WHEN 'result-expired-purged' THEN ?1 ELSE accepted_at END,
             page_expires_at=CASE result_id WHEN 'result-expired-purged' THEN ?2 ELSE page_expires_at END,
             body_retain_until=CASE result_id WHEN 'result-expired-purged' THEN ?2 ELSE body_retain_until END",
    )
    .bind(now - 2)
    .bind(now - 1)
    .execute(&state.app.db)
    .await
    .expect("purge result bodies after expiry classification");
    let app = router(state);

    let expired = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/results?pageState=expired")
                .body(Body::empty())
                .expect("expired results request"),
        )
        .await
        .expect("expired results response");
    assert_eq!(expired.status(), StatusCode::OK);
    let expired = response_json(expired).await;
    assert_eq!(expired["items"].as_array().expect("expired items").len(), 1);
    assert_eq!(expired["items"][0]["resultId"], "result-expired-purged");
    assert_eq!(expired["items"][0]["pageState"], "expired");
    let expired_row_id = expired["items"][0]["resultRowId"]
        .as_str()
        .expect("expired row ID");

    let unavailable = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/results?pageState=content_unavailable")
                .body(Body::empty())
                .expect("unavailable results request"),
        )
        .await
        .expect("unavailable results response");
    assert_eq!(unavailable.status(), StatusCode::OK);
    let unavailable = response_json(unavailable).await;
    assert_eq!(
        unavailable["items"]
            .as_array()
            .expect("unavailable items")
            .len(),
        1
    );
    assert_eq!(unavailable["items"][0]["resultId"], "result-current-purged");
    assert_eq!(unavailable["items"][0]["pageState"], "content_unavailable");

    let detail = app
        .oneshot(
            Request::builder()
                .uri(format!("/admin/api/v2/results/{expired_row_id}"))
                .body(Body::empty())
                .expect("expired result detail request"),
        )
        .await
        .expect("expired result detail response");
    assert_eq!(detail.status(), StatusCode::OK);
    assert_eq!(response_json(detail).await["pageState"], "expired");
}
