mod authorization;
mod device_status;
pub(crate) mod errors;
mod health;
mod middleware;
mod notification_bundles;
mod notifications;
pub(crate) mod results;
mod server_info;
mod wechat;

use axum::{
    Router,
    http::{
        HeaderValue,
        header::{CACHE_CONTROL, PRAGMA},
    },
    middleware as axum_middleware,
    routing::{get, post},
};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::{
    auth::DeviceScope, config::ServerConfig, gateway::gateway_websocket,
    rate_limit::RateLimitOperation, state::AppState,
};

use authorization::{
    cancel_wechat_admin_login_grant, create_wechat_admin_login_grant, wechat_handoff_preflight,
};
use device_status::device_status;
use errors::not_found;
use health::{live, ready};
pub use middleware::X_REQUEST_ID;
use middleware::{
    RateMiddlewareState, ScopeRequirement, apply_http_layers, reject_device_channel_management,
    require_device, require_rate_limit, require_scopes,
};
use notification_bundles::{notification_bundle_status, retired_notification_bundle};
use notifications::{create_notification, notification_status};
use results::{create_result, resend, result_link, result_status, revoke};
use server_info::server_info;
use wechat::{
    cancel_wechat_login, disconnect_wechat, start_wechat_login, test_wechat_notification,
    verify_wechat_login, wechat_channel_status, wechat_login_status,
};

const NOTIFY_WRITE: &[DeviceScope] = &[DeviceScope::NotifyWrite];
const NOTIFY_READ_OWN: &[DeviceScope] = &[DeviceScope::NotifyReadOwn];
const CHANNEL_READ: &[DeviceScope] = &[DeviceScope::ChannelRead];
const WECHAT_HANDOFF: &[DeviceScope] = &[DeviceScope::ChannelRead, DeviceScope::ChannelManage];
const TEST_NOTIFICATION: &[DeviceScope] = &[DeviceScope::NotifyWrite, DeviceScope::ChannelRead];
const GATEWAY_CONNECT: &[DeviceScope] = &[DeviceScope::GatewayConnect];

