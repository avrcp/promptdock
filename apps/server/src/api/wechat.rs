use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::AuthenticatedDevice, outbox::RelayNotificationV1, qr_login::LoginError, state::AppState,
};

use super::errors::{
    error_response, login_error_response, outbox_error_response, request_id, wechat_login_response,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StartWechatLoginRequest {
    #[serde(default)]
    force_fresh: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VerifyWechatLoginRequest {
    code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DisconnectWechatRequest {
    confirm: bool,
}

pub(super) async fn wechat_channel_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    match state.wechat_status().await {
        Ok(status) => Json(status).into_response(),
        Err(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHANNEL_STATUS_UNAVAILABLE",
            "channel status is unavailable",
            &request_id(&headers),
        ),
    }
}

pub(super) async fn disconnect_wechat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DisconnectWechatRequest>,
) -> Response {
    if !request.confirm {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "DISCONNECT_CONFIRMATION_REQUIRED",
            "disconnect confirmation is required",
            &request_id(&headers),
        );
    }
    let Some(service) = &state.wechat_login else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHANNEL_UNAVAILABLE",
            "WeChat channel is not enabled",
            &request_id(&headers),
        );
    };
    match service.disconnect().await {
        Ok(()) => Json(serde_json::json!({"state":"disconnected"})).into_response(),
        Err(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "WECHAT_DISCONNECT_FAILED",
            "WeChat channel could not be disconnected safely",
            &request_id(&headers),
        ),
    }
}

pub(super) async fn test_wechat_notification(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    headers: HeaderMap,
) -> Response {
    let created_at = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "CLOCK_UNAVAILABLE",
                "system clock is unavailable",
                &request_id(&headers),
            );
        }
    };
    let notification_id = format!("test-{}", Uuid::new_v4());
    let notification = RelayNotificationV1 {
        schema_version: 1,
        dedupe_key: notification_id.clone(),
        notification_id,
        kind: "test".to_owned(),
        priority: 100,
        title: "PromptDock Relay 测试".to_owned(),
        body: "✅ PromptDock Relay 测试通知".to_owned(),
        correlation_key: None,
        created_at,
        expires_at: created_at.saturating_add(5 * 60 * 1_000),
    };
    match state.outbox.enqueue_device(device.id, notification).await {
        Ok(accepted) => (StatusCode::ACCEPTED, Json(accepted)).into_response(),
        Err(error) => outbox_error_response(error, &request_id(&headers)),
    }
}

pub(super) async fn start_wechat_login(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    headers: HeaderMap,
    Json(request): Json<StartWechatLoginRequest>,
) -> Response {
    let Some(service) = &state.wechat_login else {
        return wechat_login_response(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHANNEL_UNAVAILABLE",
            "WeChat login is not enabled",
            &request_id(&headers),
        ));
    };
    let response = match service.start(device.id, request.force_fresh) {
        Ok(outcome) => Json(outcome.snapshot).into_response(),
        Err(error) => login_error_response(error, &request_id(&headers)),
    };
    wechat_login_response(response)
}

pub(super) async fn wechat_login_status(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(login_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let response = match state.wechat_login.as_ref() {
        Some(service) => match parse_login_id(&login_id) {
            Ok(login_id) => match service.snapshot(device.id, login_id) {
                Ok(snapshot) => Json(snapshot).into_response(),
                Err(error) => login_error_response(error, &request_id(&headers)),
            },
            Err(error) => login_error_response(error, &request_id(&headers)),
        },
        None => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHANNEL_UNAVAILABLE",
            "WeChat login is not enabled",
            &request_id(&headers),
        ),
    };
    wechat_login_response(response)
}

pub(super) async fn verify_wechat_login(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(login_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<VerifyWechatLoginRequest>,
) -> Response {
    let response = match state.wechat_login.as_ref() {
        Some(service) => match parse_login_id(&login_id) {
            Ok(login_id) => match service.verify(device.id, login_id, &request.code) {
                Ok(snapshot) => Json(snapshot).into_response(),
                Err(error) => login_error_response(error, &request_id(&headers)),
            },
            Err(error) => login_error_response(error, &request_id(&headers)),
        },
        None => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHANNEL_UNAVAILABLE",
            "WeChat login is not enabled",
            &request_id(&headers),
        ),
    };
    wechat_login_response(response)
}

pub(super) async fn cancel_wechat_login(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(login_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let response = match state.wechat_login.as_ref() {
        Some(service) => match parse_login_id(&login_id) {
            Ok(login_id) => match service.cancel(device.id, login_id) {
                Ok(snapshot) => Json(snapshot).into_response(),
                Err(error) => login_error_response(error, &request_id(&headers)),
            },
            Err(error) => login_error_response(error, &request_id(&headers)),
        },
        None => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHANNEL_UNAVAILABLE",
            "WeChat login is not enabled",
            &request_id(&headers),
        ),
    };
    wechat_login_response(response)
}

fn parse_login_id(value: &str) -> Result<Uuid, LoginError> {
    Uuid::parse_str(value).map_err(|_| LoginError::NotFound)
}

pub(super) fn is_wechat_login_path(path: &str) -> bool {
    path.strip_prefix("/v1/channels/wechat/login")
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}
