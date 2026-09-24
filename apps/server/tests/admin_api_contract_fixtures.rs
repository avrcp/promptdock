use std::{fs, path::Path};

use axum::{
    body::{Body, to_bytes},
    http::{
        HeaderValue, Method, Request, StatusCode,
        header::{CACHE_CONTROL, PRAGMA, VARY},
    },
};
use promptdock_server::{
    admin::{AdminState, InboundCommand, RuntimeHealthRegistry, router},
    config::{Config, DatabaseConfig, RetentionConfig},
    db,
    qr_login::LoginManager,
    retention::RetentionService,
    shutdown::TaskSupervisor,
    state::AppState,
};
use relay_admin_api::{ResultDetailV2, ResultReceiptV2, ResultsPageV2};
use relay_provider_wechat::protocol::{
    GetBotQrCodeResponse, GetQrCodeStatusResponse, QrCodeStatus,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use uuid::Uuid;

const CONTRACT_TIMESTAMP: i64 = 1_787_652_000_000;
const CONTRACT_REQUEST_ID: &str = "00000000-0000-4000-8000-000000000000";
const CONTRACT_RECEIPT_ID: &str = "00000000-0000-4000-8000-000000000000";
const CONTRACT_DEVICE_ID: &str = "00000000-0000-4000-8000-000000000111";

fn fixture(name: &str) -> Value {
    let contract = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/admin-api/v2");
    let path = if name == "manifest.json" {
        contract.join(name)
    } else {
        contract.join("fixtures").join(name)
    };
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| panic!("could not read fixture {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("invalid fixture {}: {error}", path.display()))
}

#[test]
fn canonical_fixture_inventory_is_complete_and_privacy_safe() {
    let manifest = fixture("manifest.json");
    let inventory = manifest["fixtures"]
        .as_array()
        .expect("fixture inventory")
        .iter()
        .map(|entry| entry["path"].as_str().expect("fixture path"))
        .collect::<std::collections::BTreeSet<_>>();
    let expected = [
        "deliveries-page-v2.json",
        "device-action-receipt-v2.json",
        "device-create-receipt-v2.json",
        "device-detail-v2.json",
        "device-rotate-receipt-v2.json",
        "devices-page-v2.json",
        "error-v2.json",
        "inbound-commands-page-v2.json",
        "inbound-command-actions-v2.json",
        "interactive-replies-page-v2.json",
        "meta-v2.json",
        "overview-v2.json",
        "retention-action-receipt-v2.json",
        "result-detail-v2.json",
        "result-receipt-v2.json",
        "results-page-v2.json",
        "system-v2.json",
        "wechat-action-receipt-v2.json",
        "wechat-events-page-v2.json",
        "wechat-login-cancelled-v2.json",
        "wechat-login-verify-required-v2.json",
        "wechat-login-waiting-scan-v2.json",
        "wechat-status-v2.json",
        "wechat-test-receipt-v2.json",
    ]
    .into_iter()
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(inventory, expected);
    for name in inventory {
        let serialized = serde_json::to_string(&fixture(name)).expect("fixture JSON");
        for forbidden in [
            "SENTINEL_NOTIFICATION_TITLE",
            "SENTINEL_NOTIFICATION_BODY",
            "SENTINEL_MESSAGE_KEY",
            "SENTINEL_PROVIDER_MESSAGE_ID",
            "SENTINEL_ERROR_MESSAGE",
            "token_hash",
        ] {
            assert!(!serialized.contains(forbidden), "{name} leaked {forbidden}");
        }
        let is_credential_receipt = matches!(
            name,
            "device-create-receipt-v2.json" | "device-rotate-receipt-v2.json"
        );
        assert_eq!(
            serialized.contains("oneTimeToken"),
            is_credential_receipt,
            "one-time credentials must exist only in credential receipts: {name}"
        );
        let is_qr_snapshot = matches!(
            name,
            "wechat-login-waiting-scan-v2.json" | "wechat-login-verify-required-v2.json"
        );
        assert_eq!(
            serialized.contains("qrContent"),
            is_qr_snapshot,
            "QR content is allowed only in synthetic LoginSnapshot fixtures: {name}"
        );
        if is_qr_snapshot {
            assert!(serialized.contains("contract-admin-qr-content"));
        }
    }
}

#[test]
fn result_fixtures_decode_with_the_strict_admin_contract_without_secrets() {
    let page: ResultsPageV2 =
        serde_json::from_value(fixture("results-page-v2.json")).expect("results page contract");
    let detail: ResultDetailV2 =
        serde_json::from_value(fixture("result-detail-v2.json")).expect("result detail contract");
    let receipt: ResultReceiptV2 =
        serde_json::from_value(fixture("result-receipt-v2.json")).expect("result receipt contract");

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].result_id, "result-001");
    assert_eq!(detail.result_id, page.items[0].result_id);
    assert_eq!(receipt.result_id, page.items[0].result_id);

    for serialized in [
        serde_json::to_string(&page).expect("serialize results page"),
        serde_json::to_string(&detail).expect("serialize result detail"),
        serde_json::to_string(&receipt).expect("serialize result receipt"),
    ] {
        for forbidden in ["\"body\"", "\"token\"", "\"url\""] {
            assert!(
                !serialized.contains(forbidden),
                "result contract fixture leaked {forbidden}"
            );
        }
    }
}

