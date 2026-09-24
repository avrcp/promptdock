use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use utoipa::ToSchema;

pub mod openapi;
pub mod wire;

pub use openapi::AdminApiDoc;
pub use wire::*;

pub const ADMIN_API_MAJOR: u32 = 2;
pub const ADMIN_SCHEMA_VERSION: u32 = 2;
pub const CONTRACT_MANIFEST_VERSION: u32 = 1;
pub const CONTRACT_NAME: &str = "promptdock-relay-admin-api-v2";
pub const JSON_MEDIA_TYPE: &str = "application/json";

pub mod routes {
    pub const BASE: &str = "/admin/api/v2";
    pub const META: &str = "/meta";
    pub const OVERVIEW: &str = "/overview";
    pub const DEVICES: &str = "/devices";
    pub const DEVICE: &str = "/devices/{device_id}";
    pub const DEVICE_ROTATE: &str = "/devices/{device_id}/rotate";
    pub const DEVICE_ENABLE: &str = "/devices/{device_id}/enable";
    pub const DEVICE_DISABLE: &str = "/devices/{device_id}/disable";
    pub const DEVICE_REVOKE: &str = "/devices/{device_id}/revoke";
    pub const WECHAT_STATUS: &str = "/wechat/status";
    pub const WECHAT_EVENTS: &str = "/wechat/events";
    pub const WECHAT_TEST: &str = "/wechat/test";
    pub const WECHAT_DISCONNECT: &str = "/wechat/disconnect";
    pub const WECHAT_LOGIN: &str = "/wechat/login";
    pub const WECHAT_LOGIN_SESSION: &str = "/wechat/login/{login_id}";
    pub const WECHAT_LOGIN_VERIFY: &str = "/wechat/login/{login_id}/verify";
    pub const DELIVERIES: &str = "/deliveries";
    pub const INTERACTIVE_REPLIES: &str = "/interactive-replies";
    pub const INBOUND_COMMANDS: &str = "/inbound-commands";
    pub const RESULTS: &str = "/results";
    pub const RESULT: &str = "/results/{result_row_id}";
    pub const RESULT_REVOKE: &str = "/results/{result_row_id}/revoke";
    pub const SYSTEM: &str = "/system";
    pub const MAINTENANCE_RETENTION: &str = "/maintenance/retention";
}

pub const ADMIN_READ_V2: AdminCapability = AdminCapability::Read;
pub const ADMIN_DEVICE_MANAGE_V2: AdminCapability = AdminCapability::DeviceManage;
pub const ADMIN_WECHAT_STATUS_V2: AdminCapability = AdminCapability::WechatStatus;
pub const ADMIN_WECHAT_MANAGE_V2: AdminCapability = AdminCapability::WechatManage;
pub const ADMIN_WECHAT_LOGIN_V2: AdminCapability = AdminCapability::WechatLogin;
pub const ADMIN_MAINTENANCE_V2: AdminCapability = AdminCapability::Maintenance;
pub const ADMIN_RESULTS_MANAGE_V2: AdminCapability = AdminCapability::ResultsManage;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub enum AdminCapability {
    #[serde(rename = "admin_read_v2")]
    Read,
    #[serde(rename = "admin_device_manage_v2")]
    DeviceManage,
    #[serde(rename = "admin_wechat_status_v2")]
    WechatStatus,
    #[serde(rename = "admin_wechat_manage_v2")]
    WechatManage,
    #[serde(rename = "admin_wechat_login_v2")]
    WechatLogin,
    #[serde(rename = "admin_maintenance_v2")]
    Maintenance,
    #[serde(rename = "admin_results_manage_v2")]
    ResultsManage,
}

