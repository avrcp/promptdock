//! Startup failures stay inside our setup boundary instead of reaching Tauri's
//! event-loop panic. Native checks use internally owned, fresh temporary data.
use crate::error::AppError;
use serde::Serialize;
use std::{path::PathBuf, sync::Mutex};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartupFailure {
    pub code: &'static str,
    pub message: String,
}

impl StartupFailure {
    pub(crate) fn from_error(error: &(dyn std::error::Error + 'static)) -> Self {
        if let Some(error) = error.downcast_ref::<AppError>() {
            Self {
                code: error.code,
                message: error.message.clone(),
            }
        } else {
            Self {
                code: "DESKTOP_SETUP_FAILED",
                message: "桌面窗口或系统组件初始化失败".into(),
            }
        }
    }

    pub(crate) fn guidance(&self) -> &'static str {
        match self.code {
            "CAPTURE_POLICY_UNAVAILABLE" => "capture-policy.json 缺失、损坏或版本不兼容。清空运行数据会保留此配置，因此不会修复它。请备份并修复采集策略后重试；应用不会自行恢复默认授权。",
            "UNSUPPORTED_SCHEMA" | "CORRUPT_SCHEMA" => "本地运行库不符合当前版本。请退出应用，使用便携包中的 `clear-local-data.cmd`，确认清空后再启动。",
            "RUNTIME_RESET_INCOMPLETE" => "上次清空未完成，请重新运行 `clear-local-data.cmd`；不要手工删除维护标记。",
            _ => "请检查数据目录访问权限、WebView2 与系统组件；本次不会自动清除本地数据。",
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct StartupSnapshot {
    pub data_dir: Option<PathBuf>,
    pub setup_completed: bool,
    pub page_loaded: bool,
    pub frontend_ready: bool,
    pub runtime_stopped: bool,
    pub failure: Option<StartupFailure>,
}

#[derive(Default)]
pub(crate) struct StartupState(Mutex<StartupSnapshot>);

impl StartupState {
    pub(crate) fn snapshot(&self) -> StartupSnapshot {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn update(&self, update: impl FnOnce(&mut StartupSnapshot)) {
        update(&mut self.0.lock().unwrap_or_else(|p| p.into_inner()));
    }

    pub(crate) fn fail(&self, failure: StartupFailure) {
        tracing::error!(code = failure.code, "desktop startup failed");
        self.update(|state| {
            if state.failure.is_none() {
                state.failure = Some(failure);
            }
        });
    }
}

pub(crate) struct NativeStartupProbe {
    directory: tempfile::TempDir,
    pub scenario: String,
    cleanup_token: String,
}

impl NativeStartupProbe {
    pub(crate) fn new(scenario: &str, cleanup_token: uuid::Uuid) -> Result<Self, AppError> {
        if !matches!(
            scenario,
            "fresh" | "legacy-policy" | "legacy-database" | "reset-pending"
        ) || cleanup_token.get_version_num() != 4
        {
            return Err(AppError::new(
                "INVALID_STARTUP_PROBE",
                "未知的原生启动检查场景",
            ));
        }
        let directory = tempfile::Builder::new()
            .prefix(&format!(
                "PromptDock-native-startup-{}",
                cleanup_token.simple()
            ))
            .rand_bytes(0)
            .tempdir()
            .map_err(|_| AppError::new("STARTUP_PROBE_UNAVAILABLE", "无法创建隔离启动检查目录"))?;
        let probe = Self {
            directory,
            scenario: scenario.into(),
            cleanup_token: cleanup_token.to_string(),
        };
        let data = probe.data_dir();
        std::fs::create_dir(&data)
            .map_err(|_| AppError::new("STARTUP_PROBE_UNAVAILABLE", "无法创建隔离数据目录"))?;
        match scenario {
            "legacy-policy" => {
                let legacy = serde_json::json!({
                    "policy": crate::capture_policy::CapturePolicy::default(),
                    "captureGeneration": 0
                });
                std::fs::write(data.join("capture-policy.json"), legacy.to_string()).map_err(
                    |_| AppError::new("STARTUP_PROBE_UNAVAILABLE", "无法创建旧策略夹具"),
                )?;
            }
            "legacy-database" => {
                let db = rusqlite::Connection::open(data.join("promptdock.db"))?;
                db.execute_batch(
                    "CREATE TABLE legacy_fixture(value TEXT); PRAGMA user_version = 2;",
                )?;
            }
            "reset-pending" => {
                std::fs::write(data.join(".reset-pending"), "isolated fixture")
                    .map_err(|_| AppError::new("STARTUP_PROBE_UNAVAILABLE", "无法创建维护夹具"))?;
            }
            _ => {}
        }
        std::fs::write(
            probe.directory.path().join(".startup-probe.json"),
            serde_json::json!({
                "schemaVersion": 1,
                "scenario": scenario,
                "buildCommit": env!("PROMPTDOCK_BUILD_COMMIT"),
                "cleanupToken": probe.cleanup_token,
            })
            .to_string(),
        )
        .map_err(|_| AppError::new("STARTUP_PROBE_UNAVAILABLE", "无法创建隔离检查归属记录"))?;
        Ok(probe)
    }

    pub(crate) fn data_dir(&self) -> PathBuf {
        self.directory.path().join("data")
    }
    pub(crate) fn webview_dir(&self) -> PathBuf {
        self.directory.path().join("webview")
    }

    pub(crate) fn retain_for_parent_cleanup(&mut self) {
        // WebView2 keeps cache handles until this process exits. The invoking
        // smoke script validates the root and ownership marker before deleting
        // it after process exit. Unit-test TempDirs keep automatic cleanup.
        self.directory.disable_cleanup(true);
    }

    pub(crate) fn report(&self, state: &StartupSnapshot, exit_code: i32) {
        use std::io::Write;
        let receipt = serde_json::json!({
            "scenario": self.scenario,
            "buildCommit": env!("PROMPTDOCK_BUILD_COMMIT"),
            "setupCompleted": state.setup_completed,
            "pageLoaded": state.page_loaded,
            "frontendReady": state.frontend_ready,
            "runtimeStopped": state.runtime_stopped,
            "probeDirectory": self.directory.path(),
            "cleanupToken": self.cleanup_token,
            "errorCode": state.failure.as_ref().map(|e| e.code),
            "exitCode": exit_code
        });
        let _ = writeln!(std::io::stdout(), "{receipt}");
    }
}

pub(crate) fn show_failure(state: &StartupSnapshot) {
    let Some(failure) = &state.failure else {
        return;
    };
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        let dir = state
            .data_dir
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "尚未确定".into());
        let message = format!(
            "PromptDock Desktop 无法启动。\n\n错误：{}\n{}\n\n数据目录：{}\n\n{}",
            failure.code,
            failure.message,
            dir,
            failure.guidance()
        );
        let wide = |value: &str| {
            std::ffi::OsStr::new(value)
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                wide(&message).as_ptr(),
                wide("PromptDock Desktop").as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR
                    | windows_sys::Win32::UI::WindowsAndMessaging::MB_SETFOREGROUND,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_failure_keeps_safe_code_and_actionable_policy_guidance() {
        let error = AppError::new("CAPTURE_POLICY_UNAVAILABLE", "策略版本不兼容");
        let failure = StartupFailure::from_error(&error);
        assert_eq!(failure.code, "CAPTURE_POLICY_UNAVAILABLE");
        assert!(failure.guidance().contains("清空运行数据会保留此配置"));
        let native = std::io::Error::other("private operating system details");
        let native = StartupFailure::from_error(&native);
        assert_eq!(native.code, "DESKTOP_SETUP_FAILED");
        assert!(!native.message.contains("private"));
    }

    #[test]
    fn probe_has_no_user_selected_data_directory_and_legacy_policy_stays_invalid() {
        assert!(NativeStartupProbe::new("C:\\Users\\someone", uuid::Uuid::new_v4()).is_err());
        let probe = NativeStartupProbe::new("legacy-policy", uuid::Uuid::new_v4()).unwrap();
        let data = probe.data_dir();
        assert!(data.starts_with(std::env::temp_dir()));
        assert!(
            crate::capture_policy::try_read_policy_state(&data.join("agent-events.jsonl")).is_err()
        );
    }

    #[test]
    fn probe_refuses_reusing_an_existing_owned_directory() {
        let token = uuid::Uuid::new_v4();
        let probe = NativeStartupProbe::new("legacy-policy", token).unwrap();
        let original = std::fs::read(probe.data_dir().join("capture-policy.json")).unwrap();
        assert!(NativeStartupProbe::new("fresh", token).is_err());
        assert_eq!(
            std::fs::read(probe.data_dir().join("capture-policy.json")).unwrap(),
            original
        );
        assert!(NativeStartupProbe::new("fresh", uuid::Uuid::nil()).is_err());
    }
}