#[test]
fn inbound_command_action_inventory_is_closed_and_matches_the_public_enum() {
    let actions = fixture("inbound-command-actions-v2.json")["actions"]
        .as_array()
        .expect("action inventory")
        .iter()
        .map(|value| value.as_str().expect("action value").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        InboundCommand::ALL
            .iter()
            .map(|command| command.as_str().to_owned())
            .collect::<Vec<_>>()
    );
    assert!(!actions.iter().any(|action| action == "list_runs"));
}

#[test]
fn actual_login_snapshots_match_admin_canonical_fixtures() {
    let manager = LoginManager::new();
    let owner = Uuid::parse_str("00000000-0000-4000-8000-000000000222").expect("owner UUID");
    let started = manager.start(owner, false, false).expect("start login");
    let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login UUID");
    manager
        .install_qr(
            login_id,
            GetBotQrCodeResponse {
                qrcode: "contract-admin-qr-token-not-serialized".to_owned(),
                qrcode_img_content: "contract-admin-qr-content".to_owned(),
            },
        )
        .expect("install QR");

    let normalize = |snapshot: promptdock_server::qr_login::LoginSnapshot| {
        let mut value = serde_json::to_value(snapshot).expect("snapshot JSON");
        value["loginId"] = json!("00000000-0000-4000-8000-000000000005");
        value["expiresAt"] = json!(CONTRACT_TIMESTAMP + 300_000);
        value
    };
    let waiting = normalize(manager.snapshot(owner, login_id).expect("waiting snapshot"));
    assert_eq!(waiting, fixture("wechat-login-waiting-scan-v2.json"));
    assert!(!waiting.to_string().contains("contract-admin-qr-token"));

    let input = manager.poll_input(login_id).expect("poll input");
    manager
        .apply_provider_status(
            login_id,
            input.generation,
            input.verify_revision,
            GetQrCodeStatusResponse {
                status: QrCodeStatus::NeedVerifyCode,
                bot_token: None,
                ilink_bot_id: None,
                baseurl: None,
                ilink_user_id: None,
                redirect_host: None,
            },
            false,
        )
        .expect("verify-required transition");
    let verify = normalize(manager.snapshot(owner, login_id).expect("verify snapshot"));
    assert_eq!(verify, fixture("wechat-login-verify-required-v2.json"));

    let cancelled = normalize(manager.cancel(owner, login_id).expect("cancel snapshot"));
    assert_eq!(cancelled, fixture("wechat-login-cancelled-v2.json"));
}

async fn test_app() -> (tempfile::TempDir, axum::Router) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = DatabaseConfig {
        path: directory.path().join("relay.db"),
        ..DatabaseConfig::default()
    };
    let pool = db::open(&database).await.expect("database");
    let retention = RetentionService::new(pool.clone(), RetentionConfig::default());
    let app = AppState::new(pool, TaskSupervisor::new());
    let admin = AdminState::new(
        app.clone(),
        &Config::default(),
        retention,
        RuntimeHealthRegistry::default(),
    )
    .expect("Admin state");
    (directory, router(admin))
}

