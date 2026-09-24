use std::time::Duration;

use axum::{
    Json,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::{AuthenticatedDevice, AuthenticationError, DeviceScope},
    state::AppState,
    wechat_admin_login::WechatAdminGrantError,
};

use super::errors::{error_response, request_id};

pub(super) async fn wechat_handoff_preflight() -> StatusCode {
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateWechatAdminLoginGrantRequest {
    expires_in_seconds: u64,
}

pub(super) async fn create_wechat_admin_login_grant(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    headers: HeaderMap,
    Json(request): Json<CreateWechatAdminLoginGrantRequest>,
) -> Response {
    if !(1..=600).contains(&request.expires_in_seconds) {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "grant expiration validation failed",
            &request_id(&headers),
        );
    }

    let _guard = state.wechat_admin_login_grants.lifecycle_guard().await;
    let lease = match state
        .device_auth
        .gateway_authorization_lease(device.id)
        .await
    {
        Ok(Some(lease)) => lease,
        Ok(None) => {
            return error_response(
                StatusCode::FORBIDDEN,
                "AUTHORIZATION_CHANGED",
                "device authorization changed",
                &request_id(&headers),
            );
        }
        Err(AuthenticationError::Database) | Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "AUTH_UNAVAILABLE",
                "authentication service is unavailable",
                &request_id(&headers),
            );
        }
    };
    if lease.credential_revision != device.authorization_revision()
        || !lease.scopes.contains(&DeviceScope::ChannelRead)
        || !lease.scopes.contains(&DeviceScope::ChannelManage)
    {
        return error_response(
            StatusCode::FORBIDDEN,
            "AUTHORIZATION_CHANGED",
            "device authorization changed",
            &request_id(&headers),
        );
    }

    match state.wechat_admin_login_grants.issue(
        device.id,
        lease.credential_revision,
        Duration::from_secs(request.expires_in_seconds),
    ) {
        Ok(receipt) => (StatusCode::CREATED, Json(receipt)).into_response(),
        Err(WechatAdminGrantError::InvalidRequest) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "grant expiration validation failed",
            &request_id(&headers),
        ),
        Err(WechatAdminGrantError::Random | WechatAdminGrantError::Clock) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "GRANT_UNAVAILABLE",
            "login grant is unavailable",
            &request_id(&headers),
        ),
        Err(WechatAdminGrantError::Rejected) => error_response(
            StatusCode::FORBIDDEN,
            "GRANT_REJECTED",
            "login grant was rejected",
            &request_id(&headers),
        ),
    }
}

pub(super) async fn cancel_wechat_admin_login_grant(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(grant_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(grant_id) = Uuid::parse_str(&grant_id) else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "grant identifier validation failed",
            &request_id(&headers),
        );
    };
    let _guard = state.wechat_admin_login_grants.lifecycle_guard().await;
    let lease = match state
        .device_auth
        .gateway_authorization_lease(device.id)
        .await
    {
        Ok(Some(lease)) => lease,
        Ok(None) => {
            return error_response(
                StatusCode::FORBIDDEN,
                "AUTHORIZATION_CHANGED",
                "device authorization changed",
                &request_id(&headers),
            );
        }
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "AUTH_UNAVAILABLE",
                "authentication service is unavailable",
                &request_id(&headers),
            );
        }
    };
    if lease.credential_revision != device.authorization_revision()
        || !lease.scopes.contains(&DeviceScope::ChannelRead)
        || !lease.scopes.contains(&DeviceScope::ChannelManage)
    {
        return error_response(
            StatusCode::FORBIDDEN,
            "AUTHORIZATION_CHANGED",
            "device authorization changed",
            &request_id(&headers),
        );
    }
    state
        .wechat_admin_login_grants
        .cancel_owned(grant_id, device.id);
    StatusCode::NO_CONTENT.into_response()
}
