use axum::{Json, extract::State};

use crate::state::{AppState, ServerInfo};

pub(super) async fn server_info(State(state): State<AppState>) -> Json<ServerInfo> {
    Json(state.server_info)
}