pub fn router(mut state: AppState, config: &ServerConfig) -> Router {
    state.project_server_mode(config.notification_only);
    let wechat_login = Router::new()
        .route("/channels/wechat/login", post(start_wechat_login))
        .route(
            "/channels/wechat/login/{login_id}",
            get(wechat_login_status).delete(cancel_wechat_login),
        )
        .route(
            "/channels/wechat/login/{login_id}/verify",
            post(verify_wechat_login),
        )
        .layer(axum_middleware::from_fn(reject_device_channel_management))
        .layer(SetResponseHeaderLayer::overriding(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            PRAGMA,
            HeaderValue::from_static("no-cache"),
        ));
    let notifications = Router::new()
        .route("/notifications", post(create_notification))
        .layer(axum_middleware::from_fn_with_state(
            RateMiddlewareState::fixed(state.clone(), RateLimitOperation::Notifications),
            require_rate_limit,
        ))
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(NOTIFY_WRITE),
            require_scopes,
        ));
    let notification_status_route = Router::new()
        .route("/notifications/{notification_id}", get(notification_status))
        .layer(axum_middleware::from_fn_with_state(
            RateMiddlewareState::fixed(state.clone(), RateLimitOperation::Status),
            require_rate_limit,
        ))
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(NOTIFY_READ_OWN),
            require_scopes,
        ));
    let notification_bundles = Router::new()
        .route("/notification-bundles", post(retired_notification_bundle))
        .layer(axum_middleware::from_fn_with_state(
            RateMiddlewareState::fixed(state.clone(), RateLimitOperation::Notifications),
            require_rate_limit,
        ))
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(NOTIFY_WRITE),
            require_scopes,
        ));
    let notification_bundle_status_route = Router::new()
        .route(
            "/notification-bundles/{bundle_id}",
            get(notification_bundle_status),
        )
        .layer(axum_middleware::from_fn_with_state(
            RateMiddlewareState::fixed(state.clone(), RateLimitOperation::Status),
            require_rate_limit,
        ))
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(NOTIFY_READ_OWN),
            require_scopes,
        ));
    let results_write = Router::new()
        .route("/results", post(create_result))
        .route("/results/{result_id}/revoke", post(revoke))
        .route("/results/{result_id}/resend", post(resend))
        .layer(axum_middleware::from_fn_with_state(
            RateMiddlewareState::fixed(state.clone(), RateLimitOperation::Notifications),
            require_rate_limit,
        ))
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(NOTIFY_WRITE),
            require_scopes,
        ));
    let results_read = Router::new()
        .route("/results/{result_id}", get(result_status))
        .route("/results/{result_id}/link", get(result_link))
        .layer(axum_middleware::from_fn_with_state(
            RateMiddlewareState::fixed(state.clone(), RateLimitOperation::Status),
            require_rate_limit,
        ))
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(NOTIFY_READ_OWN),
            require_scopes,
        ));
    let device_status_route = Router::new()
        .route("/device-status", get(device_status))
        .layer(axum_middleware::from_fn_with_state(
            RateMiddlewareState::fixed(state.clone(), RateLimitOperation::Status),
            require_rate_limit,
        ));
    let channel_status_route = Router::new()
        .route("/channels/wechat/status", get(wechat_channel_status))
        .layer(axum_middleware::from_fn_with_state(
            RateMiddlewareState::fixed(state.clone(), RateLimitOperation::Status),
            require_rate_limit,
        ))
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(CHANNEL_READ),
            require_scopes,
        ));
    let disconnect_route = Router::new()
        .route("/channels/wechat/disconnect", post(disconnect_wechat))
        .layer(axum_middleware::from_fn(reject_device_channel_management));
    let test_route = Router::new()
        .route("/channels/wechat/test", post(test_wechat_notification))
        .layer(axum_middleware::from_fn_with_state(
            RateMiddlewareState::fixed(state.clone(), RateLimitOperation::Notifications),
            require_rate_limit,
        ))
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(TEST_NOTIFICATION),
            require_scopes,
        ));
    let authorization_route = Router::new()
        .route(
            "/authorization/wechat-handoff",
            get(wechat_handoff_preflight),
        )
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(WECHAT_HANDOFF),
            require_scopes,
        ));
    let wechat_admin_grant_route = Router::new()
        .route(
            "/authorization/wechat-admin-login-grants",
            post(create_wechat_admin_login_grant),
        )
        .route(
            "/authorization/wechat-admin-login-grants/{grant_id}",
            axum::routing::delete(cancel_wechat_admin_login_grant),
        )
        .layer(axum_middleware::from_fn(reject_device_channel_management));
    let gateway_route = Router::new()
        .route("/gateway/ws", get(gateway_websocket))
        .layer(axum_middleware::from_fn_with_state(
            ScopeRequirement(GATEWAY_CONNECT),
            require_scopes,
        ));
    let authenticated_v5 = if config.notification_only {
        Router::new().fallback(not_found)
    } else {
        gateway_route
            .fallback(not_found)
            .layer(axum_middleware::from_fn_with_state(
                state.clone(),
                require_device,
            ))
    };
    let authenticated_v1 = Router::new()
        .route("/server-info", get(server_info))
        .merge(notifications)
        .merge(notification_status_route)
        .merge(notification_bundles)
        .merge(notification_bundle_status_route)
        .merge(results_write)
        .merge(results_read)
        .merge(device_status_route)
        .merge(channel_status_route)
        .merge(disconnect_route)
        .merge(test_route)
        .merge(authorization_route)
        .merge(wechat_admin_grant_route)
        .merge(wechat_login)
        .fallback(not_found)
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            require_device,
        ));
    let routes = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/gateway/ws", get(not_found))
        .nest("/v1", authenticated_v1)
        .nest("/v2", Router::new().fallback(not_found))
        .nest("/v5", authenticated_v5)
        .route(
            "/r/{token}",
            get(crate::results_viewer::page).head(crate::results_viewer::page),
        )
        .route(
            "/r/{token}/raw",
            get(crate::results_viewer::raw).head(crate::results_viewer::raw),
        )
        .route("/result-assets/{asset}", get(crate::results_viewer::asset))
        .fallback(not_found)
        .with_state(state);

    apply_http_layers(routes, config)
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use axum::{
        Json,
        body::{Body, Bytes, to_bytes},
        http::{
            HeaderMap, HeaderValue, Method, Request, StatusCode,
            header::{
                ALLOW, AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, PRAGMA, RETRY_AFTER,
                WWW_AUTHENTICATE,
            },
        },
        routing::{get, post},
    };
    use base64::Engine as _;
    use relay_provider_wechat::{
        credentials::{
            ConnectionBundle, WECHAT_SECRET_SCHEMA_VERSION, WechatCredentials, WechatSessionSecrets,
        },
        http_client::WechatHttpClient,
        protocol::{GetBotQrCodeResponse, GetQrCodeStatusResponse, QrCodeStatus},
    };
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt as _;
    use uuid::Uuid;

    use super::*;
    use crate::{
        config::DatabaseConfig,
        outbox::{BundleContentCipher, InteractiveReplyV1, OutboxService},
        qr_login::{PollInput, QrLoginProvider, QrLoginProviderError},
        rate_limit::{LocalRateLimiter, MonotonicClock},
        secret_store::EncryptedFileSecretStore,
        shutdown::TaskSupervisor,
        wechat_login::WechatLoginService,
        wechat_monitor::{NoopMonitorActivationHook, WechatMonitorRuntime},
    };

    struct FrozenClock;

    impl MonotonicClock for FrozenClock {
        fn now(&self) -> Duration {
            Duration::ZERO
        }
    }

    fn frozen_rate_limiter() -> LocalRateLimiter {
        LocalRateLimiter::with_clock(Arc::new(FrozenClock))
    }

    async fn test_app(ready: bool) -> (tempfile::TempDir, AppState, Router) {
        test_app_with_rate_limiter(ready, LocalRateLimiter::new()).await
    }

    async fn test_app_with_rate_limiter(
        ready: bool,
        rate_limiter: LocalRateLimiter,
    ) -> (tempfile::TempDir, AppState, Router) {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let database = DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        };
        let pool = crate::db::open(&database).await.expect("test database");
        let mut state = AppState::new(pool, TaskSupervisor::new());
        // Rate-limit boundary tests inject a frozen clock, while refill
        // behavior remains covered by rate_limit's manual-clock tests.
        state.rate_limiter = rate_limiter;
        if ready {
            state.mark_ready();
        }
        let app = router(state.clone(), &ServerConfig::default());
        (directory, state, app)
    }

    struct VerifyThenConfirmProvider;

    #[async_trait]
    impl QrLoginProvider for VerifyThenConfirmProvider {
        async fn fetch_qr(
            &self,
            _local_token_list: &[String],
            _cancellation: &CancellationToken,
        ) -> Result<GetBotQrCodeResponse, QrLoginProviderError> {
            Ok(GetBotQrCodeResponse {
                qrcode: "SENTINEL_QR_TOKEN_DO_NOT_PERSIST".into(),
                qrcode_img_content: "SENTINEL_QR_CONTENT_API_ONLY".into(),
            })
        }

        async fn poll_status(
            &self,
            input: &PollInput,
        ) -> Result<GetQrCodeStatusResponse, QrLoginProviderError> {
            if input.verify_code.is_some() {
                Ok(GetQrCodeStatusResponse {
                    status: QrCodeStatus::Confirmed,
                    bot_token: Some("SENTINEL_BOT_TOKEN_ENCRYPTED_ONLY".into()),
                    ilink_bot_id: Some("bot-id@im.bot".into()),
                    baseurl: Some("https://ilinkai.weixin.qq.com/".into()),
                    ilink_user_id: Some("user-id@im.wechat".into()),
                    redirect_host: None,
                })
            } else {
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
    }

    async fn test_app_with_wechat() -> (tempfile::TempDir, AppState, Router, TaskSupervisor, PathBuf)
    {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let database = DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        };
        let pool = crate::db::open(&database).await.expect("test database");
        let supervisor = TaskSupervisor::new();
        let connection_path = directory.path().join("wechat-connection.enc");
        let store = EncryptedFileSecretStore::new_for_test(connection_path.clone(), [7; 32]);
        let service = WechatLoginService::for_test(
            store.clone(),
            None,
            crate::auth::DeviceAuthService::new(pool.clone()),
            supervisor.clone(),
            Arc::new(VerifyThenConfirmProvider),
        );
        let monitor = WechatMonitorRuntime::new_with_bundle(
            WechatHttpClient::production("PromptDockRelay/0.1.0").expect("client"),
            Arc::new(store),
            Arc::new(crate::inbound::DurableInboundMessageSink::new(
                crate::inbound::InboundCommandService::new(pool.clone()),
            )),
            Arc::new(NoopMonitorActivationHook),
            None,
        )
        .expect("monitor");
        let state = AppState::new_with_wechat_runtime(
            pool,
            supervisor.clone(),
            Some(service),
            Some(monitor.handle()),
        );
        state.mark_ready();
        let app = router(state.clone(), &ServerConfig::default());
        (directory, state, app, supervisor, connection_path)
    }

    async fn test_app_with_bundle_target() -> (tempfile::TempDir, AppState, Router, String) {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let database = DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        };
        let pool = crate::db::open(&database).await.expect("test database");
        let supervisor = TaskSupervisor::new();
        let connection_path = directory.path().join("wechat-connection.enc");
        let store = EncryptedFileSecretStore::new_for_test(connection_path, [7; 32]);
        let bundle = ConnectionBundle::new(
            WechatCredentials {
                schema_version: WECHAT_SECRET_SCHEMA_VERSION,
                bot_token: "test-bot-token".into(),
                account_id: "bot@im.bot".into(),
                user_id: "bundle-user@im.wechat".into(),
                base_url: "https://ilinkai.weixin.qq.com".into(),
                saved_at: 1,
            },
            WechatSessionSecrets::default(),
        )
        .expect("connection bundle");
        let monitor = WechatMonitorRuntime::new_with_bundle(
            WechatHttpClient::production("PromptDockRelay/test").expect("client"),
            Arc::new(store),
            Arc::new(crate::inbound::DurableInboundMessageSink::new(
                crate::inbound::InboundCommandService::new(pool.clone()),
            )),
            Arc::new(NoopMonitorActivationHook),
            Some(bundle),
        )
        .expect("monitor");
        let outbox =
            OutboxService::new(pool.clone()).with_bundle_cipher(BundleContentCipher::new([42; 32]));
        let state =
            AppState::new_with_services(pool, supervisor, outbox, None, Some(monitor.handle()));
        state.mark_ready();
        let token = state
            .device_auth
            .create_device("BUNDLE", &DeviceScope::ALL)
            .await
            .expect("device")
            .token
            .expose()
            .to_owned();
        let app = router(state.clone(), &ServerConfig::default());
        (directory, state, app, token)
    }

    async fn call(app: Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let response = app.oneshot(request).await.expect("router response");
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 128 * 1024)
            .await
            .expect("response body");
        let json = serde_json::from_slice(&body).expect("JSON response");
        (status, headers, json)
    }

    #[tokio::test]
    async fn public_private_route_and_method_boundaries_are_frozen() {
        let (_directory, _state, app) = test_app(true).await;

        for (method, path, expected) in [
            (Method::GET, "/health/live", StatusCode::OK),
            (Method::HEAD, "/health/live", StatusCode::OK),
            (Method::POST, "/health/live", StatusCode::METHOD_NOT_ALLOWED),
            (Method::GET, "/health/ready", StatusCode::OK),
            (Method::HEAD, "/health/ready", StatusCode::OK),
            (
                Method::POST,
                "/health/ready",
                StatusCode::METHOD_NOT_ALLOWED,
            ),
            (Method::GET, "/v1/gateway/ws", StatusCode::NOT_FOUND),
            (Method::GET, "/missing", StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .expect("public route request"),
                )
                .await
                .expect("public route response");
            assert_eq!(response.status(), expected, "{path}");
        }

        for (method, path) in [
            (Method::GET, "/v1/server-info"),
            (Method::POST, "/v1/server-info"),
            (Method::POST, "/v1/notifications"),
            (Method::GET, "/v1/notifications/id"),
            (Method::POST, "/v1/notification-bundles"),
            (Method::GET, "/v1/notification-bundles/id"),
            (Method::GET, "/v1/device-status"),
            (Method::GET, "/v1/authorization/wechat-handoff"),
            (Method::HEAD, "/v1/authorization/wechat-handoff"),
            (Method::POST, "/v1/authorization/wechat-admin-login-grants"),
            (
                Method::DELETE,
                "/v1/authorization/wechat-admin-login-grants/id",
            ),
            (Method::GET, "/v1/channels/wechat/status"),
            (Method::POST, "/v1/channels/wechat/disconnect"),
            (Method::POST, "/v1/channels/wechat/test"),
            (Method::POST, "/v1/channels/wechat/login"),
            (Method::GET, "/v1/channels/wechat/login/id"),
            (Method::DELETE, "/v1/channels/wechat/login/id"),
            (Method::POST, "/v1/channels/wechat/login/id/verify"),
            (Method::GET, "/v1/missing"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .expect("private route request"),
                )
                .await
                .expect("private route response");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }

    #[tokio::test]
    async fn notification_only_mode_removes_gateway_route_but_keeps_device_api() {
        let (_directory, state, _default_app) = test_app(true).await;
        let device = state
            .device_auth
            .create_device("NOTIFICATION ONLY", &DeviceScope::ALL)
            .await
            .expect("device");
        let config = ServerConfig {
            notification_only: true,
            ..ServerConfig::default()
        };
        let app = router(state, &config);

        let gateway = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v5/gateway/ws")
                    .header(AUTHORIZATION, format!("Bearer {}", device.token.expose()))
                    .body(Body::empty())
                    .expect("gateway request"),
            )
            .await
            .expect("gateway response");
        assert_eq!(gateway.status(), StatusCode::NOT_FOUND);

        let server_info = app
            .oneshot(
                Request::builder()
                    .uri("/v1/server-info")
                    .header(AUTHORIZATION, format!("Bearer {}", device.token.expose()))
                    .body(Body::empty())
                    .expect("server-info request"),
            )
            .await
            .expect("server-info response");
        assert_eq!(server_info.status(), StatusCode::OK);
        let body = to_bytes(server_info.into_body(), 8 * 1024)
            .await
            .expect("server-info body");
        assert_eq!(
            body,
            Bytes::from_static(
                br#"{"apiVersion":1,"serverVersion":"0.6.0-rc.1","features":["notifications","device_status_v1","device_scopes_v1","wechat_handoff_preflight_v1"],"wechatProtocolReference":"2.4.6"}"#
            )
        );
    }

    #[tokio::test]
    async fn health_is_public_and_server_info_requires_a_device() {
        let (_directory, state, app) = test_app(true).await;
        let device = state
            .device_auth
            .create_device("API TEST", &crate::auth::DeviceScope::ALL)
            .await
            .expect("device");
        for (path, expected) in [
            ("/health/live", json!({"status":"ok"})),
            ("/health/ready", json!({"status":"ready"})),
        ] {
            let request = Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request");
            let (status, headers, body) = call(app.clone(), request).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, expected);
            assert_eq!(
                headers.get(CACHE_CONTROL).expect("cache header"),
                "no-store"
            );
            assert_eq!(
                headers
                    .get("x-content-type-options")
                    .expect("nosniff header"),
                "nosniff"
            );
            Uuid::parse_str(
                headers
                    .get(&X_REQUEST_ID)
                    .expect("request ID")
                    .to_str()
                    .expect("request ID text"),
            )
            .expect("UUID request ID");
        }

        let request = Request::builder()
            .uri("/v1/server-info")
            .header(AUTHORIZATION, format!("Bearer {}", device.token.expose()))
            .body(Body::empty())
            .expect("request");
        let (status, _, body) = call(app.clone(), request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({
                "apiVersion": 1,
                "serverVersion": env!("CARGO_PKG_VERSION"),
                "features": [
                    "notifications",
                    "device_status_v1",
                    "device_scopes_v1",
                    "remote_gateway_v5",
                    "remote_runs_v2",
                    "remote_harness_control_v2",
                    "wechat_handoff_preflight_v1"
                ],
                "wechatProtocolReference": "2.4.6"
            })
        );
    }

    #[tokio::test]
    async fn wechat_handoff_preflight_is_scope_exact_bodyless_and_unmetered() {
        let (_directory, state, app) = test_app(true).await;
        let read_only = state
            .device_auth
            .create_device("HANDOFF READ", &[DeviceScope::ChannelRead])
            .await
            .expect("read device");
        let manage_only = state
            .device_auth
            .create_device("HANDOFF MANAGE", &[DeviceScope::ChannelManage])
            .await
            .expect("manage device");
        let complete = state
            .device_auth
            .create_device(
                "HANDOFF COMPLETE",
                &[DeviceScope::ChannelRead, DeviceScope::ChannelManage],
            )
            .await
            .expect("complete device");
        let path = "/v1/authorization/wechat-handoff";

        let (status, _, body) = call(
            app.clone(),
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("anonymous preflight"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");

        let anonymous_head = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(path)
                    .body(Body::empty())
                    .expect("anonymous HEAD preflight"),
            )
            .await
            .expect("anonymous HEAD response");
        assert_eq!(anonymous_head.status(), StatusCode::UNAUTHORIZED);

        for token in [read_only.token.expose(), manage_only.token.expose()] {
            let (status, _, body) = call(
                app.clone(),
                Request::builder()
                    .uri(path)
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("partial-scope preflight"),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["error"]["code"], "INSUFFICIENT_SCOPE");
            assert_eq!(body["error"]["message"], "request is not permitted");
            let serialized = body.to_string();
            assert!(!serialized.contains("channel:read"));
            assert!(!serialized.contains("channel:manage"));

            let head = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::HEAD)
                        .uri(path)
                        .header(AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
                        .expect("partial-scope HEAD preflight"),
                )
                .await
                .expect("partial-scope HEAD response");
            assert_eq!(head.status(), StatusCode::FORBIDDEN);
        }

        let authorization = format!("Bearer {}", complete.token.expose());
        for method in [Method::GET, Method::HEAD] {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header(AUTHORIZATION, &authorization)
                .body(Body::empty())
                .expect("authorized preflight");
            let response = app
                .clone()
                .oneshot(request)
                .await
                .expect("preflight response");
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            let bytes = to_bytes(response.into_body(), 1)
                .await
                .expect("empty preflight body");
            assert!(bytes.is_empty());
        }

        for _ in 0..3 {
            let request = Request::builder()
                .uri(path)
                .header(AUTHORIZATION, &authorization)
                .body(Body::empty())
                .expect("repeated preflight");
            assert_eq!(
                app.clone()
                    .oneshot(request)
                    .await
                    .expect("preflight response")
                    .status(),
                StatusCode::NO_CONTENT
            );
        }
        for _ in 0..5 {
            let malformed = Request::builder()
                .method(Method::POST)
                .uri("/v1/channels/wechat/disconnect")
                .header(AUTHORIZATION, &authorization)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{"))
                .expect("malformed mutation");
            assert_eq!(call(app.clone(), malformed).await.0, StatusCode::FORBIDDEN);
        }
        let exhausted = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/disconnect")
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .expect("exhausted mutation");
        assert_eq!(call(app, exhausted).await.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    #[ignore = "superseded by device_cloud_channel_mutations_are_admin_only"]
    async fn wechat_admin_login_grants_are_strict_scoped_one_time_and_locally_limited() {
        let (_directory, state, app, supervisor, _connection_path) = test_app_with_wechat().await;
        let partial = state
            .device_auth
            .create_device("GRANT PARTIAL", &[DeviceScope::ChannelManage])
            .await
            .expect("partial device");
        let owner = state
            .device_auth
            .create_device("GRANT OWNER", WECHAT_HANDOFF)
            .await
            .expect("owner device");
        let other = state
            .device_auth
            .create_device("GRANT OTHER", WECHAT_HANDOFF)
            .await
            .expect("other device");
        let path = "/v1/authorization/wechat-admin-login-grants";

        let (status, _, body) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header(AUTHORIZATION, format!("Bearer {}", partial.token.expose()))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"expiresInSeconds":60}"#))
                .expect("partial grant request"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "INSUFFICIENT_SCOPE");

        for body in [
            r#"{"expiresInSeconds":0}"#,
            r#"{"expiresInSeconds":601}"#,
            r#"{"expiresInSeconds":60,"unknown":true}"#,
        ] {
            let device = state
                .device_auth
                .create_device("GRANT VALIDATION", WECHAT_HANDOFF)
                .await
                .expect("validation device");
            let (status, headers, response) = call(
                app.clone(),
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header(AUTHORIZATION, format!("Bearer {}", device.token.expose()))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("invalid grant request"),
            )
            .await;
            assert!(matches!(status, StatusCode::UNPROCESSABLE_ENTITY));
            assert_eq!(headers[CACHE_CONTROL], "no-store");
            assert_eq!(response["error"]["code"], "VALIDATION_FAILED");
        }

        let authorization = format!("Bearer {}", owner.token.expose());
        let (status, headers, receipt) = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header(AUTHORIZATION, &authorization)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"expiresInSeconds":600}"#))
                .expect("grant request"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(headers[CACHE_CONTROL], "no-store");
        assert_eq!(receipt.as_object().expect("receipt").len(), 3);
        let grant_id =
            Uuid::parse_str(receipt["grantId"].as_str().expect("grant id")).expect("grant UUID");
        let grant_token = receipt["grantToken"].as_str().expect("grant token");
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(grant_token)
                .expect("base64url")
                .len(),
            32
        );

        let other_delete = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("{path}/{grant_id}"))
                    .header(AUTHORIZATION, format!("Bearer {}", other.token.expose()))
                    .body(Body::empty())
                    .expect("other-owner delete"),
            )
            .await
            .expect("other-owner response");
        assert_eq!(other_delete.status(), StatusCode::NO_CONTENT);
        assert!(
            state
                .wechat_admin_login_grants
                .begin_consume(grant_id, grant_token)
                .is_ok()
        );

        let fresh = call(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header(AUTHORIZATION, &authorization)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"expiresInSeconds":60}"#))
                .expect("fresh grant request"),
        )
        .await
        .2;
        let fresh_id = fresh["grantId"].as_str().expect("fresh grant id");
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::DELETE)
                        .uri(format!("{path}/{fresh_id}"))
                        .header(AUTHORIZATION, &authorization)
                        .body(Body::empty())
                        .expect("idempotent delete"),
                )
                .await
                .expect("delete response");
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        }

        let limited = state
            .device_auth
            .create_device("GRANT LIMITED", WECHAT_HANDOFF)
            .await
            .expect("limited device");
        let limited_auth = format!("Bearer {}", limited.token.expose());
        for index in 0..6 {
            let (status, _, body) = call(
                app.clone(),
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header(AUTHORIZATION, &limited_auth)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"expiresInSeconds":60}"#))
                    .expect("limited request"),
            )
            .await;
            if index < 5 {
                assert_eq!(status, StatusCode::CREATED);
            } else {
                assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
                assert_eq!(body["error"]["code"], "RATE_LIMITED_LOCAL");
            }
        }
        supervisor.begin_shutdown();
        supervisor.wait().await;
    }

    #[tokio::test]
    #[ignore = "superseded by device_cloud_channel_mutations_are_admin_only"]
    async fn insufficient_scope_is_exact_secret_free_and_precedes_rate_limiting() {
        let (_directory, state, app) = test_app(true).await;
        let device = state
            .device_auth
            .create_device("LIMITED", &[DeviceScope::GatewayConnect])
            .await
            .expect("device");
        let malformed_without_scope = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/disconnect")
            .header(AUTHORIZATION, format!("Bearer {}", device.token.expose()))
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .expect("malformed unscoped request");
        let (status, _, body) = call(app.clone(), malformed_without_scope).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "INSUFFICIENT_SCOPE");
        let cases = [
            (
                "POST",
                "/v1/notifications",
                r#"{"schemaVersion":1,"dedupeKey":"scope-test","notificationId":"scope-test","kind":"test","priority":1,"title":"x","body":"x","createdAt":1,"expiresAt":2}"#,
            ),
            ("GET", "/v1/channels/wechat/status", ""),
            ("GET", "/v1/notifications/missing", ""),
            (
                "POST",
                "/v1/channels/wechat/disconnect",
                r#"{"confirm":true}"#,
            ),
            ("POST", "/v1/channels/wechat/test", ""),
            (
                "POST",
                "/v1/channels/wechat/login",
                r#"{"forceFresh":false}"#,
            ),
            (
                "GET",
                "/v1/channels/wechat/login/00000000-0000-4000-8000-000000000001",
                "",
            ),
            (
                "POST",
                "/v1/channels/wechat/login/00000000-0000-4000-8000-000000000001/verify",
                r#"{"code":"123456"}"#,
            ),
            (
                "DELETE",
                "/v1/channels/wechat/login/00000000-0000-4000-8000-000000000001",
                "",
            ),
        ];
        for (method, uri, body) in cases.into_iter().cycle().take(45) {
            let mut builder = Request::builder()
                .method(method)
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {}", device.token.expose()))
                .header(CONTENT_TYPE, "application/json");
            if body.is_empty() {
                builder = builder.header(CONTENT_TYPE, "application/json");
            }
            let request = builder.body(Body::from(body)).expect("request");
            let (status, _, body) = call(app.clone(), request).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["error"]["code"], "INSUFFICIENT_SCOPE");
            assert_eq!(body["error"]["message"], "request is not permitted");
            let text = body.to_string();
            assert!(!text.contains("notify:write"));
        }

        let rotated = state
            .device_auth
            .rotate_device(&device.id.to_string(), &[DeviceScope::ChannelManage])
            .await
            .expect("grant manage scope");
        for (method, uri, payload) in [
            (Method::POST, "/v1/channels/wechat/disconnect", "{"),
            (Method::POST, "/v1/channels/wechat/login", "{"),
            (
                Method::POST,
                "/v1/channels/wechat/login/00000000-0000-4000-8000-000000000001/verify",
                "{",
            ),
            (
                Method::DELETE,
                "/v1/channels/wechat/login/00000000-0000-4000-8000-000000000001",
                "",
            ),
            (Method::POST, "/v1/channels/wechat/disconnect", "{"),
        ] {
            let malformed = Request::builder()
                .method(method)
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {}", rotated.token.expose()))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .expect("scoped mutation request");
            let (status, _, body) = call(app.clone(), malformed).await;
            assert_ne!(status, StatusCode::TOO_MANY_REQUESTS);
            if !payload.is_empty() {
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(body["error"]["code"], "INVALID_REQUEST");
            }
        }
        let exhausted = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/login")
            .header(AUTHORIZATION, format!("Bearer {}", rotated.token.expose()))
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .expect("rate limited request");
        let (status, headers, body) = call(app, exhausted).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "RATE_LIMITED_LOCAL");
        assert_eq!(headers[RETRY_AFTER], "6");
    }

    #[tokio::test]
    async fn device_cloud_channel_mutations_are_admin_only() {
        let (_directory, state, app) = test_app(true).await;
        let device = state
            .device_auth
            .create_device("DEVICE OBSERVER", &DeviceScope::ALL)
            .await
            .expect("device");
        let authorization = format!("Bearer {}", device.token.expose());
        let login_id = "00000000-0000-4000-8000-000000000001";
        let cases = [
            (
                Method::POST,
                "/v1/channels/wechat/login".to_owned(),
                r#"{"forceFresh":false}"#,
            ),
            (
                Method::GET,
                format!("/v1/channels/wechat/login/{login_id}"),
                "",
            ),
            (
                Method::POST,
                format!("/v1/channels/wechat/login/{login_id}/verify"),
                r#"{"code":"123456"}"#,
            ),
            (
                Method::DELETE,
                format!("/v1/channels/wechat/login/{login_id}"),
                "",
            ),
            (
                Method::POST,
                "/v1/channels/wechat/disconnect".to_owned(),
                r#"{"confirm":true}"#,
            ),
            (
                Method::POST,
                "/v1/authorization/wechat-admin-login-grants".to_owned(),
                r#"{"expiresInSeconds":60}"#,
            ),
        ];

        for (method, uri, payload) in cases {
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .header(AUTHORIZATION, &authorization)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .expect("device channel mutation");
            let (status, _, body) = call(app.clone(), request).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["error"]["code"], "CHANNEL_MANAGED_BY_ADMIN");
        }
    }

    #[tokio::test]
    async fn errors_are_json_and_share_the_response_request_id() {
        let (_directory, _state, app) = test_app(true).await;
        for request in [
            Request::builder()
                .uri("/missing")
                .body(Body::empty())
                .expect("404 request"),
            Request::builder()
                .method(Method::POST)
                .uri("/health/live")
                .body(Body::empty())
                .expect("405 request"),
        ] {
            let (status, headers, body) = call(app.clone(), request).await;
            assert!(matches!(
                status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ));
            let response_id = headers
                .get(&X_REQUEST_ID)
                .expect("request ID")
                .to_str()
                .expect("request ID text");
            assert_eq!(body["error"]["requestId"], response_id);
            assert!(body["error"].get("internal").is_none());
            if status == StatusCode::METHOD_NOT_ALLOWED {
                assert_eq!(headers.get(ALLOW).expect("Allow header"), "GET,HEAD");
            }
        }
    }

    #[tokio::test]
    async fn inbound_request_id_is_replaced() {
        let (_directory, _state, app) = test_app(true).await;
        let request = Request::builder()
            .uri("/missing")
            .header(&X_REQUEST_ID, "attacker-controlled")
            .body(Body::empty())
            .expect("request");
        let (_, headers, body) = call(app, request).await;
        let response_id = headers[&X_REQUEST_ID].to_str().expect("request ID");
        assert_ne!(response_id, "attacker-controlled");
        Uuid::parse_str(response_id).expect("UUID");
        assert_eq!(body["error"]["requestId"], response_id);
    }

    #[tokio::test]
    async fn body_limit_accepts_64k_and_rejects_larger_known_and_streamed_bodies() {
        async fn consume(body: Bytes) -> Json<Value> {
            Json(json!({"bytes": body.len()}))
        }
        let routes = Router::new().route("/consume", post(consume));
        let app = apply_http_layers(routes, &ServerConfig::default());

        let accepted = Request::builder()
            .method(Method::POST)
            .uri("/consume")
            .body(Body::from(vec![b'x'; 65_536]))
            .expect("accepted request");
        let (status, _, body) = call(app.clone(), accepted).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"bytes": 65_536}));

        for with_content_length in [true, false] {
            let mut builder = Request::builder().method(Method::POST).uri("/consume");
            if with_content_length {
                builder = builder.header("content-length", "65537");
            }
            let request = builder
                .body(Body::from(vec![b'x'; 65_537]))
                .expect("oversized request");
            let (status, headers, body) = call(app.clone(), request).await;
            assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(body["error"]["code"], "BODY_TOO_LARGE");
            assert_eq!(
                body["error"]["requestId"],
                headers[&X_REQUEST_ID].to_str().unwrap()
            );
        }
    }

    #[tokio::test]
    async fn panic_is_replaced_with_generic_json_and_server_remains_usable() {
        async fn panic_handler() -> &'static str {
            panic!("sentinel panic detail must not escape")
        }
        let routes = Router::new()
            .route("/panic", get(panic_handler))
            .route("/ok", get(|| async { Json(json!({"status":"ok"})) }));
        let app = apply_http_layers(routes, &ServerConfig::default());
        let request = Request::builder()
            .uri("/panic")
            .body(Body::empty())
            .unwrap();
        let (status, headers, body) = call(app.clone(), request).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "INTERNAL");
        assert_eq!(body["error"]["message"], "internal server error");
        assert_eq!(
            body["error"]["requestId"],
            headers[&X_REQUEST_ID].to_str().unwrap()
        );
        assert!(!body.to_string().contains("sentinel"));

        let request = Request::builder()
            .uri("/ok")
            .body(Body::empty())
            .expect("follow-up request");
        let (status, _, body) = call(app, request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"status":"ok"}));
    }

    #[tokio::test(start_paused = true)]
    async fn route_timeout_is_a_safe_json_error() {
        let routes = Router::new().route("/slow", get(std::future::pending::<()>));
        let config = ServerConfig {
            route_timeout_seconds: 1,
            ..ServerConfig::default()
        };
        let app = apply_http_layers(routes, &config);
        let request = Request::builder()
            .uri("/slow")
            .body(Body::empty())
            .expect("request");
        let response = tokio::spawn(call(app, request));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let (status, headers, body) = response.await.expect("response task");
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(body["error"]["code"], "REQUEST_TIMEOUT");
        assert_eq!(
            body["error"]["requestId"],
            headers[&X_REQUEST_ID].to_str().unwrap()
        );
    }

    #[tokio::test]
    async fn readiness_can_report_safe_unavailable_state() {
        let (_directory, _state, app) = test_app(false).await;
        let request = Request::builder()
            .uri("/health/ready")
            .body(Body::empty())
            .expect("request");
        let (status, _, body) = call(app, request).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "NOT_READY");
    }

    #[tokio::test]
    async fn wechat_channel_status_is_authenticated_and_secret_free_when_disabled() {
        let (_directory, state, app) = test_app(true).await;
        let device = state
            .device_auth
            .create_device("STATUS TEST", &crate::auth::DeviceScope::ALL)
            .await
            .expect("device");
        let request = Request::builder()
            .uri("/v1/channels/wechat/status")
            .header(AUTHORIZATION, format!("Bearer {}", device.token.expose()))
            .body(Body::empty())
            .expect("request");
        let (status, _, body) = call(app.clone(), request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({
                "state": "disconnected",
                "accountFingerprint": null,
                "lastPollAt": null,
                "lastContextAt": null,
                "lastProviderAcceptedAt": null,
                "pendingNotifications": 0,
                "blockedNotifications": 0,
                "lastErrorCode": null
            })
        );

        let legacy = Request::builder()
            .uri("/v1/channels/wechat")
            .header(AUTHORIZATION, format!("Bearer {}", device.token.expose()))
            .body(Body::empty())
            .expect("legacy request");
        assert_eq!(call(app, legacy).await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_notification_is_durable_async_and_locally_rate_limited() {
        let (_directory, state, app) =
            test_app_with_rate_limiter(true, frozen_rate_limiter()).await;
        let device = state
            .device_auth
            .create_device("TEST NOTIFICATION", &crate::auth::DeviceScope::ALL)
            .await
            .expect("device");
        let authorization = format!("Bearer {}", device.token.expose());
        for _ in 0..20 {
            let request = Request::builder()
                .method(Method::POST)
                .uri("/v1/channels/wechat/test")
                .header(AUTHORIZATION, &authorization)
                .body(Body::empty())
                .expect("test request");
            let (status, _, body) = call(app.clone(), request).await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(body["relayStatus"], "accepted");
            assert_eq!(body["existing"], false);
        }

        let limited = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/test")
            .header(AUTHORIZATION, authorization)
            .body(Body::empty())
            .expect("limited request");
        let (status, headers, body) = call(app, limited).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "RATE_LIMITED_LOCAL");
        assert_eq!(headers[RETRY_AFTER], "1");
    }

    #[tokio::test]
    #[ignore = "superseded by device_cloud_channel_mutations_are_admin_only"]
    async fn login_get_and_head_share_status_bucket_without_consuming_mutation_budget() {
        let (_directory, state, app) =
            test_app_with_rate_limiter(true, frozen_rate_limiter()).await;
        let device = state
            .device_auth
            .create_device("LOGIN RATE", &[DeviceScope::ChannelManage])
            .await
            .expect("device");
        let authorization = format!("Bearer {}", device.token.expose());
        let login_id = "00000000-0000-4000-8000-000000000001";

        for index in 0..40 {
            let method = if index % 2 == 0 {
                Method::GET
            } else {
                Method::HEAD
            };
            let request = Request::builder()
                .method(method)
                .uri(format!("/v1/channels/wechat/login/{login_id}"))
                .header(AUTHORIZATION, &authorization)
                .body(Body::empty())
                .expect("login status request");
            let response = app.clone().oneshot(request).await.expect("router response");
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }

        let limited = Request::builder()
            .uri(format!("/v1/channels/wechat/login/{login_id}"))
            .header(AUTHORIZATION, &authorization)
            .body(Body::empty())
            .expect("limited login status request");
        let response = app.clone().oneshot(limited).await.expect("router response");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[RETRY_AFTER], "1");

        let mutation = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/login")
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"forceFresh":false}"#))
            .expect("login mutation request");
        let (status, _, body) = call(app, mutation).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "CHANNEL_UNAVAILABLE");
    }

    #[tokio::test]
    async fn v1_authentication_covers_errors_fallback_and_revocation() {
        let (_directory, state, app) = test_app(true).await;
        let device = state
            .device_auth
            .create_device("AUTH TEST", &crate::auth::DeviceScope::ALL)
            .await
            .expect("device");
        let valid_token = device.token.expose().to_owned();
        let mut wrong_token = valid_token.clone();
        let last = wrong_token.pop().expect("token character");
        wrong_token.push(if last == 'A' { 'B' } else { 'A' });

        for authorization in [None, Some("Basic abc"), Some(wrong_token.as_str())] {
            let mut builder = Request::builder().uri("/v1/server-info");
            if let Some(value) = authorization {
                let value = if value == wrong_token {
                    format!("Bearer {value}")
                } else {
                    value.to_owned()
                };
                builder = builder.header(AUTHORIZATION, value);
            }
            let request = builder.body(Body::empty()).expect("request");
            let (status, headers, body) = call(app.clone(), request).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["error"]["code"], "UNAUTHORIZED");
            assert_eq!(headers[WWW_AUTHENTICATE], "Bearer");
            assert_eq!(
                body["error"]["requestId"],
                headers[&X_REQUEST_ID].to_str().expect("request ID")
            );
        }

        let missing = Request::builder()
            .uri("/v1/missing")
            .body(Body::empty())
            .expect("missing request");
        assert_eq!(call(app.clone(), missing).await.0, StatusCode::UNAUTHORIZED);

        let authenticated_missing = Request::builder()
            .uri("/v1/missing")
            .header(AUTHORIZATION, format!("bearer {valid_token}"))
            .body(Body::empty())
            .expect("authenticated missing request");
        let (status, _, body) = call(app.clone(), authenticated_missing).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "NOT_FOUND");

        let mut duplicate = Request::builder()
            .uri("/v1/server-info")
            .body(Body::empty())
            .expect("duplicate request");
        duplicate.headers_mut().append(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {valid_token}")).expect("header"),
        );
        duplicate.headers_mut().append(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {valid_token}")).expect("header"),
        );
        assert_eq!(
            call(app.clone(), duplicate).await.0,
            StatusCode::UNAUTHORIZED
        );

        state
            .device_auth
            .revoke_device(&device.id.to_string())
            .await
            .expect("revoke");
        let revoked = Request::builder()
            .uri("/v1/server-info")
            .header(AUTHORIZATION, format!("Bearer {valid_token}"))
            .body(Body::empty())
            .expect("revoked request");
        let (status, _, body) = call(app.clone(), revoked).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "DEVICE_REVOKED");

        let wrong_after_revoke = Request::builder()
            .uri("/v1/server-info")
            .header(AUTHORIZATION, format!("Bearer {wrong_token}"))
            .body(Body::empty())
            .expect("wrong request");
        assert_eq!(
            call(app, wrong_after_revoke).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn notification_ingress_replay_conflict_and_status_are_device_scoped() {
        let (_directory, state, app) = test_app(true).await;
        let first = state
            .device_auth
            .create_device("FIRST", &crate::auth::DeviceScope::ALL)
            .await
            .expect("first device");
        let second = state
            .device_auth
            .create_device("SECOND", &crate::auth::DeviceScope::ALL)
            .await
            .expect("second device");
        let now = std::time::SystemTime::UNIX_EPOCH
            .elapsed()
            .expect("time")
            .as_millis() as i64;
        let payload = json!({
            "schemaVersion": 1,
            "notificationId": "notification-1",
            "dedupeKey": "run_completed:opaque",
            "kind": "run_completed",
            "priority": 80,
            "title": "任务完成",
            "body": "safe rendered body",
            "correlationKey": "opaque-run",
            "createdAt": now,
            "expiresAt": now + 60_000
        });
        let authorization = format!("Bearer {}", first.token.expose());

        let post = |payload: &Value| {
            Request::builder()
                .method(Method::POST)
                .uri("/v1/notifications")
                .header(AUTHORIZATION, &authorization)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .expect("notification request")
        };
        let (status, headers, accepted) = call(app.clone(), post(&payload)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(accepted["notificationId"], "notification-1");
        assert_eq!(accepted["relayStatus"], "accepted");
        assert_eq!(accepted["existing"], false);
        let accepted_at = accepted["acceptedAt"].as_i64().expect("acceptedAt");
        assert_eq!(headers[CACHE_CONTROL], "no-store");

        let (status, _, replay) = call(app.clone(), post(&payload)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(replay["existing"], true);
        assert_eq!(replay["acceptedAt"], accepted_at);

        let mut conflict = payload.clone();
        conflict["body"] = json!("different safe body");
        let (status, headers, body) = call(app.clone(), post(&conflict)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "IDEMPOTENCY_CONFLICT");
        assert_eq!(
            body["error"]["requestId"],
            headers[&X_REQUEST_ID].to_str().expect("request ID")
        );
        assert!(!body.to_string().contains("different safe body"));

        let get = |token: &str| {
            Request::builder()
                .uri("/v1/notifications/notification-1")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("status request")
        };
        let (status, _, body) = call(app.clone(), get(first.token.expose())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "pending_channel");
        assert_eq!(body["attemptCount"], 0);
        let keys = body
            .as_object()
            .expect("status object")
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            [
                "attemptCount",
                "lastErrorCode",
                "notificationId",
                "providerAcceptedAt",
                "providerMessageId",
                "status",
                "updatedAt",
            ]
            .into_iter()
            .collect()
        );
        let (status, _, body) = call(app.clone(), get(second.token.expose())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "NOT_FOUND");

        for (notification_id, dedupe_key) in [
            ("notification-1", "run_completed:opaque"),
            ("system-only", "system-only"),
        ] {
            state
                .outbox
                .enqueue_system_reply(InteractiveReplyV1 {
                    schema_version: 1,
                    notification_id: notification_id.to_owned(),
                    dedupe_key: dedupe_key.to_owned(),
                    priority: 80,
                    target_account_fingerprint: format!("wx:{}", "a".repeat(64)),
                    title: "PromptDock".to_owned(),
                    body: "safe system reply".to_owned(),
                    sensitive_body: false,
                    correlation_key: Some("opaque-message-key".to_owned()),
                    created_at: now,
                    expires_at: now + 60_000,
                })
                .await
                .expect("system reply");
        }
        let (status, _, body) = call(app.clone(), get(first.token.expose())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["notificationId"], "notification-1");
        let private_system = Request::builder()
            .uri("/v1/notifications/system-only")
            .header(AUTHORIZATION, &authorization)
            .body(Body::empty())
            .expect("private system status request");
        let (status, _, body) = call(app.clone(), private_system).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "NOT_FOUND");

        let mut invalid = payload.clone();
        invalid["notificationId"] = json!("notification-invalid");
        invalid["dedupeKey"] = json!("dedupe-invalid");
        invalid["priority"] = json!(256);
        let (status, _, body) = call(app.clone(), post(&invalid)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "VALIDATION_FAILED");

        let mut unknown = payload.clone();
        unknown["notificationId"] = json!("notification-unknown");
        unknown["dedupeKey"] = json!("dedupe-unknown");
        unknown["unknownField"] = json!(true);
        let (status, _, body) = call(app.clone(), post(&unknown)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "VALIDATION_FAILED");

        let oversized = Request::builder()
            .method(Method::POST)
            .uri("/v1/notifications")
            .header(AUTHORIZATION, &authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(vec![b'x'; 65_537]))
            .expect("oversized notification");
        let (status, _, body) = call(app.clone(), oversized).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["error"]["code"], "BODY_TOO_LARGE");

        let malformed = Request::builder()
            .method(Method::POST)
            .uri("/v1/notifications")
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .expect("malformed request");
        let (status, _, body) = call(app, malformed).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "INVALID_REQUEST");
    }

    #[tokio::test]
    async fn device_status_reports_only_authenticated_self_scopes_and_gates_channel_detail() {
        let (_directory, state, app) = test_app(true).await;
        let all = state
            .device_auth
            .create_device("ALL", &DeviceScope::ALL)
            .await
            .expect("all-scope device");
        let write_only = state
            .device_auth
            .create_device("WRITE", &[DeviceScope::NotifyWrite])
            .await
            .expect("write device");

        let get = |token: &str| {
            Request::builder()
                .uri("/v1/device-status")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("device status request")
        };
        let (status, _, body) = call(app.clone(), get(all.token.expose())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deviceId"], all.id.to_string());
        assert_eq!(body["canSubmit"], true);
        assert_eq!(body["canReadOwn"], true);
        assert_eq!(body["channelState"], "reconnect_required");

        let (status, _, body) = call(app.clone(), get(write_only.token.expose())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deviceId"], write_only.id.to_string());
        assert_eq!(body["canSubmit"], true);
        assert_eq!(body["canReadOwn"], false);
        assert_eq!(body["channelState"], "unknown");

        let (status, _, body) = call(app, get("invalid")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.get("deviceId").is_none());
    }

    #[tokio::test]
    async fn bundle_creation_is_explicitly_retired() {
        let (_directory, _state, app, owner_token) = test_app_with_bundle_target().await;
        let request = Request::builder()
            .method(Method::POST)
            .uri("/v1/notification-bundles")
            .header(AUTHORIZATION, format!("Bearer {owner_token}"))
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("retired bundle request");
        let (status, _, error) = call(app, request).await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(error["error"]["code"], "BUNDLE_CREATION_RETIRED");
    }

    #[tokio::test]
    #[ignore = "device-owned cloud login was removed"]
    async fn wechat_login_api_is_owner_scoped_strict_and_promotes_encrypted_candidate() {
        let (_directory, state, app, supervisor, connection_path) = test_app_with_wechat().await;
        let owner = state
            .device_auth
            .create_device("QR OWNER", &crate::auth::DeviceScope::ALL)
            .await
            .expect("owner");
        let other = state
            .device_auth
            .create_device("QR OTHER", &crate::auth::DeviceScope::ALL)
            .await
            .expect("other");
        let owner_auth = format!("Bearer {}", owner.token.expose());
        let other_auth = format!("Bearer {}", other.token.expose());

        let channel_status = Request::builder()
            .uri("/v1/channels/wechat/status")
            .header(AUTHORIZATION, &owner_auth)
            .body(Body::empty())
            .expect("channel status request");
        let (status, _, channel) = call(app.clone(), channel_status).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(channel["state"], "disconnected");
        assert_eq!(channel["accountFingerprint"], Value::Null);

        let unknown = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/login")
            .header(AUTHORIZATION, &owner_auth)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"forceFresh":false,"baseUrl":"http://127.0.0.1"}"#,
            ))
            .expect("unknown field request");
        let (status, headers, body) = call(app.clone(), unknown).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "VALIDATION_FAILED");
        assert_eq!(headers[CACHE_CONTROL], "no-store, max-age=0");
        assert_eq!(headers[PRAGMA], "no-cache");

        let start = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/login")
            .header(AUTHORIZATION, &owner_auth)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"forceFresh":false}"#))
            .expect("start request");
        let (status, headers, started) = call(app.clone(), start).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CACHE_CONTROL], "no-store, max-age=0");
        assert_eq!(headers[PRAGMA], "no-cache");
        let login_id = started["loginId"].as_str().expect("login id").to_owned();

        let get = |authorization: &str| {
            Request::builder()
                .uri(format!("/v1/channels/wechat/login/{login_id}"))
                .header(AUTHORIZATION, authorization)
                .body(Body::empty())
                .expect("status request")
        };
        let mut snapshot = Value::Null;
        for _ in 0..100 {
            let (_, _, body) = call(app.clone(), get(&owner_auth)).await;
            snapshot = body;
            if snapshot["state"] == "verify_code_required" {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(snapshot["state"], "verify_code_required");
        assert_eq!(snapshot["qrContent"], "SENTINEL_QR_CONTENT_API_ONLY");
        assert_eq!(
            call(app.clone(), get(&other_auth)).await.2["error"]["code"],
            "NOT_FOUND"
        );

        let verify = |authorization: &str, body: &'static str| {
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/channels/wechat/login/{login_id}/verify"))
                .header(AUTHORIZATION, authorization)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("verify request")
        };
        let (status, _, body) = call(app.clone(), verify(&owner_auth, r#"{"code":"12 34"}"#)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "VALIDATION_FAILED");
        assert_eq!(
            call(app.clone(), verify(&other_auth, r#"{"code":"123456"}"#))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(app.clone(), verify(&owner_auth, r#"{"code":"123456"}"#))
                .await
                .0,
            StatusCode::OK
        );

        for _ in 0..150 {
            let (_, _, body) = call(app.clone(), get(&owner_auth)).await;
            snapshot = body;
            if snapshot["state"] == "confirmed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(snapshot["state"], "confirmed");
        assert!(snapshot.get("qrContent").is_none());
        let encrypted = tokio::fs::read(&connection_path)
            .await
            .expect("encrypted connection file");
        assert!(!String::from_utf8_lossy(&encrypted).contains("SENTINEL_BOT_TOKEN_ENCRYPTED_ONLY"));
        assert!(!String::from_utf8_lossy(&encrypted).contains("SENTINEL_QR_TOKEN_DO_NOT_PERSIST"));

        let unconfirmed_disconnect = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/disconnect")
            .header(AUTHORIZATION, &other_auth)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"confirm":false}"#))
            .expect("unconfirmed disconnect");
        let (status, _, body) = call(app.clone(), unconfirmed_disconnect).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "DISCONNECT_CONFIRMATION_REQUIRED");

        let disconnect = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/disconnect")
            .header(AUTHORIZATION, &other_auth)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"confirm":true}"#))
            .expect("disconnect");
        let (status, _, body) = call(app.clone(), disconnect).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"state":"disconnected"}));
        assert!(!connection_path.exists());

        let disconnected_status = Request::builder()
            .uri("/v1/channels/wechat/status")
            .header(AUTHORIZATION, &other_auth)
            .body(Body::empty())
            .expect("disconnected status");
        assert_eq!(
            call(app.clone(), disconnected_status).await.2["state"],
            "disconnected"
        );

        let server_info = Request::builder()
            .uri("/v1/server-info")
            .header(AUTHORIZATION, owner_auth)
            .body(Body::empty())
            .expect("server info");
        assert_eq!(
            call(app, server_info).await.2["features"],
            json!([
                "notifications",
                "device_status_v1",
                "device_scopes_v1",
                "remote_gateway_v5",
                "remote_runs_v2",
                "remote_harness_control_v2",
                "wechat_handoff_preflight_v1",
                "wechat_login",
                "wechat_channel",
                "wechat_admin_login_grant_v1"
            ])
        );
        supervisor.begin_shutdown();
        supervisor.wait().await;
    }

    #[tokio::test]
    #[ignore = "device-owned cloud login was removed"]
    async fn disabled_wechat_login_returns_safe_unavailable_with_qr_cache_headers() {
        let (_directory, state, app) = test_app(true).await;
        let anonymous = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/login")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"forceFresh":false}"#))
            .expect("anonymous request");
        let (status, headers, body) = call(app.clone(), anonymous).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
        assert_eq!(headers[CACHE_CONTROL], "no-store, max-age=0");
        assert_eq!(headers[PRAGMA], "no-cache");

        let device = state
            .device_auth
            .create_device("DISABLED QR", &crate::auth::DeviceScope::ALL)
            .await
            .expect("device");
        let authorization = format!("Bearer {}", device.token.expose());
        let method_not_allowed = Request::builder()
            .method(Method::PUT)
            .uri("/v1/channels/wechat/login")
            .header(AUTHORIZATION, &authorization)
            .body(Body::empty())
            .expect("method not allowed request");
        let (status, headers, _) = call(app.clone(), method_not_allowed).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(headers[CACHE_CONTROL], "no-store, max-age=0");
        assert_eq!(headers[PRAGMA], "no-cache");

        let oversized = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/login")
            .header(AUTHORIZATION, &authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(vec![b'x'; 65_537]))
            .expect("oversized request");
        let (status, headers, _) = call(app.clone(), oversized).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(headers[CACHE_CONTROL], "no-store, max-age=0");
        assert_eq!(headers[PRAGMA], "no-cache");

        let request = Request::builder()
            .method(Method::POST)
            .uri("/v1/channels/wechat/login")
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"forceFresh":false}"#))
            .expect("request");
        let (status, headers, body) = call(app, request).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "CHANNEL_UNAVAILABLE");
        assert_eq!(headers[CACHE_CONTROL], "no-store, max-age=0");
        assert_eq!(headers[PRAGMA], "no-cache");
    }
}