impl AdminCapability {
    pub const ALL: &[Self] = &[
        Self::Read,
        Self::DeviceManage,
        Self::WechatStatus,
        Self::WechatManage,
        Self::WechatLogin,
        Self::Maintenance,
        Self::ResultsManage,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "admin_read_v2",
            Self::DeviceManage => "admin_device_manage_v2",
            Self::WechatStatus => "admin_wechat_status_v2",
            Self::WechatManage => "admin_wechat_manage_v2",
            Self::WechatLogin => "admin_wechat_login_v2",
            Self::Maintenance => "admin_maintenance_v2",
            Self::ResultsManage => "admin_results_manage_v2",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InboundCommand {
    Help,
    ListDevices,
    ListJobs,
    ListRecent,
    ListFailed,
    NextPage,
    GetStatus,
    GetDetail,
    GetTree,
    Unknown,
}

impl InboundCommand {
    pub const ALL: &[Self] = &[
        Self::Help,
        Self::ListDevices,
        Self::ListJobs,
        Self::ListRecent,
        Self::ListFailed,
        Self::NextPage,
        Self::GetStatus,
        Self::GetDetail,
        Self::GetTree,
        Self::Unknown,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Help => "help",
            Self::ListDevices => "list_devices",
            Self::ListJobs => "list_jobs",
            Self::ListRecent => "list_recent",
            Self::ListFailed => "list_failed",
            Self::NextPage => "next_page",
            Self::GetStatus => "get_status",
            Self::GetDetail => "get_detail",
            Self::GetTree => "get_tree",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminMeta<'a> {
    pub schema_version: u32,
    pub generated_at: i64,
    pub admin_api_version: u32,
    pub relay_version: &'a str,
    pub capabilities: Vec<AdminCapability>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub total: Option<u64>,
    pub generated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminErrorBody<'a> {
    pub code: &'a str,
    pub message: &'a str,
    pub request_id: &'a str,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractManifest {
    pub manifest_version: u32,
    pub contract: String,
    pub fixtures: Vec<FixtureEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureEntry {
    pub path: String,
    pub sha256: String,
    pub media_type: String,
    pub schema_version: u32,
}

pub fn parse_manifest(bytes: &[u8]) -> Result<ContractManifest, String> {
    serde_json::from_slice(bytes).map_err(|error| format!("invalid contract manifest: {error}"))
}

pub fn validate_fixture_manifest(
    manifest: &ContractManifest,
    files: &BTreeMap<String, Vec<u8>>,
    expected_fixtures: &[&str],
) -> Result<(), String> {
    if manifest.manifest_version != CONTRACT_MANIFEST_VERSION {
        return Err("unsupported manifestVersion".to_owned());
    }
    if manifest.contract != CONTRACT_NAME {
        return Err("unexpected contract name".to_owned());
    }
    if files.keys().map(String::as_str).collect::<Vec<_>>() != expected_fixtures {
        return Err("Admin API fixture inventory changed".to_owned());
    }

    let mut seen = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for entry in &manifest.fixtures {
        if !is_canonical_fixture_name(&entry.path) {
            return Err(format!("unsafe or non-v1 fixture path: {}", entry.path));
        }
        if !seen.insert(entry.path.clone()) {
            return Err(format!("duplicate fixture path: {}", entry.path));
        }
        if previous.is_some_and(|previous| previous >= entry.path.as_str()) {
            return Err("fixture paths are not strictly sorted".to_owned());
        }
        previous = Some(&entry.path);
        if entry.media_type != JSON_MEDIA_TYPE {
            return Err(format!("unexpected media type for {}", entry.path));
        }
        if entry.schema_version != ADMIN_SCHEMA_VERSION {
            return Err(format!("unexpected schema version for {}", entry.path));
        }
        if !is_lowercase_sha256(&entry.sha256) {
            return Err(format!("invalid lowercase SHA-256 for {}", entry.path));
        }

        let bytes = files
            .get(&entry.path)
            .ok_or_else(|| format!("manifest contains extra fixture: {}", entry.path))?;
        validate_canonical_json_bytes(&entry.path, bytes)?;
        let actual = format!("{:x}", Sha256::digest(bytes));
        if actual != entry.sha256 {
            return Err(format!("raw-byte SHA-256 mismatch for {}", entry.path));
        }
    }

    let actual_paths = files.keys().cloned().collect::<BTreeSet<_>>();
    if seen != actual_paths {
        let missing = actual_paths.difference(&seen).cloned().collect::<Vec<_>>();
        return Err(format!("manifest omits public fixtures: {missing:?}"));
    }
    Ok(())
}

pub fn is_canonical_fixture_name(value: &str) -> bool {
    matches!(value.as_bytes().first(), Some(byte) if byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.ends_with("-v2.json")
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
}

pub fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn validate_canonical_json_bytes(path: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty()
        || bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || bytes.contains(&b'\r')
        || !bytes.ends_with(b"\n")
    {
        return Err(format!("non-canonical fixture bytes: {path}"));
    }
    serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|_| format!("invalid fixture JSON: {path}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_values_serialize_to_the_public_wire_names() {
        assert_eq!(
            AdminCapability::ALL
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>(),
            [
                "admin_read_v2",
                "admin_device_manage_v2",
                "admin_wechat_status_v2",
                "admin_wechat_manage_v2",
                "admin_wechat_login_v2",
                "admin_maintenance_v2",
                "admin_results_manage_v2",
            ]
        );
        assert_eq!(InboundCommand::ListJobs.as_str(), "list_jobs");
        assert!(serde_json::from_str::<InboundCommand>("\"list_runs\"").is_err());
    }

    #[test]
    fn common_wire_objects_are_strict_camel_case() {
        let meta = AdminMeta {
            schema_version: ADMIN_SCHEMA_VERSION,
            generated_at: 1,
            admin_api_version: ADMIN_API_MAJOR,
            relay_version: "test",
            capabilities: vec![ADMIN_READ_V2],
        };
        let value = serde_json::to_value(meta).expect("meta JSON");
        assert_eq!(value["adminApiVersion"], ADMIN_API_MAJOR);
        assert_eq!(value["capabilities"][0], "admin_read_v2");
        assert!(value.get("admin_api_version").is_none());
    }
}
