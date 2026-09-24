//! Single-owner WeChat long-poll runtime.

mod poll;
mod runtime;
mod state;

pub use runtime::WechatMonitorRuntime;
pub use state::{
    ConnectionSnapshot, ERROR_AUTHENTICATION_EXPIRED, ERROR_INVALID_ENDPOINT,
    ERROR_MONITOR_CAPTURE_FAILED, ERROR_MONITOR_NETWORK, ERROR_MONITOR_PERSIST_FAILED,
    ERROR_MONITOR_PROTOCOL, ERROR_MONITOR_REJECTED, ERROR_MONITOR_WORKER_EXITED,
    MonitorActivationHook, MonitorState, MonitorStatus, NoopMonitorActivationHook,
    WechatMonitorError, WechatMonitorHandle, WechatMonitorTransport,
};

#[cfg(test)]
mod tests;
