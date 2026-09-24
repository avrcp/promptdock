use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{MatchedPath, Path, Query, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, ORIGIN, PRAGMA, VARY},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use relay_admin_api::{
    ADMIN_DEVICE_MANAGE_V2, ADMIN_MAINTENANCE_V2, ADMIN_READ_V2, ADMIN_RESULTS_MANAGE_V2,
    ADMIN_WECHAT_LOGIN_V2, ADMIN_WECHAT_MANAGE_V2, ADMIN_WECHAT_STATUS_V2, AdminPage,
    ResultDetailV2, ResultListItemV2, ResultNotificationStatusV2, ResultPageStateV2,
    ResultReceiptV2, routes,
};
use serde::{Deserialize, Serialize};
use tower_http::{
    catch_panic::CatchPanicLayer,
    limit::RequestBodyLimitLayer,
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer},
};
use tracing::Level;
use uuid::Uuid;

use crate::config::{
    ADMIN_QR_RESPONSE_BODY_BYTES, ADMIN_REQUEST_BODY_BYTES, ADMIN_RESPONSE_BODY_BYTES, AdminMode,
};
use crate::qr_login::{LoginError, LoginSnapshot, LoginState};
use zeroize::Zeroize as _;

use super::{
    ADMIN_API_VERSION, ADMIN_SCHEMA_VERSION, CursorError, CursorKind, CursorPosition,
    command::{
        DeviceActionReceipt, DeviceAdminError, DeviceAdminService, DeviceCredentialReceipt,
        WechatActionReceipt, WechatAdminError, WechatAdminService, WechatTestReceipt,
    },
    error::{error_response, normalize_framework_error, not_found, request_id},
    event_log::{OperationalEvent, OperationalEventKind},
    health::WorkerKey,
    model::AdminMeta,
    query::{
        AdminDevicePageRequest, AdminDeviceQueryError, AdminDeviceQueryService, AdminDeviceState,
        AdminOverviewQueryService, AdminQueueQueryError, AdminQueueQueryService,
        AdminSystemQueryService, AdminWechatQueryService, ChannelEventKind, ChannelEventPosition,
        ChannelEventQuery, DeliveryListQuery, DeliveryOrigin, DeliveryState, InboundCommand,
        InboundCommandListQuery, InboundState, InteractiveReplyListQuery, QueueCursorPosition,
    },
    state::AdminState,
};
use crate::results::{
    PageState, ResultAdminCursor, ResultAdminDetailItem, ResultAdminListItem, ResultAdminPageState,
    ResultError, ResultReceipt,
};

pub const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
pub const X_ADMIN_ACTION: HeaderName = HeaderName::from_static("x-promptdock-admin-action");
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");

