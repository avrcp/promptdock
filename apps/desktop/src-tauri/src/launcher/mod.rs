use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::Serialize;

use crate::error::AppError;

mod config;
mod desktop;
pub mod model;
mod proxy;

pub use self::model::{DesktopHostCandidate, DesktopLaunchReceipt, LauncherConfig, LauncherStatus};

/// Result of an explicit user-requested Desktop host activation.  This call
/// never discovers, launches, terminates, or sends input to another process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostFocusStatus {
    Focused,
    MultipleCandidates,
    HostNotRunning,
    WindowNotFound,
    ForegroundDenied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostFocusResult {
    pub status: HostFocusStatus,
}

use self::{
    config::{load, save},
    model::LauncherIssue,
};

#[derive(Clone)]
pub struct LauncherState(Arc<LauncherStateInner>);

struct LauncherStateInner {
    config_path: PathBuf,
    config: Mutex<LauncherConfig>,
    config_warning: Mutex<Option<LauncherIssue>>,
    cached_candidates: Mutex<Vec<DesktopHostCandidate>>,
    cached_discovery_issue: Mutex<Option<LauncherIssue>>,
    discovery_initialized: Mutex<bool>,
}

impl LauncherState {
    pub fn open(config_path: PathBuf) -> Result<Self, AppError> {
        let loaded = load(&config_path);
        Ok(Self(Arc::new(LauncherStateInner {
            config_path,
            config: Mutex::new(loaded.config),
            config_warning: Mutex::new(loaded.warning),
            cached_candidates: Mutex::new(Vec::new()),
            cached_discovery_issue: Mutex::new(None),
            discovery_initialized: Mutex::new(false),
        })))
    }

    pub fn config(&self) -> Result<LauncherConfig, AppError> {
        self.0
            .config
            .lock()
            .map(|config| config.clone())
            .map_err(|_| state_unavailable())
    }

    pub fn replace_config(&self, config: LauncherConfig) -> Result<LauncherConfig, AppError> {
        self.replace_config_checked(config, None)
    }

    pub fn replace_config_checked(
        &self,
        config: LauncherConfig,
        expected: Option<&LauncherConfig>,
    ) -> Result<LauncherConfig, AppError> {
        let mut current = self.0.config.lock().map_err(|_| state_unavailable())?;
        if expected.is_some_and(|expected| {
            expected != &*current || load(&self.0.config_path).config != *expected
        }) {
            return Err(AppError::new(
                "CONFIG_REVISION_CONFLICT",
                "配置已变化，请核对并重新载入已保存设置",
            ));
        }
        let saved = save(&self.0.config_path, config)?;
        *current = saved.clone();
        *self
            .0
            .config_warning
            .lock()
            .map_err(|_| state_unavailable())? = None;
        Ok(saved)
    }

    /// Lists all resolvable AppX hosts. This function never chooses a
    /// candidate or changes the persisted selection.
    pub async fn discover_candidates(&self) -> Result<Vec<DesktopHostCandidate>, AppError> {
        let candidates = desktop::discover_appx().await?;
        *self
            .0
            .cached_candidates
            .lock()
            .map_err(|_| state_unavailable())? = candidates.clone();
        *self
            .0
            .cached_discovery_issue
            .lock()
            .map_err(|_| state_unavailable())? = None;
        *self
            .0
            .discovery_initialized
            .lock()
            .map_err(|_| state_unavailable())? = true;
        Ok(candidates)
    }

