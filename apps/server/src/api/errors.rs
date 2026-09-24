use axum::{
    body::Body,
    http::{
        HeaderMap, Request, StatusCode,
        header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE, PRAGMA},
    },
    middleware::Next,
    response::Response,
};
use relay_transport_http::apply_sensitive_no_store;

pub(crate) use relay_transport_http::{error_response, request_id};

use crate::{outbox::OutboxError, qr_login::LoginError};

pub(super) async fn normalize_framework_error(request: Request<Body>, next: Next) -> Response {
    let request_id = request_id(request.headers());
    let result_viewer = request.uri().path().starts_with("/r/");
    let response = next.run(request).await;
    // Public result pages intentionally return generic HTML errors with their
    // own no-store/CSP headers. Do not turn those into API JSON and discard
    // the viewer's security envelope.
    if result_viewer {
        return response;
    }
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
    let (code, message) = match status {
        StatusCode::NOT_FOUND => ("NOT_FOUND", "resource not found"),
        StatusCode::METHOD_NOT_ALLOWED => ("METHOD_NOT_ALLOWED", "method not allowed"),
        StatusCode::REQUEST_TIMEOUT => ("REQUEST_TIMEOUT", "request timed out"),
        StatusCode::PAYLOAD_TOO_LARGE => ("BODY_TOO_LARGE", "request body is too large"),
        StatusCode::BAD_REQUEST => ("INVALID_REQUEST", "invalid request"),
        StatusCode::UNAUTHORIZED => ("UNAUTHORIZED", "authentication required"),
        StatusCode::FORBIDDEN => ("FORBIDDEN", "request forbidden"),
        StatusCode::CONFLICT => ("CONFLICT", "request conflict"),
        StatusCode::UNPROCESSABLE_ENTITY => ("VALIDATION_FAILED", "validation failed"),
        StatusCode::TOO_MANY_REQUESTS => ("RATE_LIMITED_LOCAL", "request rate limited"),
        StatusCode::SERVICE_UNAVAILABLE => ("SERVICE_UNAVAILABLE", "service unavailable"),
        StatusCode::INTERNAL_SERVER_ERROR => ("INTERNAL", "internal server error"),
        status if status.is_client_error() => ("INVALID_REQUEST", "invalid request"),
        status if status.is_server_error() => ("INTERNAL", "internal server error"),
        _ => return response,
    };
    let mut normalized = error_response(status, code, message, &request_id);
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

pub(super) fn login_error_response(error: LoginError, request_id: &str) -> Response {
    match error {
        LoginError::NotFound => error_response(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "login session not found",
            request_id,
        ),
        LoginError::CandidateInProgress | LoginError::Terminal | LoginError::VerifyNotAllowed => {
            error_response(
                StatusCode::CONFLICT,
                "LOGIN_CONFLICT",
                "login operation conflicts with the current state",
                request_id,
            )
        }
        LoginError::VerifyLimitReached | LoginError::RateLimited => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED_LOCAL",
            "login operation is locally rate limited",
            request_id,
        ),
        LoginError::InvalidVerifyCode => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "verify code validation failed",
            request_id,
        ),
        LoginError::Clock => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHANNEL_UNAVAILABLE",
            "WeChat login is unavailable",
            request_id,
        ),
    }
}

pub(super) fn wechat_login_response(mut response: Response) -> Response {
    apply_sensitive_no_store(&mut response);
    response
}

pub(super) fn outbox_error_response(error: OutboxError, request_id: &str) -> Response {
    match error {
        OutboxError::Validation => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "notification validation failed",
            request_id,
        ),
        OutboxError::IdempotencyConflict => error_response(
            StatusCode::CONFLICT,
            "IDEMPOTENCY_CONFLICT",
            "notification identifiers conflict with existing content",
            request_id,
        ),
        OutboxError::NotFound => error_response(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "notification not found",
            request_id,
        ),
        OutboxError::QueueFull => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "BUNDLE_QUEUE_FULL",
            "notification bundle queue is full",
            request_id,
        ),
        OutboxError::TargetUnavailable => error_response(
            StatusCode::CONFLICT,
            "TARGET_UNAVAILABLE",
            "notification target is unavailable",
            request_id,
        ),
        OutboxError::Database
        | OutboxError::Clock
        | OutboxError::CryptoUnavailable
        | OutboxError::CorruptContent => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "OUTBOX_UNAVAILABLE",
            "notification service is unavailable",
            request_id,
        ),
    }
}

pub(super) async fn not_found(headers: HeaderMap) -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "NOT_FOUND",
        "resource not found",
        &request_id(&headers),
    )
}
