use axum::{
    Extension,
    extract::{State, WebSocketUpgrade},
    http::{HeaderMap, HeaderValue, StatusCode, header::WWW_AUTHENTICATE},
    response::{IntoResponse, Response},
};

use super::{protocol::MAX_GATEWAY_FRAME_BYTES, runtime::GatewayHandshakeError};
use crate::{
    api::errors::{error_response, request_id},
    auth::AuthenticatedDevice,
    state::AppState,
};

// Keep the transport fence one byte above the public protocol limit so the
// runtime can observe the exact first-invalid frame and emit the stable 4009
// close code. The closed codec remains the authoritative 64 KiB boundary;
// larger abuse is still rejected by Tungstenite before unbounded allocation.
const MAX_GATEWAY_TRANSPORT_FRAME_BYTES: usize = MAX_GATEWAY_FRAME_BYTES + 1;

pub async fn gateway_websocket(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let lease = match state.gateway.prepare_initial_lease(&device).await {
        Ok(lease) => lease,
        Err(error) => return handshake_error(error, &headers),
    };
    let runtime = state.gateway.clone();
    upgrade
        .max_message_size(MAX_GATEWAY_TRANSPORT_FRAME_BYTES)
        .max_frame_size(MAX_GATEWAY_TRANSPORT_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            runtime.run_tracked(socket, device, lease).await;
        })
        .into_response()
}

fn handshake_error(error: GatewayHandshakeError, headers: &HeaderMap) -> Response {
    let request_id = request_id(headers);
    match error {
        GatewayHandshakeError::Unauthorized => {
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
        GatewayHandshakeError::Revoked | GatewayHandshakeError::Disabled => error_response(
            StatusCode::FORBIDDEN,
            "DEVICE_REVOKED",
            "device credential is not active",
            &request_id,
        ),
        GatewayHandshakeError::InsufficientScope => error_response(
            StatusCode::FORBIDDEN,
            "INSUFFICIENT_SCOPE",
            "request is not permitted",
            &request_id,
        ),
        GatewayHandshakeError::Database => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "AUTH_UNAVAILABLE",
            "authentication service is unavailable",
            &request_id,
        ),
    }
}
