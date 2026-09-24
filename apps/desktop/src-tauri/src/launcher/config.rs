use std::{
    net::IpAddr,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{atomic_file, error::AppError};

use super::model::{LauncherConfig, LauncherIssue, LauncherProxyConfig};

const LAUNCHER_SCHEMA_VERSION: u32 = 1;
const CONFIG_INVALID_CODE: &str = "LAUNCHER_CONFIG_INVALID";
const CONFIG_INVALID_MESSAGE: &str = "Launcher configuration is invalid.";
const CONFIG_REPAIR_MESSAGE: &str = "Launcher configuration needs repair.";
const CONFIG_WRITE_FAILED_CODE: &str = "LAUNCHER_CONFIG_WRITE_FAILED";
const CONFIG_WRITE_FAILED_MESSAGE: &str = "Launcher configuration could not be saved.";
const MAX_NO_PROXY_ITEMS: usize = 32;
const MAX_NO_PROXY_ITEM_CHARS: usize = 255;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoadedLauncherConfig {
    pub config: LauncherConfig,
    pub warning: Option<LauncherIssue>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LauncherConfigDocument {
    schema_version: u32,
    config: LauncherConfig,
}

impl LauncherConfig {
    pub fn normalized(mut self) -> Result<Self, AppError> {
        normalize_optional_path(&mut self.desktop.selected_executable);
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        validate_proxy_config(&self.proxy)?;
        validate_optional_absolute_path(&self.desktop.selected_executable)
    }
}

pub(crate) fn load(path: &Path) -> LoadedLauncherConfig {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedLauncherConfig {
                config: LauncherConfig::default(),
                warning: None,
            };
        }
        Err(_) => return damaged_config(),
    };
    let document = match serde_json::from_slice::<LauncherConfigDocument>(&bytes) {
        Ok(document) if document.schema_version == LAUNCHER_SCHEMA_VERSION => document,
        Ok(_) | Err(_) => return damaged_config(),
    };
    match document.config.normalized() {
        Ok(config) => LoadedLauncherConfig {
            config,
            warning: None,
        },
        Err(_) => damaged_config(),
    }
}

pub(crate) fn save(path: &Path, config: LauncherConfig) -> Result<LauncherConfig, AppError> {
    let config = config.normalized()?;
    let document = LauncherConfigDocument {
        schema_version: LAUNCHER_SCHEMA_VERSION,
        config: config.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&document).map_err(|error| {
        AppError::internal(
            CONFIG_WRITE_FAILED_CODE,
            CONFIG_WRITE_FAILED_MESSAGE,
            error.to_string(),
        )
    })?;
    bytes.push(b'\n');
    atomic_file::write(
        path,
        &bytes,
        CONFIG_WRITE_FAILED_CODE,
        CONFIG_WRITE_FAILED_MESSAGE,
    )?;
    Ok(config)
}

pub(crate) fn validate_proxy_config(config: &LauncherProxyConfig) -> Result<(), AppError> {
    if !config.enabled {
        return Ok(());
    }
    if config.port == 0 {
        return Err(invalid_config("proxy port is zero"));
    }
    if !is_loopback_host(&config.host) {
        return Err(invalid_config("proxy host is not loopback"));
    }
    if config.no_proxy.is_empty() || config.no_proxy.len() > MAX_NO_PROXY_ITEMS {
        return Err(invalid_config("noProxy item count is outside limits"));
    }
    if config.no_proxy.iter().any(|entry| {
        entry.chars().count() > MAX_NO_PROXY_ITEM_CHARS || entry.contains(['\r', '\n', '\0'])
    }) {
        return Err(invalid_config("noProxy entry is unsafe"));
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn validate_optional_absolute_path(path: &Option<PathBuf>) -> Result<(), AppError> {
    match path {
        Some(path) if !path.is_absolute() => Err(invalid_config("desktop path is relative")),
        _ => Ok(()),
    }
}

fn normalize_optional_path(path: &mut Option<PathBuf>) {
    if path.as_ref().is_some_and(|value| {
        value.as_os_str().is_empty() || value.to_str().is_some_and(|value| value.trim().is_empty())
    }) {
        *path = None;
    }
}

fn invalid_config(detail: &'static str) -> AppError {
    AppError::internal(CONFIG_INVALID_CODE, CONFIG_INVALID_MESSAGE, detail)
}

fn damaged_config() -> LoadedLauncherConfig {
    tracing::warn!(
        code = CONFIG_INVALID_CODE,
        "launcher configuration could not be loaded"
    );
    LoadedLauncherConfig {
        config: LauncherConfig::default(),
        warning: Some(LauncherIssue::new(
            CONFIG_INVALID_CODE,
            CONFIG_REPAIR_MESSAGE,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_through_schema_v1() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("launcher.json");
        let saved = save(&path, LauncherConfig::default()).unwrap();
        assert_eq!(load(&path).config, saved);
    }

    #[test]
    fn rejects_unsafe_enabled_proxy_values() {
        for host in [
            "192.168.1.4",
            "example.com",
            "http://127.0.0.1",
            "user@localhost",
            "localhost/path",
            "[::1]",
            "localhost\nHTTP_PROXY=unsafe",
        ] {
            let mut config = LauncherConfig::default();
            config.proxy.host = host.to_owned();
            assert!(
                config.validate().is_err(),
                "host unexpectedly valid: {host:?}"
            );
        }
        let mut config = LauncherConfig::default();
        config.proxy.port = 0;
        assert!(config.validate().is_err());
        config.proxy.port = 10808;
        config.proxy.no_proxy = vec!["localhost\r\nALL_PROXY=unsafe".to_owned()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_loopback_ipv4_ipv6_and_localhost() {
        for host in ["localhost", "LOCALHOST", "127.0.0.2", "::1"] {
            let mut config = LauncherConfig::default();
            config.proxy.host = host.to_owned();
            config.validate().unwrap();
        }
    }

    #[test]
    fn disabled_proxy_may_retain_inactive_values() {
        let mut config = LauncherConfig::default();
        config.proxy.enabled = false;
        config.proxy.host = "saved-for-later.invalid".to_owned();
        config.proxy.port = 0;
        config.proxy.no_proxy.clear();
        config.validate().unwrap();
    }

    #[test]
    fn selection_must_be_absolute_and_errors_hide_its_value() {
        let mut config = LauncherConfig::default();
        let sensitive = "private-user/secret/ChatGPT.exe";
        config.desktop.selected_executable = Some(PathBuf::from(sensitive));
        let error = config.validate().unwrap_err();
        assert_eq!(error.code, CONFIG_INVALID_CODE);
        assert!(!format!("{error:?}").contains(sensitive));
    }

    #[test]
    fn damaged_config_is_fail_soft_and_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("launcher.json");
        std::fs::write(&path, b"damaged").unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.warning.unwrap().code, CONFIG_INVALID_CODE);
        assert_eq!(std::fs::read(path).unwrap(), b"damaged");
    }
}
