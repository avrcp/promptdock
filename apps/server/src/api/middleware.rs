use std::time::Duration;

use axum::{
    Router,
    body::Body,
    body::to_bytes,
    extract::{Extension, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Request, StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL, RETRY_AFTER, WWW_AUTHENTICATE},
    },
    middleware::{self, Next},
    response::Response,
};
pub use relay_transport_http::X_REQUEST_ID;
use relay_transport_http::assign_request_id;
use tower_http::{
    catch_panic::CatchPanicLayer,
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer},
};
use tracing::Level;

use crate::{
    auth::{AuthenticatedDevice, AuthenticationError, DeviceScope},
    config::ServerConfig,
    rate_limit::RateLimitOperation,
    state::AppState,
};

pub(super) const MAX_BUNDLE_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy)]
struct BodyLimits {
    regular: usize,
    bundle_or_result: usize,
}

use super::{
    errors::{error_response, normalize_framework_error, request_id, wechat_login_response},
    wechat::is_wechat_login_path,
};

#[derive(Clone)]
pub(super) struct ScopeRequirement(pub(super) &'static [DeviceScope]);

#[derive(Clone)]
pub(super) struct RateMiddlewareState {
    app: AppState,
    operation: RateLimitOperation,
}

impl RateMiddlewareState {
    pub(super) fn fixed(app: AppState, operation: RateLimitOperation) -> Self {
        Self { app, operation }
    }
}

pub(super) async fn require_device(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let is_wechat_login = is_wechat_login_path(request.uri().path());
    let request_id = request_id(request.headers());
    let token = bearer_token(request.headers());
    let identity = match token {
        Some(token) => state.device_auth.authenticate(token).await,
        None => Err(AuthenticationError::Unauthorized),
    };
    let response = match identity {
        Ok(identity) => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Err(AuthenticationError::Unauthorized) => {
            let mut response = error_response(
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "authentication required",
                &request_id,
            );
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            response
        }
        Err(AuthenticationError::Revoked) => error_response(
            StatusCode::FORBIDDEN,
            "DEVICE_REVOKED",
            "device credential is revoked",
            &request_id,
        ),
        Err(AuthenticationError::Database) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "AUTH_UNAVAILABLE",
            "authentication service is unavailable",
            &request_id,
        ),
    };

    if is_wechat_login {
        wechat_login_response(response)
    } else {
        response
    }
}

/// Cloud WeChat connection ownership belongs exclusively to Relay Admin.
/// Device credentials may observe channel health and submit notifications,
/// but can never create, alter, or disconnect the shared provider session.
pub(super) async fn reject_device_channel_management(
    headers: HeaderMap,
    request: Request<Body>,
    _next: Next,
) -> Response {
    let response = error_response(
        StatusCode::FORBIDDEN,
        "CHANNEL_MANAGED_BY_ADMIN",
        "cloud WeChat connection is managed by Relay Admin",
        &request_id(&headers),
    );
    if is_wechat_login_path(request.uri().path()) {
        wechat_login_response(response)
    } else {
        response
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let value = value.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer")
        || token.is_empty()
        || token.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    Some(token)
}

pub(super) fn apply_http_layers(routes: Router, config: &ServerConfig) -> Router {
    let trace = TraceLayer::new_for_http()
        .make_span_with(|request: &Request<Body>| {
            let request_id = request_id(request.headers());
            tracing::info_span!(
                "http.request",
                operation = "http.request",
                request_id = %request_id,
                method = %request.method()
            )
        })
        .on_response(DefaultOnResponse::new().level(Level::INFO))
        .on_failure(DefaultOnFailure::new().level(Level::ERROR));

    routes
        .layer(middleware::from_fn_with_state(
            BodyLimits {
                regular: config.request_body_limit_bytes,
                bundle_or_result: MAX_BUNDLE_REQUEST_BODY_BYTES,
            },
            enforce_request_body_limit,
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(config.route_timeout_seconds),
        ))
        .layer(CatchPanicLayer::new())
        .layer(middleware::from_fn(normalize_framework_error))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(trace)
        .layer(middleware::from_fn(assign_request_id))
        .layer(middleware::from_fn(sensitive_response_headers))
}

async fn enforce_request_body_limit(
    State(limits): State<BodyLimits>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let limit = if request.method() == axum::http::Method::POST
        && matches!(
            request.uri().path(),
            "/v1/notification-bundles" | "/v1/results"
        ) {
        limits.bundle_or_result
    } else {
        limits.regular
    };
    let (parts, body) = request.into_parts();
    match to_bytes(body, limit).await {
        Ok(bytes) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Err(_) => error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "BODY_TOO_LARGE",
            "request body is too large",
            &request_id(&parts.headers),
        ),
    }
}

async fn sensitive_response_headers(request: Request<Body>, next: Next) -> Response {
    let is_wechat_login = is_wechat_login_path(request.uri().path());
    let response = next.run(request).await;
    if is_wechat_login {
        wechat_login_response(response)
    } else {
        response
    }
}

pub(super) async fn require_rate_limit(
    State(state): State<RateMiddlewareState>,
    Extension(device): Extension<AuthenticatedDevice>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(exceeded) = state
        .app
        .rate_limiter
        .check(device.id, state.operation)
        .err()
    else {
        return next.run(request).await;
    };
    let seconds = exceeded.retry_after().as_secs_f64().ceil().max(1.0) as u64;
    let mut response = error_response(
        StatusCode::TOO_MANY_REQUESTS,
        "RATE_LIMITED_LOCAL",
        "request rate limit exceeded",
        &request_id(request.headers()),
    );
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response
}

pub(super) async fn require_scopes(
    State(required): State<ScopeRequirement>,
    Extension(device): Extension<AuthenticatedDevice>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if required.0.iter().all(|scope| device.has_scope(*scope)) {
        return next.run(request).await;
    }
    error_response(
        StatusCode::FORBIDDEN,
        "INSUFFICIENT_SCOPE",
        "request is not permitted",
        &request_id(request.headers()),
    )
}