pub fn router(state: AdminState) -> Router {
    let mutations = Router::new()
        .route(routes::DEVICES, post(create_device))
        .route(routes::DEVICE_ROTATE, post(rotate_device))
        .route(routes::DEVICE_ENABLE, post(enable_device))
        .route(routes::DEVICE_DISABLE, post(disable_device))
        .route(routes::DEVICE_REVOKE, post(revoke_device))
        .route(routes::RESULT_REVOKE, post(revoke_result))
        .route(routes::WECHAT_TEST, post(test_wechat))
        .route(routes::WECHAT_DISCONNECT, post(disconnect_wechat))
        .route(routes::WECHAT_LOGIN, post(start_admin_wechat_login))
        .route(routes::WECHAT_LOGIN_VERIFY, post(verify_admin_wechat_login))
        .route(
            routes::WECHAT_LOGIN_SESSION,
            axum::routing::delete(cancel_admin_wechat_login),
        )
        .route(routes::MAINTENANCE_RETENTION, post(run_retention))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_mutation_boundary,
        ));
    let v2 = Router::new()
        .route(routes::META, get(meta))
        .route(routes::OVERVIEW, get(overview))
        .route(routes::DEVICES, get(devices))
        .route(routes::DEVICE, get(device_detail))
        .route(routes::WECHAT_STATUS, get(wechat_status))
        .route(routes::WECHAT_EVENTS, get(wechat_events))
        .route(routes::WECHAT_LOGIN_SESSION, get(admin_wechat_login_status))
        .route(routes::DELIVERIES, get(deliveries))
        .route(routes::INTERACTIVE_REPLIES, get(interactive_replies))
        .route(routes::INBOUND_COMMANDS, get(inbound_commands))
        .route(routes::RESULTS, get(results))
        .route(routes::RESULT, get(result_detail))
        .route(routes::SYSTEM, get(system))
        .merge(mutations)
        .fallback(not_found);
    let routes = Router::new()
        .nest(routes::BASE, v2)
        .fallback(not_found)
        .with_state(state.clone());
    apply_http_layers(routes, state)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateDeviceRequest {
    name: String,
    scopes: Vec<crate::auth::DeviceScope>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyMutationRequest {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResultRevokeRequest {
    request_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisconnectWechatRequest {
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartAdminWechatLoginRequest {
    #[serde(default)]
    force_fresh: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyAdminWechatLoginRequest {
    code: String,
}

async fn create_device(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<CreateDeviceRequest>,
) -> Response {
    let service = device_admin_service(&state);
    match service.create_device(&request.name, &request.scopes).await {
        Ok(receipt) => {
            record_device_event(&state, &receipt, OperationalEventKind::DeviceCreated).await;
            (StatusCode::CREATED, Json(receipt)).into_response()
        }
        Err(error) => device_command_error(error, &headers),
    }
}

async fn rotate_device(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(_request): Json<EmptyMutationRequest>,
) -> Response {
    let service = device_admin_service(&state);
    match service.rotate_device_preserving_scopes(&device_id).await {
        Ok(receipt) => {
            record_device_event(&state, &receipt, OperationalEventKind::DeviceRotated).await;
            Json(receipt).into_response()
        }
        Err(error) => device_command_error(error, &headers),
    }
}

async fn enable_device(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(_request): Json<EmptyMutationRequest>,
) -> Response {
    set_device_enabled(state, headers, device_id, true).await
}

async fn disable_device(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(_request): Json<EmptyMutationRequest>,
) -> Response {
    set_device_enabled(state, headers, device_id, false).await
}

async fn set_device_enabled(
    state: AdminState,
    headers: HeaderMap,
    device_id: String,
    enabled: bool,
) -> Response {
    let service = device_admin_service(&state);
    match service.set_device_enabled(&device_id, enabled).await {
        Ok(receipt) => {
            let kind = if enabled {
                OperationalEventKind::DeviceEnabled
            } else {
                OperationalEventKind::DeviceDisabled
            };
            record_action_event(&state, &receipt, kind).await;
            Json(receipt).into_response()
        }
        Err(error) => device_command_error(error, &headers),
    }
}

async fn revoke_device(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(_request): Json<EmptyMutationRequest>,
) -> Response {
    let service = device_admin_service(&state);
    match service.revoke_device(&device_id).await {
        Ok(receipt) => {
            record_action_event(&state, &receipt, OperationalEventKind::DeviceRevoked).await;
            Json(receipt).into_response()
        }
        Err(error) => device_command_error(error, &headers),
    }
}

async fn revoke_result(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(result_row_id): Path<String>,
    Json(request): Json<ResultRevokeRequest>,
) -> Response {
    if result_row_id.is_empty() || request.request_id.is_empty() || request.request_id.len() > 128 {
        return validation_error(&headers);
    }
    match state
        .app
        .results
        .admin_revoke(&result_row_id, &request.request_id)
        .await
    {
        Ok(receipt) => Json(result_receipt(receipt)).into_response(),
        Err(error) => result_error(error, &headers),
    }
}

fn device_admin_service(state: &AdminState) -> DeviceAdminService {
    DeviceAdminService::new(state.app.device_auth.clone(), state.app.gateway.registry())
        .with_wechat_admin_authorization(
            state.app.wechat_admin_login_grants.clone(),
            state.app.admin_login_access.clone(),
            state.app.wechat_login.clone(),
        )
}

async fn run_retention(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(_request): Json<EmptyMutationRequest>,
) -> Response {
    match state.retention.run_once(&state.app.shutdown).await {
        Ok(report) if report.is_complete() => {
            state.health.tick(WorkerKey::Retention);
            let receipt = RetentionActionReceipt {
                receipt_id: Uuid::new_v4().to_string(),
                action: "maintenance.retention",
                outbox_deleted: report.outbox_deleted,
                inbound_deleted: report.inbound_deleted,
                selection_deleted: report.selection_deleted,
                started_at: report.started_at,
                completed_at: report.completed_at,
            };
            state
                .events
                .record(OperationalEvent {
                    id: receipt.receipt_id.clone(),
                    kind: OperationalEventKind::RetentionCompleted,
                    message: "Retention pass completed",
                    occurred_at: receipt.completed_at,
                })
                .await;
            Json(receipt).into_response()
        }
        Ok(_) | Err(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "ADMIN_MAINTENANCE_UNAVAILABLE",
            "maintenance action is unavailable",
            &request_id(&headers),
        ),
    }
}

async fn test_wechat(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(_request): Json<EmptyMutationRequest>,
) -> Response {
    let service = match wechat_admin_service(&state) {
        Ok(service) => service,
        Err(error) => return wechat_command_error(error, &headers),
    };
    match service.enqueue_test().await {
        Ok(receipt) => {
            record_wechat_test_event(&state, &receipt).await;
            (StatusCode::ACCEPTED, Json(receipt)).into_response()
        }
        Err(error) => wechat_command_error(error, &headers),
    }
}

async fn disconnect_wechat(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<DisconnectWechatRequest>,
) -> Response {
    let service = match wechat_admin_service(&state) {
        Ok(service) => service,
        Err(error) => return wechat_command_error(error, &headers),
    };
    match service.disconnect(&request.reason).await {
        Ok(receipt) => {
            record_wechat_disconnect_event(&state, &receipt).await;
            Json(receipt).into_response()
        }
        Err(error) => wechat_command_error(error, &headers),
    }
}

async fn start_admin_wechat_login(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(request): Json<StartAdminWechatLoginRequest>,
) -> Response {
    let login = match admin_wechat_login_service(&state) {
        Ok(service) => service,
        Err(error) => return admin_login_route_error(error, &headers),
    };
    let outcome = match login.start_operator(request.force_fresh) {
        Ok(outcome) => outcome,
        Err(error) => return admin_login_error(error, &headers),
    };
    record_admin_login_event(&state, OperationalEventKind::WechatLoginStarted).await;
    Json(outcome.snapshot).into_response()
}

async fn admin_wechat_login_status(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(login_id): Path<String>,
) -> Response {
    let login = match admin_wechat_login_service(&state) {
        Ok(service) => service,
        Err(error) => return admin_login_route_error(error, &headers),
    };
    let login_id = match Uuid::parse_str(&login_id) {
        Ok(login_id) => login_id,
        Err(_) => return admin_login_not_found(&headers),
    };
    match login.operator_snapshot(login_id) {
        Ok(snapshot) => admin_login_snapshot_response(snapshot),
        Err(error) => admin_login_error(error, &headers),
    }
}

async fn verify_admin_wechat_login(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(login_id): Path<String>,
    Json(mut request): Json<VerifyAdminWechatLoginRequest>,
) -> Response {
    if request.code.is_empty()
        || request.code.len() > 16
        || !request.code.bytes().all(|byte| byte.is_ascii_digit())
    {
        request.code.zeroize();
        return admin_login_validation_error(&headers);
    }
    let login = match admin_wechat_login_service(&state) {
        Ok(service) => service,
        Err(error) => return admin_login_route_error(error, &headers),
    };
    let login_id = match Uuid::parse_str(&login_id) {
        Ok(login_id) => login_id,
        Err(_) => {
            request.code.zeroize();
            return admin_login_not_found(&headers);
        }
    };
    let result = login.operator_verify(login_id, &request.code);
    request.code.zeroize();
    match result {
        Ok(snapshot) => {
            record_admin_login_event(&state, OperationalEventKind::WechatVerifySubmitted).await;
            admin_login_snapshot_response(snapshot)
        }
        Err(error) => admin_login_error(error, &headers),
    }
}

async fn cancel_admin_wechat_login(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(login_id): Path<String>,
    Json(_request): Json<EmptyMutationRequest>,
) -> Response {
    let login = match admin_wechat_login_service(&state) {
        Ok(service) => service,
        Err(error) => return admin_login_route_error(error, &headers),
    };
    let login_id = match Uuid::parse_str(&login_id) {
        Ok(login_id) => login_id,
        Err(_) => return admin_login_not_found(&headers),
    };
    match login.operator_cancel(login_id) {
        Ok(snapshot) => {
            if snapshot.state == LoginState::Cancelled {
                record_admin_login_event(&state, OperationalEventKind::WechatLoginCancelled).await;
            }
            admin_login_snapshot_response(snapshot)
        }
        Err(error) => admin_login_error(error, &headers),
    }
}

fn admin_wechat_login_service(
    state: &AdminState,
) -> Result<crate::wechat_login::WechatLoginService, AdminLoginRouteError> {
    if state.config.mode != AdminMode::Operator || !state.config.wechat_enabled {
        return Err(AdminLoginRouteError::Unavailable);
    }
    state
        .app
        .wechat_login
        .clone()
        .ok_or(AdminLoginRouteError::Unavailable)
}

fn admin_login_snapshot_response(snapshot: LoginSnapshot) -> Response {
    Json(snapshot).into_response()
}

fn admin_login_error(error: LoginError, headers: &HeaderMap) -> Response {
    let (status, code, message) = match error {
        LoginError::NotFound => (
            StatusCode::NOT_FOUND,
            "ADMIN_WECHAT_LOGIN_NOT_FOUND",
            "WeChat login was not found",
        ),
        LoginError::CandidateInProgress | LoginError::Terminal | LoginError::VerifyNotAllowed => (
            StatusCode::CONFLICT,
            "ADMIN_WECHAT_LOGIN_CONFLICT",
            "WeChat login conflicts with its current state",
        ),
        LoginError::VerifyLimitReached | LoginError::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "ADMIN_WECHAT_LOGIN_RATE_LIMITED",
            "WeChat login is locally rate limited",
        ),
        LoginError::InvalidVerifyCode => (
            StatusCode::BAD_REQUEST,
            "ADMIN_VALIDATION_FAILED",
            "WeChat verification validation failed",
        ),
        LoginError::Clock => (
            StatusCode::SERVICE_UNAVAILABLE,
            "ADMIN_WECHAT_UNAVAILABLE",
            "WeChat login is unavailable",
        ),
    };
    error_response(status, code, message, &request_id(headers))
}

fn admin_login_validation_error(headers: &HeaderMap) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "ADMIN_VALIDATION_FAILED",
        "WeChat login request validation failed",
        &request_id(headers),
    )
}

fn admin_login_not_found(headers: &HeaderMap) -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "ADMIN_WECHAT_LOGIN_NOT_FOUND",
        "WeChat login was not found",
        &request_id(headers),
    )
}