    /// Returns independent discovery, selection, process, and proxy facts for
    /// the local UI. Discovery errors are reported in the status rather than
    /// hiding a still-valid explicit executable selection.
    pub async fn status(&self) -> Result<LauncherStatus, AppError> {
        let config = self.config()?;
        let config_warning = self
            .0
            .config_warning
            .lock()
            .map(|warning| warning.clone())
            .map_err(|_| state_unavailable())?;
        // Discovery launches PowerShell.  Do it only for the first snapshot
        // (or an explicit refresh) instead of coupling ordinary settings
        // saves to AppX enumeration.
        let initialized = *self
            .0
            .discovery_initialized
            .lock()
            .map_err(|_| state_unavailable())?;
        let (candidates, discovery_issue) = if !initialized {
            match self.discover_candidates().await {
                Ok(candidates) => (candidates, None),
                Err(error) => {
                    let issue = LauncherIssue::new(error.code, error.message);
                    *self
                        .0
                        .cached_discovery_issue
                        .lock()
                        .map_err(|_| state_unavailable())? = Some(issue.clone());
                    *self
                        .0
                        .discovery_initialized
                        .lock()
                        .map_err(|_| state_unavailable())? = true;
                    (Vec::new(), Some(issue))
                }
            }
        } else {
            (
                self.cached_candidates()?,
                self.0
                    .cached_discovery_issue
                    .lock()
                    .map(|issue| issue.clone())
                    .map_err(|_| state_unavailable())?,
            )
        };
        let (selected, selection_issue) =
            match desktop::selected_candidate(&config.desktop, &candidates) {
                Ok(selected) => (selected, None),
                Err(error) => (None, Some(LauncherIssue::new(error.code, error.message))),
            };
        let (selected_running, selected_pid, selected_process_issue) = match selected.as_ref() {
            Some(candidate) => match desktop::desktop_root_pid(&candidate.executable) {
                Ok(pid) => (pid.is_some(), pid, None),
                Err(error) => (
                    false,
                    None,
                    Some(LauncherIssue::new(error.code, error.message)),
                ),
            },
            None => (false, None, None),
        };
        let proxy_endpoint = config
            .proxy
            .environment()?
            .map(|environment| environment.proxy_url);
        Ok(LauncherStatus {
            config,
            config_warning,
            candidates,
            selected,
            selection_issue,
            selected_running,
            selected_pid,
            selected_process_issue,
            discovery_issue,
            proxy_endpoint,
        })
    }

    /// Starts only the executable explicitly persisted in the desktop config.
    /// An enabled invalid proxy fails before process creation.
    pub async fn launch_selected(&self) -> Result<DesktopLaunchReceipt, AppError> {
        let config = self.config()?;
        let candidates = match self.discover_candidates().await {
            Ok(candidates) => candidates,
            Err(_) => self.cached_candidates()?,
        };
        let candidate =
            desktop::selected_candidate(&config.desktop, &candidates)?.ok_or_else(|| {
                AppError::new(
                    "DESKTOP_SELECTION_REQUIRED",
                    "Select a ChatGPT Desktop executable before launching.",
                )
            })?;
        let (pid, proxy_endpoint) =
            desktop::spawn_desktop(&candidate, config.desktop.refuse_if_running, &config.proxy)?;
        Ok(DesktopLaunchReceipt {
            pid,
            product: candidate.product,
            product_label: candidate.product_label,
            package_name: candidate.package_name,
            package_version: candidate.package_version,
            proxy_endpoint,
        })
    }

    /// Activates a visible top-level window for the explicitly selected host.
    /// The IPC boundary must invoke this only for an intentional user action.
    pub fn focus_host(&self) -> Result<HostFocusResult, AppError> {
        let config = self.config()?;
        let candidate = desktop::selected_candidate(&config.desktop, &[])?.ok_or_else(|| {
            AppError::new(
                "DESKTOP_SELECTION_REQUIRED",
                "Select a ChatGPT Desktop executable before focusing it.",
            )
        })?;
        Ok(HostFocusResult {
            status: desktop::focus_host(&candidate.executable),
        })
    }

    fn cached_candidates(&self) -> Result<Vec<DesktopHostCandidate>, AppError> {
        self.0
            .cached_candidates
            .lock()
            .map(|candidates| candidates.clone())
            .map_err(|_| state_unavailable())
    }
}

fn state_unavailable() -> AppError {
    AppError::new(
        "LAUNCHER_STATE_UNAVAILABLE",
        "Launcher state is temporarily unavailable.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damaged_config_is_fail_soft_and_save_clears_warning() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("launcher.json");
        std::fs::write(&path, b"damaged").unwrap();
        let state = LauncherState::open(path).unwrap();
        assert_eq!(
            state
                .0
                .config_warning
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .code,
            "LAUNCHER_CONFIG_INVALID"
        );
        state.replace_config(LauncherConfig::default()).unwrap();
        assert_eq!(*state.0.config_warning.lock().unwrap(), None);
    }
}