#[tokio::test]
async fn actual_meta_and_error_responses_match_canonical_fixtures() {
    let (_directory, app) = test_app().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/api/v2/meta")
                .header("x-request-id", "untrusted-client-value")
                .body(Body::empty())
                .expect("meta request"),
        )
        .await
        .expect("meta response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store, max-age=0");
    assert_eq!(response.headers()[PRAGMA], "no-cache");
    assert_eq!(response.headers()[VARY], "Authorization");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .expect("request ID");
    assert_ne!(request_id, "untrusted-client-value");
    Uuid::parse_str(request_id).expect("server-generated UUID");
    let mut body: Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 256 * 1024)
            .await
            .expect("meta body"),
    )
    .expect("meta JSON");
    body["generatedAt"] = json!(CONTRACT_TIMESTAMP);
    assert_eq!(body, fixture("meta-v2.json"));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/missing")
                .body(Body::empty())
                .expect("missing request"),
        )
        .await
        .expect("missing response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response_request_id = response.headers()["x-request-id"]
        .to_str()
        .expect("request ID")
        .to_owned();
    let mut body: Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 256 * 1024)
            .await
            .expect("error body"),
    )
    .expect("error JSON");
    assert_eq!(body["requestId"], response_request_id);
    body["requestId"] = json!(CONTRACT_REQUEST_ID);
    assert_eq!(body, fixture("error-v2.json"));
}

#[tokio::test]
async fn inbound_command_filter_exposes_list_jobs_and_rejects_the_gateway_storage_name() {
    let (_directory, app) = test_app().await;
    let stable = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/inbound-commands?command=list_jobs")
                .body(Body::empty())
                .expect("stable action request"),
        )
        .await
        .expect("stable action response");
    assert_eq!(stable.status(), StatusCode::OK);
    let legacy = app
        .oneshot(
            Request::builder()
                .uri("/admin/api/v2/inbound-commands?command=list_runs")
                .body(Body::empty())
                .expect("legacy action request"),
        )
        .await
        .expect("legacy action response");
    assert_eq!(legacy.status(), StatusCode::BAD_REQUEST);
}

fn mutation_request(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("origin", "http://127.0.0.1:5173")
        .header("content-type", "application/json")
        .header("x-promptdock-admin-action", "1")
        .header("sec-fetch-site", "same-origin")
        .body(Body::from(serde_json::to_vec(&body).expect("request JSON")))
        .expect("mutation request")
}

