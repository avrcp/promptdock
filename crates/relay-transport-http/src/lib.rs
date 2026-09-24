#![forbid(unsafe_code)]

use axum::{
    Json,
    body::Body,
    http::{
        HeaderMap, HeaderName, HeaderValue, Request, StatusCode,
        header::{CACHE_CONTROL, PRAGMA},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use uuid::Uuid;

pub const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

pub fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(&X_REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("request-id-unavailable")
        .to_owned()
}

pub async fn assign_request_id(mut request: Request<Body>, next: Next) -> Response {
    let value = HeaderValue::from_str(&Uuid::new_v4().to_string())
        .unwrap_or_else(|_| HeaderValue::from_static("request-id-unavailable"));
    request
        .headers_mut()
        .insert(X_REQUEST_ID.clone(), value.clone());
    let mut response = next.run(request).await;
    response.headers_mut().insert(X_REQUEST_ID.clone(), value);
    response
}

pub fn error_response(status: StatusCode, code: &str, message: &str, request_id: &str) -> Response {
    (
        status,
        Json(ErrorEnvelope {
            error: ErrorBody {
                code,
                message,
                request_id,
            },
        }),
    )
        .into_response()
}

pub fn apply_sensitive_no_store(response: &mut Response) {
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
        .headers_mut()
        .insert(PRAGMA, HeaderValue::from_static("no-cache"));
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    request_id: &'a str,
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, http::StatusCode};

    use super::*;

    #[tokio::test]
    async fn error_envelope_is_closed_and_carries_request_identity() {
        let response = error_response(
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "request forbidden",
            "request-1",
        );
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(
            body.as_ref(),
            br#"{"error":{"code":"FORBIDDEN","message":"request forbidden","requestId":"request-1"}}"#
        );
    }
}