fn admin_login_unavailable(headers: &HeaderMap) -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "ADMIN_WECHAT_UNAVAILABLE",
        "WeChat login is unavailable",
        &request_id(headers),
    )
}

#[derive(Clone, Copy)]
enum AdminLoginRouteError {
    Unavailable,
}

fn admin_login_route_error(error: AdminLoginRouteError, headers: &HeaderMap) -> Response {
    match error {
        AdminLoginRouteError::Unavailable => admin_login_unavailable(headers),
    }
}

fn wechat_admin_service(state: &AdminState) -> Result<WechatAdminService, WechatAdminError> {
    if state.config.mode != AdminMode::Operator || !state.config.wechat_enabled {
        return Err(WechatAdminError::Unavailable);
    }
    let login = state
        .app
        .wechat_login
        .clone()
        .ok_or(WechatAdminError::Unavailable)?;
    Ok(WechatAdminService::new(
        state.app.outbox.clone(),
        Some(login),
    ))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RetentionActionReceipt {
    receipt_id: String,
    action: &'static str,
    outbox_deleted: u64,
    inbound_deleted: u64,
    selection_deleted: u64,
    started_at: i64,
    completed_at: i64,
}

async fn record_device_event(
    state: &AdminState,
    receipt: &DeviceCredentialReceipt,
    kind: OperationalEventKind,
) {
    state
        .events
        .record(OperationalEvent {
            id: receipt.receipt_id.clone(),
            kind,
            message: "Device credential lifecycle changed",
            occurred_at: receipt.issued_at,
        })
        .await;
}

async fn record_action_event(
    state: &AdminState,
    receipt: &DeviceActionReceipt,
    kind: OperationalEventKind,
) {
    state
        .events
        .record(OperationalEvent {
            id: receipt.receipt_id.clone(),
            kind,
            message: "Device lifecycle changed",
            occurred_at: receipt.completed_at,
        })
        .await;
}

async fn record_wechat_test_event(state: &AdminState, receipt: &WechatTestReceipt) {
    state
        .events
        .record(OperationalEvent {
            id: receipt.receipt_id.clone(),
            kind: OperationalEventKind::WechatTestEnqueued,
            message: "WeChat test notification was queued",
            occurred_at: receipt.issued_at,
        })
        .await;
}

async fn record_wechat_disconnect_event(state: &AdminState, receipt: &WechatActionReceipt) {
    state
        .events
        .record(OperationalEvent {
            id: receipt.receipt_id.clone(),
            kind: OperationalEventKind::WechatDisconnected,
            message: "WeChat channel lifecycle changed",
            occurred_at: receipt.completed_at,
        })
        .await;
}

async fn record_admin_login_event(state: &AdminState, kind: OperationalEventKind) {
    let message = match kind {
        OperationalEventKind::WechatLoginStarted => "WeChat login start was accepted",
        OperationalEventKind::WechatVerifySubmitted => "WeChat login verification was submitted",
        OperationalEventKind::WechatLoginCancelled => "WeChat login was cancelled",
        _ => unreachable!("login event helper only accepts login lifecycle kinds"),
    };
    state
        .events
        .record(OperationalEvent {
            id: Uuid::new_v4().to_string(),
            kind,
            message,
            occurred_at: generated_at().unwrap_or(state.started_at_wall_ms),
        })
        .await;
}

fn device_command_error(error: DeviceAdminError, headers: &HeaderMap) -> Response {
    let (status, code, message) = match error {
        DeviceAdminError::InvalidName
        | DeviceAdminError::InvalidDeviceId
        | DeviceAdminError::InvalidScopes => (
            StatusCode::BAD_REQUEST,
            "ADMIN_VALIDATION_FAILED",
            "device request validation failed",
        ),
        DeviceAdminError::NotFound => (
            StatusCode::NOT_FOUND,
            "ADMIN_DEVICE_NOT_FOUND",
            "device not found",
        ),
        DeviceAdminError::RevokedConflict => (
            StatusCode::CONFLICT,
            "ADMIN_DEVICE_STATE_CONFLICT",
            "device state conflicts with this action",
        ),
        DeviceAdminError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "ADMIN_COMMAND_UNAVAILABLE",
            "admin command is unavailable",
        ),
    };
    error_response(status, code, message, &request_id(headers))
}

