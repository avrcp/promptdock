use axum::{
    Json,
    extract::{Extension, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};

use crate::{
    auth::AuthenticatedDevice, outbox::NotificationBundleV1, state::AppState,
    wechat::account_fingerprint,
};

use super::errors::{outbox_error_response, request_id};

pub(super) async fn retired_notification_bundle(headers: HeaderMap) -> Response {
    super::errors::error_response(
        axum::http::StatusCode::GONE,
        "BUNDLE_CREATION_RETIRED",
        "notification bundle creation is retired; use results",
        &request_id(&headers),
    )
}

#[allow(dead_code)] // legacy status reads remain; creation is intentionally retired.
pub(super) async fn create_notification_bundle(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    headers: HeaderMap,
    Json(bundle): Json<NotificationBundleV1>,
) -> Response {
    let Some(target_account_fingerprint) = state
        .wechat_monitor
        .as_ref()
        .and_then(|monitor| monitor.snapshot_bundle())
        .map(|bundle| account_fingerprint(&bundle.credentials.user_id))
    else {
        return outbox_error_response(
            crate::outbox::OutboxError::TargetUnavailable,
            &request_id(&headers),
        );
    };
    match state
        .outbox
        .enqueue_bundle(device.id, target_account_fingerprint, bundle)
        .await
    {
        Ok(receipt) => (axum::http::StatusCode::ACCEPTED, Json(receipt)).into_response(),
        Err(error) => outbox_error_response(error, &request_id(&headers)),
    }
}

pub(super) async fn notification_bundle_status(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(bundle_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match state.outbox.bundle_status(device.id, &bundle_id).await {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => outbox_error_response(error, &request_id(&headers)),
    }
}
