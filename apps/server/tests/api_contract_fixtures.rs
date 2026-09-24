use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{
        HeaderMap, HeaderValue, Method, Request, StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL, PRAGMA},
    },
};
use promptdock_server::{
    api,
    auth::DeviceScope,
    config::{DatabaseConfig, ResultsConfig, ServerConfig},
    db,
    outbox::{BundleContentCipher, NotificationBundleV1, RelayNotificationV1},
    qr_login::LoginManager,
    results::{ResultPublicationV1, ResultService},
    shutdown::TaskSupervisor,
    state::AppState,
};
use relay_provider_wechat::protocol::GetBotQrCodeResponse;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt as _;
use uuid::Uuid;

const CONTRACT_TIMESTAMP: i64 = 1_800_000_000_000;
const PROVIDER_TIMESTAMP: i64 = CONTRACT_TIMESTAMP + 100;
const CONTRACT_LOGIN_ID: &str = "00000000-0000-4000-8000-000000000001";
const CONTRACT_REQUEST_ID: &str = "00000000-0000-4000-8000-000000000002";
const CONTRACT_TEST_ID: &str = "test-00000000-0000-4000-8000-000000000003";

fn fixtures_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/server-http/v1")
}

fn fixture(name: &str) -> Value {
    let path = fixtures_directory().join(name);
    let bytes = fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "could not read API contract fixture {}: {error}",
            path.display()
        )
    });
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("invalid API contract fixture {}: {error}", path.display()))
}

fn set_field(value: &mut Value, field: &str, replacement: Value) {
    let field = value
        .as_object_mut()
        .and_then(|object| object.get_mut(field))
        .unwrap_or_else(|| panic!("response did not contain contract field {field}"));
    *field = replacement;
}

fn set_error_request_id(value: &mut Value) {
    let request_id = value
        .get_mut("error")
        .and_then(Value::as_object_mut)
        .and_then(|error| error.get_mut("requestId"))
        .expect("error response did not contain requestId");
    *request_id = json!(CONTRACT_REQUEST_ID);
}

async fn call_with_headers(app: Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = app.oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 128 * 1024)
        .await
        .expect("response body");
    let body = serde_json::from_slice(&bytes).expect("JSON response body");
    (status, headers, body)
}

async fn call(app: Router, request: Request<Body>) -> (StatusCode, Value) {
    let (status, _, body) = call_with_headers(app, request).await;
    (status, body)
}

fn request(method: Method, uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    request
        .body(Body::from(serde_json::to_vec(&body).expect("request JSON")))
        .expect("request")
}

async fn test_app() -> (tempfile::TempDir, AppState, Router, String) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = DatabaseConfig {
        path: directory.path().join("relay.db"),
        ..DatabaseConfig::default()
    };
    let pool = db::open(&database).await.expect("database");
    let mut state = AppState::new(pool, TaskSupervisor::new());
    state.results = ResultService::new(
        state.db.clone(),
        ResultsConfig {
            enabled: true,
            public_origin: Some("https://relay.example.test".into()),
            ..Default::default()
        },
        Some(BundleContentCipher::new_for_test([9; 32])),
    );
    assert!(state.results.available());
    state
        .server_info
        .features
        .push("result_pages_v1".to_owned());
    state.mark_ready();
    let token = state
        .device_auth
        .create_device("CONTRACT TEST", &DeviceScope::ALL)
        .await
        .expect("device")
        .token
        .expose()
        .to_owned();
    let app = api::router(state.clone(), &ServerConfig::default());
    (directory, state, app, token)
}

#[test]
fn fixture_inventory_and_notification_request_shape_are_frozen() {
    let mut names: Vec<String> = fs::read_dir(fixtures_directory())
        .expect("fixture directory")
        .map(|entry| {
            entry
                .expect("fixture entry")
                .file_name()
                .into_string()
                .expect("UTF-8 fixture name")
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "channel-managed-by-admin-v1.json",
            "device-status-v1.json",
            "error-v1.json",
            "insufficient-scope-v1.json",
            "legacy-wechat-not-found-v1.json",
            "manifest.json",
            "notification-accepted-v1.json",
            "notification-bundle-receipt-v1.json",
            "notification-bundle-v1.json",
            "notification-status-v1.json",
            "notification-v1.json",
            "result-publication-v1.json",
            "result-receipt-v1.json",
            "server-info-v1.json",
            "wechat-login-v1.json",
            "wechat-status-v1.json",
            "wechat-test-accepted-v1.json",
        ]
    );

    let notification = fixture("notification-v1.json");
    serde_json::from_value::<RelayNotificationV1>(notification)
        .expect("notification fixture must deserialize through the production DTO");
    serde_json::from_value::<NotificationBundleV1>(fixture("notification-bundle-v1.json"))
        .expect("bundle fixture must deserialize through the production DTO");
    let result: ResultPublicationV1 = serde_json::from_value(fixture("result-publication-v1.json"))
        .expect("result fixture must deserialize through the production DTO");
    assert_eq!(
        format!("{:x}", Sha256::digest(result.body.as_bytes())),
        result.source_hash
    );
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        fixture("result-publication-v1.json")
    );
}

