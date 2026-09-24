use axum::{Json, extract::Extension, extract::State};
use serde::Serialize;

use crate::{
    auth::{AuthenticatedDevice, DeviceScope},
    state::{AppState, WechatConnectionState},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeviceStatusV1 {
    device_id: String,
    can_submit: bool,
    can_read_own: bool,
    channel_state: &'static str,
}

pub(super) async fn device_status(
    State(state): State<AppState>,
    Extension(device): Extension<AuthenticatedDevice>,
) -> Json<DeviceStatusV1> {
    let channel_state = if device.has_scope(DeviceScope::ChannelRead) {
        match state.wechat_status().await {
            Ok(status) => match status.state {
                WechatConnectionState::Ready => "ready",
                WechatConnectionState::ConnectedAwaitingActivation => "activation_required",
                WechatConnectionState::Disconnected
                | WechatConnectionState::Degraded
                | WechatConnectionState::NeedsReconnect
                | WechatConnectionState::CredentialsUnreadable => "reconnect_required",
            },
            Err(_) => "unknown",
        }
    } else {
        "unknown"
    };
    Json(DeviceStatusV1 {
        device_id: device.id.to_string(),
        can_submit: device.has_scope(DeviceScope::NotifyWrite),
        can_read_own: device.has_scope(DeviceScope::NotifyReadOwn),
        channel_state,
    })
}
