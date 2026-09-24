use axum::{
    Json,
    extract::{Extension, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};

use crate::{auth::AuthenticatedDevice, outbox::RelayNotificationV1, state::AppState};

use super::errors::{outbox_error_response, request_id};

pub(super) async fn create_notification(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    headers: HeaderMap,
    Json(notification): Json<RelayNotificationV1>,
) -> Response {
    match state.outbox.enqueue_device(device.id, notification).await {
        Ok(accepted) => (axum::http::StatusCode::ACCEPTED, Json(accepted)).into_response(),
        Err(error) => outbox_error_response(error, &request_id(&headers)),
    }
}

pub(super) async fn notification_status(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(notification_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match state.outbox.status(device.id, &notification_id).await {
        Ok(status) => Json(status).into_response(),
        Err(error) => outbox_error_response(error, &request_id(&headers)),
    }
}
