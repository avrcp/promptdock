use std::time::Duration;

use promptdock_server::{
    qr_login::{
        DEFAULT_FINISHED_RETENTION, DEFAULT_LOGIN_TTL, LoginError, LoginManager, LoginState,
    },
    secret_store::{DEFAULT_CONNECTION_FILE, SYSTEMD_CREDENTIAL_NAME, SecretStoreError},
    wechat,
    wechat_login::{WechatDisconnectError, WechatRuntimeStatus},
    wechat_monitor::{
        ERROR_AUTHENTICATION_EXPIRED, ERROR_INVALID_ENDPOINT, ERROR_MONITOR_NETWORK,
        ERROR_MONITOR_PERSIST_FAILED, ERROR_MONITOR_PROTOCOL, ERROR_MONITOR_REJECTED,
        ERROR_MONITOR_WORKER_EXITED, MonitorState, MonitorStatus,
    },
};
use uuid::Uuid;

#[test]
fn legacy_wechat_public_surface_and_safe_values_are_frozen() {
    assert_eq!(DEFAULT_LOGIN_TTL, Duration::from_secs(5 * 60));
    assert_eq!(DEFAULT_FINISHED_RETENTION, Duration::from_secs(5 * 60));
    assert_eq!(
        DEFAULT_CONNECTION_FILE,
        "/var/lib/promptdock-relay/wechat-connection.enc"
    );
    assert_eq!(SYSTEMD_CREDENTIAL_NAME, "relay-master-key");

    assert_eq!(MonitorStatus::default().monitor, MonitorState::Stopped);
    assert_eq!(MonitorStatus::default().error_code, None);
    assert_eq!(
        WechatRuntimeStatus::Disconnected,
        WechatRuntimeStatus::Disconnected
    );
    assert_eq!(
        WechatDisconnectError.to_string(),
        "WeChat disconnect could not durably clear all connection state"
    );

    assert_eq!(
        ERROR_AUTHENTICATION_EXPIRED,
        "WECHAT_AUTHENTICATION_EXPIRED"
    );
    assert_eq!(ERROR_INVALID_ENDPOINT, "WECHAT_INVALID_ENDPOINT");
    assert_eq!(ERROR_MONITOR_NETWORK, "WECHAT_MONITOR_NETWORK");
    assert_eq!(ERROR_MONITOR_PROTOCOL, "WECHAT_MONITOR_PROTOCOL");
    assert_eq!(ERROR_MONITOR_REJECTED, "WECHAT_MONITOR_REJECTED");
    assert_eq!(
        ERROR_MONITOR_PERSIST_FAILED,
        "WECHAT_MONITOR_PERSIST_FAILED"
    );
    assert_eq!(ERROR_MONITOR_WORKER_EXITED, "WECHAT_MONITOR_WORKER_EXITED");

    let legacy_status = MonitorStatus::default();
    let facade_status: wechat::monitor::MonitorStatus = legacy_status;
    assert_eq!(
        facade_status.monitor,
        wechat::monitor::MonitorState::Stopped
    );
    let _: wechat::login::LoginManager = LoginManager::new();
    let _: &str = wechat::secret_store::DEFAULT_CONNECTION_FILE;

    let owner = Uuid::new_v4();
    let manager = LoginManager::new();
    let started = manager.start(owner, false, false).expect("login start");
    assert_eq!(started.snapshot.state, LoginState::FetchingQr);
    let login_id = Uuid::parse_str(&started.snapshot.login_id).expect("login id");
    assert_eq!(
        manager.snapshot(Uuid::new_v4(), login_id),
        Err(LoginError::NotFound)
    );

    let public_error = SecretStoreError::InvalidMasterCredential;
    assert_eq!(
        public_error.to_string(),
        "systemd master credential is invalid"
    );
}