#[tokio::test]
async fn legacy_path_request_ids_and_sensitive_cache_headers_are_frozen() {
    let (_directory, _state, app, token) = test_app().await;
    let mut legacy_request = request(
        Method::GET,
        "/v1/channels/wechat",
        Some(&token),
        Value::Null,
    );
    legacy_request.headers_mut().insert(
        api::X_REQUEST_ID.clone(),
        HeaderValue::from_static("client-supplied-request-id"),
    );
    let (status, headers, mut body) = call_with_headers(app.clone(), legacy_request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers[CACHE_CONTROL], "no-store");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    let response_request_id = headers[api::X_REQUEST_ID.clone()]
        .to_str()
        .expect("response request ID");
    let body_request_id = body["error"]["requestId"]
        .as_str()
        .expect("error request ID");
    assert_eq!(response_request_id, body_request_id);
    assert_ne!(response_request_id, "client-supplied-request-id");
    let parsed_request_id = Uuid::parse_str(response_request_id).expect("server UUID request ID");
    assert!(!parsed_request_id.is_nil());
    assert_eq!(parsed_request_id.to_string(), response_request_id);
    set_error_request_id(&mut body);
    assert_eq!(body, fixture("legacy-wechat-not-found-v1.json"));

    let (second_status, second_headers, second_body) = call_with_headers(
        app.clone(),
        request(
            Method::GET,
            "/v1/channels/wechat",
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(second_status, StatusCode::NOT_FOUND);
    let second_request_id = second_headers[api::X_REQUEST_ID.clone()]
        .to_str()
        .expect("second response request ID");
    assert_eq!(second_request_id, second_body["error"]["requestId"]);
    assert_ne!(second_request_id, response_request_id);
    Uuid::parse_str(second_request_id).expect("fresh server UUID request ID");

    let (status, headers, login_error) = call_with_headers(
        app,
        request(
            Method::POST,
            "/v1/channels/wechat/login",
            None,
            json!({"forceFresh": false}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers[CACHE_CONTROL], "no-store, max-age=0");
    assert_eq!(headers[PRAGMA], "no-cache");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers[api::X_REQUEST_ID.clone()]
            .to_str()
            .expect("request ID"),
        login_error["error"]["requestId"]
            .as_str()
            .expect("login error request ID")
    );
}

#[tokio::test]
async fn actual_router_responses_match_the_golden_contracts() {
    let (_directory, state, app, token) = test_app().await;

    let (status, mut unauthorized) = call(
        app.clone(),
        request(Method::GET, "/v1/server-info", None, Value::Null),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    set_error_request_id(&mut unauthorized);
    assert_eq!(unauthorized, fixture("error-v1.json"));

    let limited_token = state
        .device_auth
        .create_device("LIMITED CONTRACT", &[DeviceScope::ChannelRead])
        .await
        .expect("limited device")
        .token
        .expose()
        .to_owned();
    let (status, mut insufficient) = call(
        app.clone(),
        request(
            Method::GET,
            "/v1/notifications/missing",
            Some(&limited_token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    set_error_request_id(&mut insufficient);
    assert_eq!(insufficient, fixture("insufficient-scope-v1.json"));

    let (status, server_info) = call(
        app.clone(),
        request(Method::GET, "/v1/server-info", Some(&token), Value::Null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(server_info, fixture("server-info-v1.json"));

    let (status, mut device_status) = call(
        app.clone(),
        request(Method::GET, "/v1/device-status", Some(&token), Value::Null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    set_field(
        &mut device_status,
        "deviceId",
        json!("00000000-0000-4000-8000-000000000001"),
    );
    assert_eq!(device_status, fixture("device-status-v1.json"));

    for method in [Method::GET, Method::HEAD] {
        let response = app
            .clone()
            .oneshot(request(
                method,
                "/v1/authorization/wechat-handoff",
                Some(&token),
                Value::Null,
            ))
            .await
            .expect("handoff preflight response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let body = to_bytes(response.into_body(), 1)
            .await
            .expect("handoff preflight body");
        assert!(body.is_empty());
    }

    let (status, mut handoff_insufficient) = call(
        app.clone(),
        request(
            Method::GET,
            "/v1/authorization/wechat-handoff",
            Some(&limited_token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    set_error_request_id(&mut handoff_insufficient);
    assert_eq!(handoff_insufficient, fixture("insufficient-scope-v1.json"));

    let (status, headers, mut grant_denied) = call_with_headers(
        app.clone(),
        request(
            Method::POST,
            "/v1/authorization/wechat-admin-login-grants",
            Some(&token),
            json!({"expiresInSeconds": 600}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(headers[CACHE_CONTROL], "no-store");
    set_error_request_id(&mut grant_denied);
    assert_eq!(grant_denied, fixture("channel-managed-by-admin-v1.json"));

    let (status, mut disconnect_denied) = call(
        app.clone(),
        request(
            Method::POST,
            "/v1/channels/wechat/disconnect",
            Some(&token),
            json!({"confirm": false}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    set_error_request_id(&mut disconnect_denied);
    assert_eq!(
        disconnect_denied,
        fixture("channel-managed-by-admin-v1.json")
    );

    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_millis(),
    )
    .expect("timestamp");
    let mut notification = fixture("notification-v1.json");
    set_field(&mut notification, "createdAt", json!(now - 1));
    set_field(&mut notification, "expiresAt", json!(now + 300_000));
    let (status, mut accepted) = call(
        app.clone(),
        request(
            Method::POST,
            "/v1/notifications",
            Some(&token),
            notification,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    set_field(&mut accepted, "acceptedAt", json!(CONTRACT_TIMESTAMP));
    assert_eq!(accepted, fixture("notification-accepted-v1.json"));

    sqlx::query(
        "UPDATE notification_outbox \
         SET status = 'provider_accepted', attempt_count = 1, \
             provider_message_id = 'provider-contract-001', \
             provider_accepted_at = ?1, updated_at = ?1 \
         WHERE notification_id = 'contract-notification-001'",
    )
    .bind(PROVIDER_TIMESTAMP)
    .execute(&state.db)
    .await
    .expect("freeze provider-accepted row");

    let (status, notification_status) = call(
        app.clone(),
        request(
            Method::GET,
            "/v1/notifications/contract-notification-001",
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(notification_status, fixture("notification-status-v1.json"));

    let (status, wechat_status) = call(
        app.clone(),
        request(
            Method::GET,
            "/v1/channels/wechat/status",
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(wechat_status, fixture("wechat-status-v1.json"));

    let (status, mut test_accepted) = call(
        app,
        request(
            Method::POST,
            "/v1/channels/wechat/test",
            Some(&token),
            json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    set_field(
        &mut test_accepted,
        "notificationId",
        json!(CONTRACT_TEST_ID),
    );
    set_field(&mut test_accepted, "acceptedAt", json!(CONTRACT_TIMESTAMP));
    assert_eq!(test_accepted, fixture("wechat-test-accepted-v1.json"));
}

#[test]
fn actual_login_state_machine_snapshot_matches_the_golden_contract() {
    let manager = LoginManager::new();
    let owner = Uuid::parse_str("00000000-0000-4000-8000-000000000010").expect("owner UUID");
    let started = manager.start(owner, false, false).expect("start login");
    let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login UUID");
    manager
        .install_qr(
            login_id,
            GetBotQrCodeResponse {
                qrcode: "contract-qr-token-not-serialized".to_owned(),
                qrcode_img_content: "contract-qr-content".to_owned(),
            },
        )
        .expect("install QR");
    let mut snapshot =
        serde_json::to_value(manager.snapshot(owner, login_id).expect("login snapshot"))
            .expect("serialize login snapshot");
    set_field(&mut snapshot, "loginId", json!(CONTRACT_LOGIN_ID));
    set_field(
        &mut snapshot,
        "expiresAt",
        json!(CONTRACT_TIMESTAMP + 300_000),
    );
    assert_eq!(snapshot, fixture("wechat-login-v1.json"));
    assert!(!snapshot.to_string().contains("contract-qr-token"));
}

#[test]
fn bundle_fixture_hash_matches_exact_body_and_receipt() {
    let request = fixture("notification-bundle-v1.json");
    let receipt = fixture("notification-bundle-receipt-v1.json");
    let body = request["body"].as_str().expect("bundle body");
    let hash = format!("{:x}", Sha256::digest(body.as_bytes()));
    assert_eq!(request["sourceHash"], hash);
    assert_eq!(receipt["sourceHash"], request["sourceHash"]);
    assert_eq!(receipt["bundleId"], request["bundleId"]);
}
