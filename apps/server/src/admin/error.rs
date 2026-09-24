use axum::{
    Json,
    body::Body,
    http::{
        HeaderMap, Request, StatusCode,
        header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE, PRAGMA},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use relay_admin_api::AdminErrorBody;

use super::router::X_REQUEST_ID;

pub async fn normalize_framework_error(request: Request<Body>, next: Next) -> Response {
    let request_id = request_id(request.headers());
    let response = next.run(request).await;
    let is_json = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    if is_json {
        return response;
    }

    let status = response.status();
    let allow = response.headers().get(ALLOW).cloned();
    let cache_control = response.headers().get(CACHE_CONTROL).cloned();
    let pragma = response.headers().get(PRAGMA).cloned();
    let (normalized_status, code, message) = match status {
        StatusCode::NOT_FOUND => (status, "ADMIN_NOT_FOUND", "resource not found"),
        StatusCode::METHOD_NOT_ALLOWED => {
            (status, "ADMIN_METHOD_NOT_ALLOWED", "method not allowed")
        }
        StatusCode::REQUEST_TIMEOUT => (status, "ADMIN_TIMEOUT", "request timed out"),
        StatusCode::PAYLOAD_TOO_LARGE => {
            (status, "ADMIN_BODY_TOO_LARGE", "request body is too large")
        }
        StatusCode::BAD_REQUEST => (status, "ADMIN_INVALID_REQUEST", "invalid request"),
        StatusCode::FORBIDDEN => (status, "ADMIN_FORBIDDEN", "request forbidden"),
        StatusCode::CONFLICT => (status, "ADMIN_CONFLICT", "request conflict"),
        StatusCode::UNPROCESSABLE_ENTITY => (
            StatusCode::BAD_REQUEST,
            "ADMIN_VALIDATION_FAILED",
            "validation failed",
        ),
        StatusCode::TOO_MANY_REQUESTS => (status, "ADMIN_RATE_LIMITED", "request rate limited"),
        StatusCode::SERVICE_UNAVAILABLE => (status, "ADMIN_UNAVAILABLE", "service unavailable"),
        StatusCode::INTERNAL_SERVER_ERROR => (status, "ADMIN_INTERNAL", "internal server error"),
        status if status.is_client_error() => (status, "ADMIN_INVALID_REQUEST", "invalid request"),
        status if status.is_server_error() => (status, "ADMIN_INTERNAL", "internal server error"),
        _ => return response,
    };
    let mut normalized = error_response(normalized_status, code, message, &request_id);
    if let Some(allow) = allow {
        normalized.headers_mut().insert(ALLOW, allow);
    }
    if let Some(cache_control) = cache_control {
        normalized
            .headers_mut()
            .insert(CACHE_CONTROL, cache_control);
    }
    if let Some(pragma) = pragma {
        normalized.headers_mut().insert(PRAGMA, pragma);
    }
    normalized
}

pub async fn not_found(headers: HeaderMap) -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "ADMIN_NOT_FOUND",
        "resource not found",
        &request_id(&headers),
    )
}

pub fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(&X_REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("request-id-unavailable")
        .to_owned()
}

pub fn error_response(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    request_id: &str,
) -> Response {
    (
        status,
        Json(AdminErrorBody {
            code,
            message,
            request_id,
        }),
    )
        .into_response()
}
