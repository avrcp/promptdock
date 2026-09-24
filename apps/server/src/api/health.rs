use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::state::AppState;

use super::errors::{error_response, request_id};

pub(super) async fn live() -> Json<HealthStatus> {
    Json(HealthStatus { status: "ok" })
}

pub(super) async fn ready(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if state.is_ready() {
        Json(HealthStatus { status: "ready" }).into_response()
    } else {
        error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "NOT_READY",
            "relay is not ready",
            &request_id(&headers),
        )
    }
}

#[derive(Serialize)]
pub(super) struct HealthStatus {
    status: &'static str,
}
