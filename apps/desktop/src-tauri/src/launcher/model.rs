use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LauncherConfig {
    pub proxy: LauncherProxyConfig,
    pub desktop: DesktopLauncherConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LauncherProxyConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub no_proxy: Vec<String>,
}

impl Default for LauncherProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "127.0.0.1".to_owned(),
            port: 10_808,
            no_proxy: vec![
                "localhost".to_owned(),
                "127.0.0.1".to_owned(),
                "::1".to_owned(),
            ],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopLauncherConfig {
    /// The user-selected desktop executable. Discovery never fills this field
    /// implicitly, so different installed products cannot silently replace one
    /// another based on package version.
    #[serde(deserialize_with = "deserialize_optional_path")]
    pub selected_executable: Option<PathBuf>,
    pub refuse_if_running: bool,
}

impl Default for DesktopLauncherConfig {
    fn default() -> Self {
        Self {
            selected_executable: None,
            refuse_if_running: true,
        }
    }
}

fn deserialize_optional_path<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.and_then(|value| {
        if value.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(value))
        }
    }))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LauncherIssue {
    pub code: String,
    pub message: String,
}

impl LauncherIssue {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopProduct {
    ChatGptDesktop,
    ChatGptDesktopClassic,
    ExplicitExecutable,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopDiscoverySource {
    AppxManifest,
    AppxFallback,
    ExplicitExecutable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopHostCandidate {
    /// This path is intentionally exposed to the local settings UI so the user
    /// can make and persist an explicit host choice.
    pub executable: PathBuf,
    pub product: DesktopProduct,
    pub product_label: String,
    pub package_name: Option<String>,
    pub package_version: Option<String>,
    pub architecture: Option<String>,
    pub discovery_source: DesktopDiscoverySource,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LauncherStatus {
    pub config: LauncherConfig,
    pub config_warning: Option<LauncherIssue>,
    pub candidates: Vec<DesktopHostCandidate>,
    pub selected: Option<DesktopHostCandidate>,
    /// A persisted selection may become invalid after an AppX update.  Keep
    /// the rest of the settings view available so the user can repair it.
    pub selection_issue: Option<LauncherIssue>,
    pub selected_running: bool,
    pub selected_pid: Option<u32>,
    pub selected_process_issue: Option<LauncherIssue>,
    pub discovery_issue: Option<LauncherIssue>,
    pub proxy_endpoint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopLaunchReceipt {
    pub pid: u32,
    pub product: DesktopProduct,
    pub product_label: String,
    pub package_name: Option<String>,
    pub package_version: Option<String>,
    pub proxy_endpoint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_desktop_selection_deserializes_as_none() {
        let value = serde_json::json!({
            "proxy": {
                "enabled": true,
                "host": "127.0.0.1",
                "port": 10808,
                "noProxy": ["localhost"]
            },
            "desktop": {
                "selectedExecutable": "   ",
                "refuseIfRunning": true
            }
        });
        let config: LauncherConfig = serde_json::from_value(value).unwrap();
        assert_eq!(config.desktop.selected_executable, None);
    }
}