fn normalize_receipt(mut body: Value) -> Value {
    if body.get("receiptId").is_some() {
        body["receiptId"] =
            if body.get("state").and_then(Value::as_str) == Some("accepted_by_relay") {
                json!(format!("wechat_test_{}", "0".repeat(64)))
            } else {
                json!(CONTRACT_RECEIPT_ID)
            };
    }
    if body.get("deviceId").is_some() {
        body["deviceId"] = json!(CONTRACT_DEVICE_ID);
    }
    if body.get("oneTimeToken").is_some() {
        let action = body["action"].as_str().unwrap_or_default();
        let token = if action == "rotate" {
            "pdv2.00000000-0000-4000-8000-000000000111.BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"
        } else {
            "pdv2.00000000-0000-4000-8000-000000000111.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        };
        body["oneTimeToken"] = json!(token);
    }
    for field in ["issuedAt", "completedAt", "startedAt"] {
        if body.get(field).is_some() {
            body[field] = json!(CONTRACT_TIMESTAMP);
        }
    }
    body
}

#[test]
fn wechat_receipt_fixtures_are_closed_and_safe() {
    let test = fixture("wechat-test-receipt-v2.json");
    assert_eq!(test.as_object().expect("test receipt").len(), 3);
    assert_eq!(test["state"], "accepted_by_relay");
    assert!(
        test["receiptId"]
            .as_str()
            .is_some_and(|value| value.starts_with("wechat_test_") && value.len() == 76)
    );
    assert!(test.get("notificationId").is_none());

    let action = fixture("wechat-action-receipt-v2.json");
    assert_eq!(action.as_object().expect("action receipt").len(), 3);
    assert_eq!(action["action"], "wechat.disconnect");
    assert!(action.get("reason").is_none());
}

#[tokio::test]
async fn actual_operator_mutations_match_canonical_receipts() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = DatabaseConfig {
        path: directory.path().join("relay.db"),
        ..DatabaseConfig::default()
    };
    let pool = db::open(&database).await.expect("database");
    let retention = RetentionService::new(pool.clone(), RetentionConfig::default());
    let app = AppState::new(pool, TaskSupervisor::new());
    let admin = AdminState::new(
        app,
        &Config {
            admin: promptdock_server::config::AdminConfig {
                mode: promptdock_server::config::AdminMode::Operator,
                ..Default::default()
            },
            ..Config::default()
        },
        retention,
        RuntimeHealthRegistry::default(),
    )
    .expect("Admin state");
    let app = router(admin);
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

    let created = app
        .clone()
        .oneshot(mutation_request("/admin/api/v2/devices", create_body))
        .await
        .expect("create response");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(
        &to_bytes(created.into_body(), 256 * 1024)
            .await
            .expect("create body"),
    )
    .expect("create JSON");
    let device_id = created["deviceId"].as_str().expect("device id").to_owned();
    let created = normalize_receipt(created);
    assert_eq!(created, fixture("device-create-receipt-v2.json"));

    let rotated = app
        .clone()
        .oneshot(mutation_request(
            &format!("/admin/api/v2/devices/{device_id}/rotate"),
            json!({}),
        ))
        .await
        .expect("rotate response");
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated = normalize_receipt(
        serde_json::from_slice(
            &to_bytes(rotated.into_body(), 256 * 1024)
                .await
                .expect("rotate body"),
        )
        .expect("rotate JSON"),
    );
    assert_eq!(rotated, fixture("device-rotate-receipt-v2.json"));

    let disabled = app
        .clone()
        .oneshot(mutation_request(
            &format!("/admin/api/v2/devices/{device_id}/disable"),
            json!({}),
        ))
        .await
        .expect("disable response");
    assert_eq!(disabled.status(), StatusCode::OK);
    let disabled = normalize_receipt(
        serde_json::from_slice(
            &to_bytes(disabled.into_body(), 256 * 1024)
                .await
                .expect("disable body"),
        )
        .expect("disable JSON"),
    );
    assert_eq!(disabled, fixture("device-action-receipt-v2.json"));

    let test = app
        .clone()
        .oneshot(mutation_request("/admin/api/v2/wechat/test", json!({})))
        .await
        .expect("disabled WeChat test response");
    assert_eq!(test.status(), StatusCode::SERVICE_UNAVAILABLE);
    let test: Value = serde_json::from_slice(
        &to_bytes(test.into_body(), 256 * 1024)
            .await
            .expect("disabled WeChat test body"),
    )
    .expect("disabled WeChat test JSON");
    assert_eq!(test["code"], "ADMIN_WECHAT_UNAVAILABLE");

    let retention = app
        .oneshot(mutation_request(
            "/admin/api/v2/maintenance/retention",
            json!({}),
        ))
        .await
        .expect("retention response");
    assert_eq!(retention.status(), StatusCode::OK);
    let retention = normalize_receipt(
        serde_json::from_slice(
            &to_bytes(retention.into_body(), 256 * 1024)
                .await
                .expect("retention body"),
        )
        .expect("retention JSON"),
    );
    assert_eq!(retention, fixture("retention-action-receipt-v2.json"));
}

#[tokio::test]
async fn meta_supports_head_without_exposing_a_body() {
    let (_directory, app) = test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/admin/api/v2/meta")
                .header("authorization", HeaderValue::from_static("Basic redacted"))
                .body(Body::empty())
                .expect("HEAD request"),
        )
        .await
        .expect("HEAD response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        to_bytes(response.into_body(), 1)
            .await
            .expect("HEAD body")
            .is_empty()
    );
}
