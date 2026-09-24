use super::*;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureHealthChanged {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error_code: Option<String>,
}

pub(super) trait RuntimeEventEmitter: Send + Sync + 'static {
    fn capture_health_changed(&self, checkpoint: &SourceCheckpointUpdate);
}

pub(super) struct TauriRuntimeEventEmitter(pub(super) tauri::AppHandle);

impl RuntimeEventEmitter for TauriRuntimeEventEmitter {
    fn capture_health_changed(&self, checkpoint: &SourceCheckpointUpdate) {
        if let Err(error) = self.0.emit(
            "promptdock://capture-status-changed",
            CaptureHealthChanged {
                status: checkpoint.status.clone(),
                last_error_code: checkpoint.last_error_code.clone(),
            },
        ) {
            tracing::warn!(%error, "failed to emit capture health change");
        }
    }
}