fn wechat_command_error(error: WechatAdminError, headers: &HeaderMap) -> Response {
    let (status, code, message) = match error {
        WechatAdminError::InvalidRequest => (
            StatusCode::BAD_REQUEST,
            "ADMIN_VALIDATION_FAILED",
            "WeChat request validation failed",
        ),
        WechatAdminError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "ADMIN_WECHAT_UNAVAILABLE",
            "WeChat action is unavailable",
        ),
    };
    error_response(status, code, message, &request_id(headers))
}

async fn overview(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    match AdminOverviewQueryService::new(&state)
        .snapshot(state.started_at)
        .await
    {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(_) => query_unavailable(&headers),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceListParameters {
    search: Option<String>,
    state: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

async fn devices(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(parameters): Query<DeviceListParameters>,
) -> Response {
    let after = match decode_optional_cursor(&state, CursorKind::Devices, parameters.cursor).await {
        Ok(after) => after,
        Err(error) => return cursor_error(error, &headers),
    };
    let state_filter = match parameters.state.as_deref().map(parse_device_state) {
        Some(Ok(value)) => Some(value),
        Some(Err(())) => return validation_error(&headers),
        None => None,
    };
    let service = AdminDeviceQueryService::new(
        state.app.db.clone(),
        state.app.gateway.registry(),
        state.config.max_page_size,
    );
    match service
        .page(AdminDevicePageRequest {
            after,
            limit: parameters.limit,
            search: parameters.search,
            state: state_filter,
        })
        .await
    {
        Ok(page) => {
            let next_cursor =
                match encode_optional_cursor(&state, CursorKind::Devices, page.next_position).await
                {
                    Ok(cursor) => cursor,
                    Err(_) => return query_unavailable(&headers),
                };
            page_response(page.items, next_cursor, &headers)
        }
        Err(AdminDeviceQueryError::InvalidPage | AdminDeviceQueryError::InvalidSearch) => {
            validation_error(&headers)
        }
        Err(AdminDeviceQueryError::InvalidDeviceId) => validation_error(&headers),
        Err(AdminDeviceQueryError::Database | AdminDeviceQueryError::InvalidProjection) => {
            query_unavailable(&headers)
        }
    }
}

fn parse_device_state(value: &str) -> Result<AdminDeviceState, ()> {
    match value {
        "online" => Ok(AdminDeviceState::Online),
        "offline" => Ok(AdminDeviceState::Offline),
        "disabled" => Ok(AdminDeviceState::Disabled),
        "revoked" => Ok(AdminDeviceState::Revoked),
        "unknown" => Err(()),
        _ => Err(()),
    }
}

async fn device_detail(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Response {
    let service = AdminDeviceQueryService::new(
        state.app.db.clone(),
        state.app.gateway.registry(),
        state.config.max_page_size,
    );
    match service.detail(&device_id).await {
        Ok(Some(device)) => Json(device).into_response(),
        Ok(None) => error_response(
            StatusCode::NOT_FOUND,
            "ADMIN_DEVICE_NOT_FOUND",
            "device not found",
            &request_id(&headers),
        ),
        Err(AdminDeviceQueryError::InvalidDeviceId) => validation_error(&headers),
        Err(_) => query_unavailable(&headers),
    }
}

async fn wechat_status(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let service = AdminWechatQueryService::new(
        state.app.clone(),
        state.events.clone(),
        state.config.max_page_size,
    );
    match service.status().await {
        Ok(status) => Json(status).into_response(),
        Err(_) => query_unavailable(&headers),
    }
}

#[derive(Debug, Default)]
struct ChannelEventParameters {
    kinds: std::collections::BTreeSet<ChannelEventKind>,
    since_at: Option<i64>,
    limit: Option<usize>,
    cursor: Option<String>,
}

async fn wechat_events(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(parameters): Query<Vec<(String, String)>>,
) -> Response {
    let parameters = match parse_channel_event_parameters(parameters) {
        Ok(parameters) => parameters,
        Err(()) => return validation_error(&headers),
    };
    let before =
        match decode_optional_cursor(&state, CursorKind::WechatEvents, parameters.cursor).await {
            Ok(Some(position)) => Some(ChannelEventPosition {
                occurred_at: position.sort_timestamp,
                id: position.tie_break,
            }),
            Ok(None) => None,
            Err(error) => return cursor_error(error, &headers),
        };
    let service = AdminWechatQueryService::new(
        state.app.clone(),
        state.events.clone(),
        state.config.max_page_size,
    );
    match service
        .events(ChannelEventQuery {
            kinds: parameters.kinds,
            since_at: parameters.since_at,
            limit: parameters.limit.unwrap_or(50),
            before,
        })
        .await
    {
        Ok(page) => {
            let position = page
                .items
                .last()
                .filter(|_| page.has_more)
                .map(|item| CursorPosition {
                    sort_timestamp: item.occurred_at,
                    tie_break: item.id.clone(),
                });
            let next_cursor =
                match encode_optional_cursor(&state, CursorKind::WechatEvents, position).await {
                    Ok(cursor) => cursor,
                    Err(_) => return query_unavailable(&headers),
                };
            page_response(page.items, next_cursor, &headers)
        }
        Err(super::query::WechatQueryError::InvalidQuery) => validation_error(&headers),
        Err(_) => query_unavailable(&headers),
    }
}

async fn system(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let service = AdminSystemQueryService::new(
        state.app.db.clone(),
        state.health.clone(),
        (*state.config).clone(),
    );
    match service.snapshot().await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(_) => query_unavailable(&headers),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeliveryParameters {
    state: Option<String>,
    origin: Option<String>,
    since_at: Option<i64>,
    until_at: Option<i64>,
    limit: Option<usize>,
    cursor: Option<String>,
}

async fn deliveries(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(parameters): Query<DeliveryParameters>,
) -> Response {
    let state_filter = match parameters.state.as_deref().map(parse_delivery_state) {
        Some(Ok(value)) => Some(value),
        Some(Err(())) => return validation_error(&headers),
        None => None,
    };
    let origin = match parameters.origin.as_deref().map(parse_delivery_origin) {
        Some(Ok(value)) => Some(value),
        Some(Err(())) => return validation_error(&headers),
        None => None,
    };
    let cursor = match queue_cursor(&state, CursorKind::Deliveries, parameters.cursor).await {
        Ok(cursor) => cursor,
        Err(error) => return cursor_error(error, &headers),
    };
    let service = AdminQueueQueryService::new(state.app.db.clone(), state.config.max_page_size);
    match service
        .list_deliveries(DeliveryListQuery {
            state: state_filter,
            origin,
            since_at: parameters.since_at,
            until_at: parameters.until_at,
            limit: parameters.limit.unwrap_or(50),
            cursor,
        })
        .await
    {
        Ok(page) => {
            queue_page_response(
                &state,
                CursorKind::Deliveries,
                page.items,
                page.next_cursor_position,
                &headers,
            )
            .await
        }
        Err(AdminQueueQueryError::InvalidQuery | AdminQueueQueryError::InvalidCursor) => {
            validation_error(&headers)
        }
        Err(_) => query_unavailable(&headers),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InteractiveReplyParameters {
    state: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

async fn interactive_replies(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(parameters): Query<InteractiveReplyParameters>,
) -> Response {
    let state_filter = match parameters.state.as_deref().map(parse_delivery_state) {
        Some(Ok(value)) => Some(value),
        Some(Err(())) => return validation_error(&headers),
        None => None,
    };
    let cursor = match queue_cursor(&state, CursorKind::InteractiveReplies, parameters.cursor).await
    {
        Ok(cursor) => cursor,
        Err(error) => return cursor_error(error, &headers),
    };
    let service = AdminQueueQueryService::new(state.app.db.clone(), state.config.max_page_size);
    match service
        .list_interactive_replies(InteractiveReplyListQuery {
            state: state_filter,
            limit: parameters.limit.unwrap_or(50),
            cursor,
        })
        .await
    {
        Ok(page) => {
            queue_page_response(
                &state,
                CursorKind::InteractiveReplies,
                page.items,
                page.next_cursor_position,
                &headers,
            )
            .await
        }
        Err(AdminQueueQueryError::InvalidQuery | AdminQueueQueryError::InvalidCursor) => {
            validation_error(&headers)
        }
        Err(_) => query_unavailable(&headers),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InboundParameters {
    command: Option<String>,
    state: Option<String>,
    since_at: Option<i64>,
    limit: Option<usize>,
    cursor: Option<String>,
}

async fn inbound_commands(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(parameters): Query<InboundParameters>,
) -> Response {
    let command = match parameters.command.as_deref().map(parse_inbound_command) {
        Some(Ok(value)) => Some(value),
        Some(Err(())) => return validation_error(&headers),
        None => None,
    };
    let state_filter = match parameters.state.as_deref().map(parse_inbound_state) {
        Some(Ok(value)) => Some(value),
        Some(Err(())) => return validation_error(&headers),
        None => None,
    };
    let cursor = match queue_cursor(&state, CursorKind::InboundCommands, parameters.cursor).await {
        Ok(cursor) => cursor,
        Err(error) => return cursor_error(error, &headers),
    };
    let service = AdminQueueQueryService::new(state.app.db.clone(), state.config.max_page_size);
    match service
        .list_inbound_commands(InboundCommandListQuery {
            command,
            state: state_filter,
            since_at: parameters.since_at,
            limit: parameters.limit.unwrap_or(50),
            cursor,
        })
        .await
    {
        Ok(page) => {
            queue_page_response(
                &state,
                CursorKind::InboundCommands,
                page.items,
                page.next_cursor_position,
                &headers,
            )
            .await
        }
        Err(AdminQueueQueryError::InvalidQuery | AdminQueueQueryError::InvalidCursor) => {
            validation_error(&headers)
        }
        Err(_) => query_unavailable(&headers),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResultsParameters {
    page_state: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

async fn results(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(parameters): Query<ResultsParameters>,
) -> Response {
    let wanted_state = match parameters
        .page_state
        .as_deref()
        .map(parse_result_page_state)
    {
        Some(Ok(value)) => Some(value),
        Some(Err(())) => return validation_error(&headers),
        None => None,
    };
    let limit = parameters.limit.unwrap_or(50);
    if limit == 0 || limit > state.config.max_page_size {
        return validation_error(&headers);
    }
    let after = match decode_optional_cursor(&state, CursorKind::Results, parameters.cursor).await {
        Ok(position) => position.map(|position| ResultAdminCursor {
            accepted_at: position.sort_timestamp,
            result_row_id: position.tie_break,
        }),
        Err(error) => return cursor_error(error, &headers),
    };
    let page_state = wanted_state.map(result_admin_page_state);
    match state
        .app
        .results
        .admin_list_page(limit, after, page_state)
        .await
    {
        Ok(page) => {
            let items = page.items.into_iter().map(result_list_item).collect();
            let next = page.next.map(|position| CursorPosition {
                sort_timestamp: position.accepted_at,
                tie_break: position.result_row_id,
            });
            match encode_optional_cursor(&state, CursorKind::Results, next).await {
                Ok(cursor) => page_response(items, cursor, &headers),
                Err(error) => cursor_error(error, &headers),
            }
        }
        Err(error) => result_error(error, &headers),
    }
}

fn result_admin_page_state(value: ResultPageStateV2) -> ResultAdminPageState {
    match value {
        ResultPageStateV2::Available => ResultAdminPageState::Available,
        ResultPageStateV2::Revoked => ResultAdminPageState::Revoked,
        ResultPageStateV2::Expired => ResultAdminPageState::Expired,
        ResultPageStateV2::ContentUnavailable => ResultAdminPageState::ContentUnavailable,
    }
}

async fn result_detail(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(result_row_id): Path<String>,
) -> Response {
    if result_row_id.is_empty() {
        return validation_error(&headers);
    }
    match state.app.results.admin_detail(&result_row_id).await {
        Ok(item) => Json(result_detail_item(item)).into_response(),
        Err(error) => result_error(error, &headers),
    }
}

fn parse_result_page_state(value: &str) -> Result<ResultPageStateV2, ()> {
    match value {
        "available" => Ok(ResultPageStateV2::Available),
        "revoked" => Ok(ResultPageStateV2::Revoked),
        "expired" => Ok(ResultPageStateV2::Expired),
        "content_unavailable" => Ok(ResultPageStateV2::ContentUnavailable),
        _ => Err(()),
    }
}

fn result_page_state(item: &ResultAdminListItem) -> ResultPageStateV2 {
    if item.revoked_at.is_some() {
        ResultPageStateV2::Revoked
    } else if generated_at().is_some_and(|now| now >= item.page_expires_at) {
        ResultPageStateV2::Expired
    } else if item.body_purged_at.is_some() {
        ResultPageStateV2::ContentUnavailable
    } else {
        ResultPageStateV2::Available
    }
}

fn result_notification_status(value: &str) -> ResultNotificationStatusV2 {
    match value {
        "pending_channel" => ResultNotificationStatusV2::PendingChannel,
        "sending_channel" => ResultNotificationStatusV2::SendingChannel,
        "retry_wait" => ResultNotificationStatusV2::RetryWait,
        "provider_accepted" => ResultNotificationStatusV2::ProviderAccepted,
        "blocked_activation" => ResultNotificationStatusV2::BlockedActivation,
        "blocked_reconnect" => ResultNotificationStatusV2::BlockedReconnect,
        "dead_letter" => ResultNotificationStatusV2::DeadLetter,
        "expired" => ResultNotificationStatusV2::Expired,
        _ => ResultNotificationStatusV2::Cancelled,
    }
}

fn result_list_item(item: ResultAdminListItem) -> ResultListItemV2 {
    let page_state = result_page_state(&item);
    ResultListItemV2 {
        result_row_id: item.result_row_id,
        result_id: item.result_id,
        owner_device_id: item.owner_device_id,
        safe_title: item.safe_title,
        source: item.source,
        content_mode: item.content_mode,
        source_hash: item.source_hash,
        accepted_at: item.accepted_at,
        updated_at: item.updated_at,
        page_state,
        page_expires_at: item.page_expires_at,
        body_bytes: u64::try_from(item.body_bytes).unwrap_or(0),
        body_purged_at: item.body_purged_at,
        notification_id: item.notification_id,
        notification_status: result_notification_status(&item.notification_status),
        notification_attempt_count: u32::try_from(item.notification_attempt_count).unwrap_or(0),
        notification_last_error_code: item.notification_last_error_code,
        target_account_fingerprint: item.target_account_fingerprint,
    }
}

fn result_detail_item(item: ResultAdminDetailItem) -> ResultDetailV2 {
    let ResultAdminDetailItem {
        item,
        correlation_key,
        result_revision,
        started_at,
        completed_at,
        duration_ms,
    } = item;
    let revoked_at = item.revoked_at;
    let row = result_list_item(item);
    ResultDetailV2 {
        result_row_id: row.result_row_id,
        result_id: row.result_id,
        owner_device_id: row.owner_device_id,
        safe_title: row.safe_title,
        source: row.source,
        content_mode: row.content_mode,
        source_hash: row.source_hash,
        accepted_at: row.accepted_at,
        updated_at: row.updated_at,
        page_state: row.page_state,
        page_expires_at: row.page_expires_at,
        body_bytes: row.body_bytes,
        body_purged_at: row.body_purged_at,
        notification_id: row.notification_id,
        notification_status: row.notification_status,
        notification_attempt_count: row.notification_attempt_count,
        notification_last_error_code: row.notification_last_error_code,
        target_account_fingerprint: row.target_account_fingerprint,
        correlation_key,
        result_revision,
        started_at,
        completed_at,
        duration_ms,
        revoked_at,
    }
}

fn result_receipt(receipt: ResultReceipt) -> ResultReceiptV2 {
    ResultReceiptV2 {
        schema_version: u32::try_from(receipt.schema_version).unwrap_or(1),
        result_id: receipt.result_id,
        source_hash: receipt.source_hash,
        accepted_at: receipt.accepted_at,
        updated_at: receipt.updated_at,
        page_state: match receipt.page_state {
            PageState::Available => ResultPageStateV2::Available,
            PageState::Revoked => ResultPageStateV2::Revoked,
            PageState::Expired => ResultPageStateV2::Expired,
            PageState::ContentUnavailable => ResultPageStateV2::ContentUnavailable,
        },
        page_expires_at: receipt.page_expires_at,
        notification_id: receipt.notification_id,
        notification_status: result_notification_status(&receipt.notification_status),
    }
}

fn result_error(error: ResultError, headers: &HeaderMap) -> Response {
    let (status, code, message) = match error {
        ResultError::Validation => (
            StatusCode::BAD_REQUEST,
            "ADMIN_VALIDATION_FAILED",
            "result request validation failed",
        ),
        ResultError::NotFound => (
            StatusCode::NOT_FOUND,
            "ADMIN_RESULT_NOT_FOUND",
            "result not found",
        ),
        ResultError::Conflict => (
            StatusCode::CONFLICT,
            "ADMIN_RESULT_CONFLICT",
            "result action conflicts with current state",
        ),
        ResultError::Inaccessible => (
            StatusCode::FORBIDDEN,
            "ADMIN_RESULT_INACCESSIBLE",
            "result is inaccessible",
        ),
        ResultError::Capacity => (
            StatusCode::TOO_MANY_REQUESTS,
            "ADMIN_RESULT_CAPACITY_REACHED",
            "result capacity is exhausted",
        ),
        ResultError::TargetUnavailable
        | ResultError::Unavailable
        | ResultError::Database
        | ResultError::Clock => (
            StatusCode::SERVICE_UNAVAILABLE,
            "ADMIN_RESULTS_UNAVAILABLE",
            "result administration is unavailable",
        ),
    };
    error_response(status, code, message, &request_id(headers))
}

async fn queue_cursor(
    state: &AdminState,
    kind: CursorKind,
    cursor: Option<String>,
) -> Result<Option<QueueCursorPosition>, CursorError> {
    decode_optional_cursor(state, kind, cursor)
        .await
        .map(|position| {
            position.map(|position| QueueCursorPosition {
                sort_timestamp: position.sort_timestamp,
                tie_break: position.tie_break,
            })
        })
}

async fn queue_page_response<T: Serialize>(
    state: &AdminState,
    kind: CursorKind,
    items: Vec<T>,
    position: Option<QueueCursorPosition>,
    headers: &HeaderMap,
) -> Response {
    let position = position.map(|position| CursorPosition {
        sort_timestamp: position.sort_timestamp,
        tie_break: position.tie_break,
    });
    match encode_optional_cursor(state, kind, position).await {
        Ok(cursor) => page_response(items, cursor, headers),
        Err(_) => query_unavailable(headers),
    }
}

fn parse_delivery_state(value: &str) -> Result<DeliveryState, ()> {
    match value {
        "queued" => Ok(DeliveryState::Queued),
        "sending" => Ok(DeliveryState::Sending),
        "retrying" => Ok(DeliveryState::Retrying),
        "blocked" => Ok(DeliveryState::Blocked),
        "accepted" => Ok(DeliveryState::Accepted),
        "failed" => Ok(DeliveryState::Failed),
        "cancelled" => Ok(DeliveryState::Cancelled),
        "expired" => Ok(DeliveryState::Expired),
        _ => Err(()),
    }
}

fn parse_delivery_origin(value: &str) -> Result<DeliveryOrigin, ()> {
    match value {
        "device" => Ok(DeliveryOrigin::Device),
        "system" => Ok(DeliveryOrigin::System),
        "admin" => Ok(DeliveryOrigin::Admin),
        _ => Err(()),
    }
}

fn parse_inbound_command(value: &str) -> Result<InboundCommand, ()> {
    match value {
        "help" => Ok(InboundCommand::Help),
        "list_devices" => Ok(InboundCommand::ListDevices),
        "list_jobs" => Ok(InboundCommand::ListJobs),
        "list_recent" => Ok(InboundCommand::ListRecent),
        "list_failed" => Ok(InboundCommand::ListFailed),
        "next_page" => Ok(InboundCommand::NextPage),
        "get_status" => Ok(InboundCommand::GetStatus),
        "get_detail" => Ok(InboundCommand::GetDetail),
        "get_tree" => Ok(InboundCommand::GetTree),
        "unknown" => Ok(InboundCommand::Unknown),
        _ => Err(()),
    }
}

fn parse_inbound_state(value: &str) -> Result<InboundState, ()> {
    match value {
        "received" => Ok(InboundState::Received),
        "dispatching" => Ok(InboundState::Dispatching),
        "waiting_gateway" => Ok(InboundState::WaitingGateway),
        "reply_queued" => Ok(InboundState::ReplyQueued),
        "expired" => Ok(InboundState::Expired),
        "dead_letter" => Ok(InboundState::DeadLetter),
        _ => Err(()),
    }
}

fn parse_channel_event_parameters(
    pairs: Vec<(String, String)>,
) -> Result<ChannelEventParameters, ()> {
    let mut parameters = ChannelEventParameters::default();
    for (key, value) in pairs {
        match key.as_str() {
            "kinds" => {
                for kind in value.split(',') {
                    parameters.kinds.insert(parse_event_kind(kind)?);
                }
            }
            "sinceAt" if parameters.since_at.is_none() => {
                parameters.since_at = Some(value.parse().map_err(|_| ())?);
            }
            "limit" if parameters.limit.is_none() => {
                parameters.limit = Some(value.parse().map_err(|_| ())?);
            }
            "cursor" if parameters.cursor.is_none() => parameters.cursor = Some(value),
            _ => return Err(()),
        }
    }
    Ok(parameters)
}

fn parse_event_kind(value: &str) -> Result<ChannelEventKind, ()> {
    match value {
        "poll" => Ok(ChannelEventKind::Poll),
        "context" => Ok(ChannelEventKind::Context),
        "login" => Ok(ChannelEventKind::Login),
        "login_cancelled" => Ok(ChannelEventKind::LoginCancelled),
        "login_expired" => Ok(ChannelEventKind::LoginExpired),
        "login_failed" => Ok(ChannelEventKind::LoginFailed),
        "disconnect" => Ok(ChannelEventKind::Disconnect),
        "reconnect" => Ok(ChannelEventKind::Reconnect),
        "test" => Ok(ChannelEventKind::Test),
        _ => Err(()),
    }
}

async fn decode_optional_cursor(
    state: &AdminState,
    kind: CursorKind,
    cursor: Option<String>,
) -> Result<Option<CursorPosition>, CursorError> {
    match cursor.filter(|cursor| !cursor.is_empty()) {
        Some(cursor) => state.cursors.decode(kind, &cursor).await.map(Some),
        None => Ok(None),
    }
}

async fn encode_optional_cursor(
    state: &AdminState,
    kind: CursorKind,
    position: Option<CursorPosition>,
) -> Result<Option<String>, CursorError> {
    match position {
        Some(position) => state.cursors.encode(kind, position).await.map(Some),
        None => Ok(None),
    }
}

fn cursor_error(error: CursorError, headers: &HeaderMap) -> Response {
    let (code, message) = match error {
        CursorError::Expired => ("ADMIN_CURSOR_EXPIRED", "cursor expired"),
        CursorError::Invalid => ("ADMIN_CURSOR_INVALID", "cursor is invalid"),
        CursorError::Clock => return query_unavailable(headers),
    };
    error_response(StatusCode::BAD_REQUEST, code, message, &request_id(headers))
}

fn validation_error(headers: &HeaderMap) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "ADMIN_VALIDATION_FAILED",
        "request validation failed",
        &request_id(headers),
    )
}

fn query_unavailable(headers: &HeaderMap) -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "ADMIN_QUERY_UNAVAILABLE",
        "admin query is unavailable",
        &request_id(headers),
    )
}

fn page_response<T: Serialize>(
    items: Vec<T>,
    next_cursor: Option<String>,
    headers: &HeaderMap,
) -> Response {
    let Some(generated_at) = generated_at() else {
        return query_unavailable(headers);
    };
    Json(AdminPage {
        items,
        next_cursor,
        total: None,
        generated_at,
    })
    .into_response()
}

fn generated_at() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

async fn meta(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let generated_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok());
    let Some(generated_at) = generated_at else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "ADMIN_CLOCK_UNAVAILABLE",
            "server clock is unavailable",
            &request_id(&headers),
        );
    };
    let capabilities = if state.config.mode == AdminMode::Operator
        && state.config.wechat_enabled
        && state.app.wechat_login.is_some()
    {
        vec![
            ADMIN_READ_V2,
            ADMIN_DEVICE_MANAGE_V2,
            ADMIN_WECHAT_STATUS_V2,
            ADMIN_WECHAT_MANAGE_V2,
            ADMIN_WECHAT_LOGIN_V2,
            ADMIN_MAINTENANCE_V2,
            ADMIN_RESULTS_MANAGE_V2,
        ]
    } else if state.config.mode == AdminMode::Operator {
        vec![
            ADMIN_READ_V2,
            ADMIN_DEVICE_MANAGE_V2,
            ADMIN_WECHAT_STATUS_V2,
            ADMIN_MAINTENANCE_V2,
            ADMIN_RESULTS_MANAGE_V2,
        ]
    } else {
        vec![ADMIN_READ_V2, ADMIN_WECHAT_STATUS_V2]
    };
    Json(AdminMeta {
        schema_version: ADMIN_SCHEMA_VERSION,
        generated_at,
        admin_api_version: ADMIN_API_VERSION,
        relay_version: env!("CARGO_PKG_VERSION"),
        capabilities,
    })
    .into_response()
}

fn apply_http_layers(routes: Router, state: AdminState) -> Router {
    let trace = TraceLayer::new_for_http()
        .make_span_with(|request: &Request<Body>| {
            let request_id = request_id(request.headers());
            let matched_route = request
                .extensions()
                .get::<MatchedPath>()
                .map(MatchedPath::as_str)
                .unwrap_or("unmatched");
            tracing::info_span!(
                "admin.http.request",
                operation = "admin.http.request",
                request_id = %request_id,
                matched_route,
                method = %request.method()
            )
        })
        .on_response(DefaultOnResponse::new().level(Level::INFO))
        .on_failure(DefaultOnFailure::new().level(Level::ERROR));

    routes
        .layer(RequestBodyLimitLayer::new(ADMIN_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn(enforce_response_body_limit))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(state.config.route_timeout_seconds),
        ))
        .layer(CatchPanicLayer::new())
        .layer(middleware::from_fn(normalize_framework_error))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            VARY,
            HeaderValue::from_static("Authorization"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            PRAGMA,
            HeaderValue::from_static("no-cache"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0"),
        ))
        .layer(trace)
        .layer(middleware::from_fn(assign_request_id))
}

async fn enforce_response_body_limit(request: Request<Body>, next: Next) -> Response {
    let limit = admin_response_body_limit(&request);
    let request_id = request_id(request.headers());
    let response = next.run(request).await;
    let (parts, body) = response.into_parts();
    match to_bytes(body, limit).await {
        Ok(bytes) => Response::from_parts(parts, Body::from(bytes)),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ADMIN_INTERNAL",
            "internal server error",
            &request_id,
        ),
    }
}

fn admin_response_body_limit(request: &Request<Body>) -> usize {
    match request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
    {
        Some(
            "/admin/api/v2/wechat/login"
            | "/admin/api/v2/wechat/login/{login_id}"
            | "/admin/api/v2/wechat/login/{login_id}/verify",
        ) => ADMIN_QR_RESPONSE_BODY_BYTES,
        _ => ADMIN_RESPONSE_BODY_BYTES,
    }
}

async fn assign_request_id(mut request: Request<Body>, next: Next) -> Response {
    let value = HeaderValue::from_str(&Uuid::new_v4().to_string())
        .unwrap_or_else(|_| HeaderValue::from_static("request-id-unavailable"));
    request
        .headers_mut()
        .insert(X_REQUEST_ID.clone(), value.clone());
    let mut response = next.run(request).await;
    response.headers_mut().insert(X_REQUEST_ID.clone(), value);
    response
}

pub async fn require_mutation_boundary(
    State(state): State<AdminState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if matches!(request.method(), &Method::GET | &Method::HEAD) {
        return next.run(request).await;
    }
    let request_id = request_id(request.headers());
    if state.config.mode == AdminMode::ReadOnly {
        return error_response(
            StatusCode::FORBIDDEN,
            "ADMIN_READ_ONLY",
            "Admin API is read-only",
            &request_id,
        );
    }
    if !single_header_equals(
        request.headers(),
        ORIGIN,
        &state.config.allowed_origin,
        false,
    ) || !single_header_equals(request.headers(), X_ADMIN_ACTION.clone(), "1", false)
        || !single_json_content_type(request.headers())
        || !optional_single_header_equals(request.headers(), SEC_FETCH_SITE.clone(), "same-origin")
    {
        return error_response(
            StatusCode::FORBIDDEN,
            "ADMIN_MUTATION_FORBIDDEN",
            "Admin mutation boundary rejected the request",
            &request_id,
        );
    }
    next.run(request).await
}

fn single_header_equals(
    headers: &HeaderMap,
    name: HeaderName,
    expected: &str,
    ignore_ascii_case: bool,
) -> bool {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    if ignore_ascii_case {
        value.eq_ignore_ascii_case(expected)
    } else {
        value == expected
    }
}

fn optional_single_header_equals(headers: &HeaderMap, name: HeaderName, expected: &str) -> bool {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return true;
    };
    let Some(value) = first.to_str().ok() else {
        return false;
    };
    values.next().is_none() && value == expected
}

fn single_json_content_type(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(CONTENT_TYPE).iter();
    let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
        return false;
    };
    values.next().is_none()
        && value
            .split(';')
            .next()
            .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}
#[cfg(test)]
mod tests;
