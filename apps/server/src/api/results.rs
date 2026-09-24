use super::errors::{error_response, request_id};
use crate::{
    auth::AuthenticatedDevice,
    results::{ResultError, ResultPublicationV1},
    state::AppState,
    wechat::account_fingerprint,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

pub(super) async fn create_result(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    headers: HeaderMap,
    Json(request): Json<ResultPublicationV1>,
) -> Response {
    let target = state
        .wechat_monitor
        .as_ref()
        .and_then(|m| m.snapshot_bundle())
        .map(|b| account_fingerprint(&b.credentials.user_id));
    match state.results.publish(device.id, request, target).await {
        Ok(v) => {
            state.outbox.wake_worker();
            (StatusCode::ACCEPTED, Json(v)).into_response()
        }
        Err(e) => result_error(e, &headers),
    }
}
pub(super) async fn result_status(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match state.results.status(device.id, &id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => result_error(e, &headers),
    }
}
pub(super) async fn result_link(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match state.results.link(device.id, &id).await {
        Ok(v) => no_store(Json(v).into_response()),
        Err(e) => result_error(e, &headers),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Action {
    request_id: String,
}
pub(super) async fn revoke(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(action): Json<Action>,
) -> Response {
    if action.request_id.is_empty() || action.request_id.len() > 128 {
        return result_error(ResultError::Validation, &headers);
    }
    match state
        .results
        .revoke(device.id, &id, &action.request_id)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => result_error(e, &headers),
    }
}
pub(super) async fn resend(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(action): Json<Action>,
) -> Response {
    if action.request_id.is_empty() || action.request_id.len() > 128 {
        return result_error(ResultError::Validation, &headers);
    }
    match state
        .results
        .resend(device.id, &id, &action.request_id)
        .await
    {
        Ok(v) => {
            state.outbox.wake_worker();
            Json(v).into_response()
        }
        Err(e) => result_error(e, &headers),
    }
}
pub(super) fn result_error(error: ResultError, headers: &HeaderMap) -> Response {
    let (status, code, message) = match error {
        ResultError::Validation => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "result validation failed",
        ),
        ResultError::Conflict => (
            StatusCode::CONFLICT,
            "IDEMPOTENCY_CONFLICT",
            "result identifiers conflict with existing content",
        ),
        ResultError::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", "result not found"),
        ResultError::Inaccessible => (StatusCode::NOT_FOUND, "NOT_FOUND", "resource not found"),
        ResultError::TargetUnavailable => (
            StatusCode::CONFLICT,
            "TARGET_UNAVAILABLE",
            "notification target is unavailable",
        ),
        ResultError::Capacity => (
            StatusCode::TOO_MANY_REQUESTS,
            "RESULT_CAPACITY_REACHED",
            "result body retention capacity is reached",
        ),
        ResultError::Unavailable | ResultError::Database | ResultError::Clock => (
            StatusCode::SERVICE_UNAVAILABLE,
            "RESULTS_UNAVAILABLE",
            "result service is unavailable",
        ),
    };
    error_response(status, code, message, &request_id(headers))
}
pub(super) fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        axum::http::header::PRAGMA,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    response.headers_mut().insert(
        axum::http::header::REFERRER_POLICY,
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("x-robots-tag"),
        axum::http::HeaderValue::from_static("noindex, nofollow, noarchive"),
    );
    response.headers_mut().insert(
        axum::http::header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static(
            "default-src 'none'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    response
}
