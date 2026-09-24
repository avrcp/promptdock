use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::run::{
    RemoteDesiredStateV5, RemoteRunDetailV5, RemoteRunFilterV5, RemoteRunPageV5, RemoteRunPhaseV5,
    RemoteRunTreeV5, RunQueryActionV5, RunQueryResultV5,
};

pub const GATEWAY_PROTOCOL_VERSION: u16 = 5;
pub const MAX_GATEWAY_FRAME_BYTES: usize = 65_536;
pub const MAX_GATEWAY_RESPONSE_BYTES: usize = 16_384;
pub const GATEWAY_HEARTBEAT_SECONDS: u16 = 20;
pub const GATEWAY_PONG_TIMEOUT_SECONDS: u16 = 60;
pub const MAX_GATEWAY_OUTBOUND_QUEUE: usize = 64;
pub const MAX_GATEWAY_PENDING_REQUESTS: usize = 16;
pub const MAX_GATEWAY_REQUEST_TTL_MS: i64 = 30_000;

pub const CLOSE_CODE_SUPERSEDED: u16 = 4_001;
pub const CLOSE_CODE_PROTOCOL_ERROR: u16 = 4_002;
pub const CLOSE_CODE_POLICY_REVOKED: u16 = 4_003;
pub const CLOSE_CODE_SLOW_CONSUMER: u16 = 4_008;
pub const CLOSE_CODE_FRAME_TOO_LARGE: u16 = 4_009;
pub const CLOSE_CODE_SERVER_SHUTDOWN: u16 = 1_001;

pub const CLOSE_REASON_SUPERSEDED: &str = "superseded";
pub const CLOSE_REASON_PROTOCOL_ERROR: &str = "protocol_error";
pub const CLOSE_REASON_POLICY_REVOKED: &str = "policy_revoked";
pub const CLOSE_REASON_SLOW_CONSUMER: &str = "slow_consumer";
pub const CLOSE_REASON_FRAME_TOO_LARGE: &str = "frame_too_large";
pub const CLOSE_REASON_SERVER_SHUTDOWN: &str = "server_shutdown";

const MAX_CLIENT_VERSION_BYTES: usize = 64;
const MAX_CAPABILITIES: usize = 16;
const MAX_DEVICE_NAME_CHARS: usize = 80;
const MAX_DEVICE_NAME_BYTES: usize = 320;
const MAX_ERROR_MESSAGE_CHARS: usize = 256;
const MAX_ERROR_MESSAGE_BYTES: usize = 1_024;
const MAX_CATALOG_ITEMS: usize = 50;
const MAX_INTENT_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayServerFrameV5 {
    Welcome(WelcomeV5),
    Ping(PingV5),
    Request(RemoteRequestV5),
    Superseded(SupersededV5),
    PolicyRevoked(PolicyRevokedV5),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayClientFrameV5 {
    Hello(HelloV5),
    Pong(PongV5),
    RequestAck(RequestAckV5),
    Response(Box<RemoteResponseV5>),
    Error(RemoteErrorV5),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelloV5 {
    pub protocol_version: u16,
    pub client_version: String,
    pub capabilities: Vec<GatewayCapabilityV5>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayCapabilityV5 {
    DeviceInfoV5,
    RemoteRunsReadV2,
    RuntimeCatalogV2,
    RunLaunchV2,
    RunControlV2,
    RunChildrenV2,
}

impl GatewayCapabilityV5 {
    pub const DEVICE_INFO_REQUIREMENTS: &'static [Self] = &[Self::DeviceInfoV5];
    pub const RUN_QUERY_REQUIREMENTS: &'static [Self] = &[Self::RemoteRunsReadV2];
    pub const CATALOG_REQUIREMENTS: &'static [Self] = &[Self::RuntimeCatalogV2];
    pub const START_RUN_REQUIREMENTS: &'static [Self] = &[Self::RunLaunchV2, Self::RunControlV2];
    pub const CANCEL_RUN_REQUIREMENTS: &'static [Self] = &[Self::RunControlV2];

    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::DeviceInfoV5 => "device_info_v5",
            Self::RemoteRunsReadV2 => "remote_runs_read_v2",
            Self::RuntimeCatalogV2 => "runtime_catalog_v2",
            Self::RunLaunchV2 => "run_launch_v2",
            Self::RunControlV2 => "run_control_v2",
            Self::RunChildrenV2 => "run_children_v2",
        }
    }

    pub const fn requirements_for(action: &RemoteRequestActionV5) -> &'static [Self] {
        match action {
            RemoteRequestActionV5::GetDeviceInfo => Self::DEVICE_INFO_REQUIREMENTS,
            RemoteRequestActionV5::ListRuns { .. }
            | RemoteRequestActionV5::GetRunDetail { .. }
            | RemoteRequestActionV5::GetRunTree { .. } => Self::RUN_QUERY_REQUIREMENTS,
            RemoteRequestActionV5::ListRuntimes
            | RemoteRequestActionV5::ListWorkspaces
            | RemoteRequestActionV5::ListHarnessProfiles { .. }
            | RemoteRequestActionV5::ListTaskPresets { .. } => Self::CATALOG_REQUIREMENTS,
            RemoteRequestActionV5::StartRun { .. } => Self::START_RUN_REQUIREMENTS,
            RemoteRequestActionV5::CancelRun { .. } => Self::CANCEL_RUN_REQUIREMENTS,
        }
    }

    pub fn has_all(capabilities: &BTreeSet<Self>, required: &[Self]) -> bool {
        required
            .iter()
            .all(|capability| capabilities.contains(capability))
    }

    pub fn supports_action(capabilities: &BTreeSet<Self>, action: &RemoteRequestActionV5) -> bool {
        Self::has_all(capabilities, Self::requirements_for(action))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WelcomeV5 {
    pub protocol_version: u16,
    pub connection_id: String,
    pub generation: u64,
    pub heartbeat_seconds: u16,
    pub max_frame_bytes: usize,
    pub server_time: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PingV5 {
    pub ping_id: String,
    pub generation: u64,
    pub sent_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PongV5 {
    pub ping_id: String,
    pub generation: u64,
    pub client_time: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteRequestV5 {
    pub request_id: String,
    pub generation: u64,
    pub issued_at: i64,
    pub expires_at: i64,
    pub action: RemoteRequestActionV5,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RemoteRequestActionV5 {
    GetDeviceInfo,
    ListRuns {
        filter: RemoteRunFilterV5,
        page_size: u16,
        cursor: Option<String>,
    },
    GetRunDetail {
        run_handle: String,
    },
    GetRunTree {
        run_handle: String,
        max_depth: u8,
        max_nodes: u16,
    },
    ListRuntimes,
    ListWorkspaces,
    ListHarnessProfiles {
        runtime_handle: String,
        workspace_handle: String,
    },
    ListTaskPresets {
        runtime_handle: String,
        workspace_handle: String,
        harness_profile_handle: String,
    },
    StartRun {
        runtime_handle: String,
        workspace_handle: String,
        harness_profile_handle: String,
        preset_handle: String,
        intent_id: String,
    },
    CancelRun {
        run_handle: String,
        intent_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogQueryActionV5 {
    ListRuntimes,
    ListWorkspaces,
    ListHarnessProfiles {
        runtime_handle: String,
        workspace_handle: String,
    },
    ListTaskPresets {
        runtime_handle: String,
        workspace_handle: String,
        harness_profile_handle: String,
    },
}

impl From<CatalogQueryActionV5> for RemoteRequestActionV5 {
    fn from(action: CatalogQueryActionV5) -> Self {
        match action {
            CatalogQueryActionV5::ListRuntimes => Self::ListRuntimes,
            CatalogQueryActionV5::ListWorkspaces => Self::ListWorkspaces,
            CatalogQueryActionV5::ListHarnessProfiles {
                runtime_handle,
                workspace_handle,
            } => Self::ListHarnessProfiles {
                runtime_handle,
                workspace_handle,
            },
            CatalogQueryActionV5::ListTaskPresets {
                runtime_handle,
                workspace_handle,
                harness_profile_handle,
            } => Self::ListTaskPresets {
                runtime_handle,
                workspace_handle,
                harness_profile_handle,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlActionV5 {
    StartRun {
        runtime_handle: String,
        workspace_handle: String,
        harness_profile_handle: String,
        preset_handle: String,
        intent_id: String,
    },
    CancelRun {
        run_handle: String,
        intent_id: String,
    },
}

impl From<ControlActionV5> for RemoteRequestActionV5 {
    fn from(action: ControlActionV5) -> Self {
        match action {
            ControlActionV5::StartRun {
                runtime_handle,
                workspace_handle,
                harness_profile_handle,
                preset_handle,
                intent_id,
            } => Self::StartRun {
                runtime_handle,
                workspace_handle,
                harness_profile_handle,
                preset_handle,
                intent_id,
            },
            ControlActionV5::CancelRun {
                run_handle,
                intent_id,
            } => Self::CancelRun {
                run_handle,
                intent_id,
            },
        }
    }
}

impl<'de> Deserialize<'de> for RemoteRequestActionV5 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            rename_all = "snake_case",
            rename_all_fields = "camelCase",
            deny_unknown_fields
        )]
        enum ClosedAction {
            GetDeviceInfo {},
            ListRuns {
                filter: RemoteRunFilterV5,
                page_size: u16,
                cursor: Option<String>,
            },
            GetRunDetail {
                run_handle: String,
            },
            GetRunTree {
                run_handle: String,
                max_depth: u8,
                max_nodes: u16,
            },
            ListRuntimes {},
            ListWorkspaces {},
            ListHarnessProfiles {
                runtime_handle: String,
                workspace_handle: String,
            },
            ListTaskPresets {
                runtime_handle: String,
                workspace_handle: String,
                harness_profile_handle: String,
            },
            StartRun {
                runtime_handle: String,
                workspace_handle: String,
                harness_profile_handle: String,
                preset_handle: String,
                intent_id: String,
            },
            CancelRun {
                run_handle: String,
                intent_id: String,
            },
        }

        Ok(match ClosedAction::deserialize(deserializer)? {
            ClosedAction::GetDeviceInfo {} => Self::GetDeviceInfo,
            ClosedAction::ListRuns {
                filter,
                page_size,
                cursor,
            } => Self::ListRuns {
                filter,
                page_size,
                cursor,
            },
            ClosedAction::GetRunDetail { run_handle } => Self::GetRunDetail { run_handle },
            ClosedAction::GetRunTree {
                run_handle,
                max_depth,
                max_nodes,
            } => Self::GetRunTree {
                run_handle,
                max_depth,
                max_nodes,
            },
            ClosedAction::ListRuntimes {} => Self::ListRuntimes,
            ClosedAction::ListWorkspaces {} => Self::ListWorkspaces,
            ClosedAction::ListHarnessProfiles {
                runtime_handle,
                workspace_handle,
            } => Self::ListHarnessProfiles {
                runtime_handle,
                workspace_handle,
            },
            ClosedAction::ListTaskPresets {
                runtime_handle,
                workspace_handle,
                harness_profile_handle,
            } => Self::ListTaskPresets {
                runtime_handle,
                workspace_handle,
                harness_profile_handle,
            },
            ClosedAction::StartRun {
                runtime_handle,
                workspace_handle,
                harness_profile_handle,
                preset_handle,
                intent_id,
            } => Self::StartRun {
                runtime_handle,
                workspace_handle,
                harness_profile_handle,
                preset_handle,
                intent_id,
            },
            ClosedAction::CancelRun {
                run_handle,
                intent_id,
            } => Self::CancelRun {
                run_handle,
                intent_id,
            },
        })
    }
}

impl From<RunQueryActionV5> for RemoteRequestActionV5 {
    fn from(action: RunQueryActionV5) -> Self {
        match action {
            RunQueryActionV5::ListRuns {
                filter,
                page_size,
                cursor,
            } => Self::ListRuns {
                filter,
                page_size,
                cursor,
            },
            RunQueryActionV5::GetRunDetail { run_handle } => Self::GetRunDetail { run_handle },
            RunQueryActionV5::GetRunTree {
                run_handle,
                max_depth,
                max_nodes,
            } => Self::GetRunTree {
                run_handle,
                max_depth,
                max_nodes,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestAckV5 {
    pub request_id: String,
    pub generation: u64,
    pub accepted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteResponseV5 {
    pub request_id: String,
    pub generation: u64,
    pub result: RemoteResponseResultV5,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetDeviceInfoResultV5 {
    pub schema_version: u16,
    pub device_name: String,
    pub client_version: String,
    pub capabilities: Vec<GatewayCapabilityV5>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceDescriptorV5 {
    pub workspace_handle: String,
    pub label: String,
    pub sensitivity: WorkspaceSensitivityV5,
    pub remote_start_enabled: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSensitivityV5 {
    Public,
    InternalSafe,
    Confidential,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCapabilityFacetV5 {
    Lifecycle,
    Cancel,
    Attention,
    Steer,
    ReconnectLive,
    OpenStored,
    Continue,
    Usage,
    Tools,
    ChildTopology,
    Artifacts,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeDescriptorV5 {
    pub runtime_handle: String,
    pub label: String,
    pub ready: bool,
    pub capability_facets: Vec<RuntimeCapabilityFacetV5>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessProfileDescriptorV5 {
    pub harness_profile_handle: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskPresetDescriptorV5 {
    pub preset_handle: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeCatalogResultV5 {
    pub items: Vec<RuntimeDescriptorV5>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceCatalogResultV5 {
    pub items: Vec<WorkspaceDescriptorV5>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessProfileCatalogResultV5 {
    pub items: Vec<HarnessProfileDescriptorV5>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskPresetCatalogResultV5 {
    pub items: Vec<TaskPresetDescriptorV5>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartRunResultV5 {
    pub accepted: bool,
    pub run_handle: String,
    pub desired_state: RemoteDesiredStateV5,
    pub phase: RemoteRunPhaseV5,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelRunResultV5 {
    pub accepted: bool,
    pub run_handle: String,
    pub desired_state: RemoteDesiredStateV5,
    pub phase: RemoteRunPhaseV5,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoteResponseResultV5 {
    GetDeviceInfo(GetDeviceInfoResultV5),
    ListRuns(RemoteRunPageV5),
    GetRunDetail(RemoteRunDetailV5),
    GetRunTree(RemoteRunTreeV5),
    ListRuntimes(RuntimeCatalogResultV5),
    ListWorkspaces(WorkspaceCatalogResultV5),
    ListHarnessProfiles(HarnessProfileCatalogResultV5),
    ListTaskPresets(TaskPresetCatalogResultV5),
    StartRun(StartRunResultV5),
    CancelRun(CancelRunResultV5),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogQueryResultV5 {
    ListRuntimes(RuntimeCatalogResultV5),
    ListWorkspaces(WorkspaceCatalogResultV5),
    ListHarnessProfiles(HarnessProfileCatalogResultV5),
    ListTaskPresets(TaskPresetCatalogResultV5),
}

impl TryFrom<RemoteResponseResultV5> for CatalogQueryResultV5 {
    type Error = ();

    fn try_from(result: RemoteResponseResultV5) -> Result<Self, Self::Error> {
        match result {
            RemoteResponseResultV5::ListRuntimes(value) => Ok(Self::ListRuntimes(value)),
            RemoteResponseResultV5::ListWorkspaces(value) => Ok(Self::ListWorkspaces(value)),
            RemoteResponseResultV5::ListHarnessProfiles(value) => {
                Ok(Self::ListHarnessProfiles(value))
            }
            RemoteResponseResultV5::ListTaskPresets(value) => Ok(Self::ListTaskPresets(value)),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlResultV5 {
    StartRun(StartRunResultV5),
    CancelRun(CancelRunResultV5),
}

impl TryFrom<RemoteResponseResultV5> for ControlResultV5 {
    type Error = ();

    fn try_from(result: RemoteResponseResultV5) -> Result<Self, Self::Error> {
        match result {
            RemoteResponseResultV5::StartRun(value) => Ok(Self::StartRun(value)),
            RemoteResponseResultV5::CancelRun(value) => Ok(Self::CancelRun(value)),
            _ => Err(()),
        }
    }
}

impl TryFrom<RemoteResponseResultV5> for RunQueryResultV5 {
    type Error = ();

    fn try_from(result: RemoteResponseResultV5) -> Result<Self, Self::Error> {
        match result {
            RemoteResponseResultV5::ListRuns(page) => Ok(Self::ListRuns(page)),
            RemoteResponseResultV5::GetRunDetail(detail) => Ok(Self::GetRunDetail(detail)),
            RemoteResponseResultV5::GetRunTree(tree) => Ok(Self::GetRunTree(tree)),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteErrorV5 {
    pub request_id: String,
    pub generation: u64,
    pub code: RemoteErrorCodeV5,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemoteErrorCodeV5 {
    InvalidRequest,
    RequestExpired,
    UnsupportedAction,
    UnsupportedCapability,
    Unauthorized,
    Conflict,
    Busy,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupersededV5 {
    pub generation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyRevokedV5 {
    pub generation: u64,
    pub code: PolicyRevocationCodeV5,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PolicyRevocationCodeV5 {
    TokenRotated,
    DeviceRevoked,
    DeviceDisabled,
    ScopeChanged,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum GatewayProtocolError {
    #[error("gateway frame exceeds the byte limit")]
    FrameTooLarge,
    #[error("gateway response exceeds the byte limit")]
    ResponseTooLarge,
    #[error("gateway frame is malformed")]
    Malformed,
    #[error("gateway frame failed validation")]
    Validation,
}

pub fn decode_client_frame(text: &str) -> Result<GatewayClientFrameV5, GatewayProtocolError> {
    check_frame_size(text.as_bytes())?;
    let frame = serde_json::from_str::<GatewayClientFrameV5>(text)
        .map_err(|_| GatewayProtocolError::Malformed)?;
    if matches!(frame, GatewayClientFrameV5::Response(_)) && text.len() > MAX_GATEWAY_RESPONSE_BYTES
    {
        return Err(GatewayProtocolError::ResponseTooLarge);
    }
    validate_client_frame(&frame)?;
    Ok(frame)
}

pub fn decode_server_frame(text: &str) -> Result<GatewayServerFrameV5, GatewayProtocolError> {
    check_frame_size(text.as_bytes())?;
    let frame = serde_json::from_str::<GatewayServerFrameV5>(text)
        .map_err(|_| GatewayProtocolError::Malformed)?;
    validate_server_frame(&frame)?;
    Ok(frame)
}

pub fn encode_client_frame(frame: &GatewayClientFrameV5) -> Result<String, GatewayProtocolError> {
    validate_client_frame(frame)?;
    let text = serde_json::to_string(frame).map_err(|_| GatewayProtocolError::Malformed)?;
    check_frame_size(text.as_bytes())?;
    if matches!(frame, GatewayClientFrameV5::Response(_)) && text.len() > MAX_GATEWAY_RESPONSE_BYTES
    {
        return Err(GatewayProtocolError::ResponseTooLarge);
    }
    Ok(text)
}

pub fn encode_server_frame(frame: &GatewayServerFrameV5) -> Result<String, GatewayProtocolError> {
    validate_server_frame(frame)?;
    let text = serde_json::to_string(frame).map_err(|_| GatewayProtocolError::Malformed)?;
    check_frame_size(text.as_bytes())?;
    Ok(text)
}

fn validate_client_frame(frame: &GatewayClientFrameV5) -> Result<(), GatewayProtocolError> {
    match frame {
        GatewayClientFrameV5::Hello(frame) => {
            check_protocol_version(frame.protocol_version)?;
            check_client_version(&frame.client_version)?;
            check_capabilities(&frame.capabilities)
        }
        GatewayClientFrameV5::Pong(frame) => {
            check_uuid(&frame.ping_id)?;
            check_generation(frame.generation)?;
            check_timestamp(frame.client_time)
        }
        GatewayClientFrameV5::RequestAck(frame) => {
            check_uuid(&frame.request_id)?;
            check_generation(frame.generation)
        }
        GatewayClientFrameV5::Response(frame) => {
            check_uuid(&frame.request_id)?;
            check_generation(frame.generation)?;
            validate_response(&frame.result)
        }
        GatewayClientFrameV5::Error(frame) => {
            check_uuid(&frame.request_id)?;
            check_generation(frame.generation)?;
            check_safe_text(
                &frame.message,
                MAX_ERROR_MESSAGE_CHARS,
                MAX_ERROR_MESSAGE_BYTES,
            )
        }
    }
}

fn validate_response(result: &RemoteResponseResultV5) -> Result<(), GatewayProtocolError> {
    match result {
        RemoteResponseResultV5::GetDeviceInfo(result) => {
            if result.schema_version != GATEWAY_PROTOCOL_VERSION {
                return Err(GatewayProtocolError::Validation);
            }
            check_safe_text(
                &result.device_name,
                MAX_DEVICE_NAME_CHARS,
                MAX_DEVICE_NAME_BYTES,
            )?;
            check_client_version(&result.client_version)?;
            check_capabilities(&result.capabilities)
        }
        RemoteResponseResultV5::ListRuns(page) => {
            RunQueryResultV5::ListRuns(page.clone())
                .validate()
                .map_err(|()| GatewayProtocolError::Validation)?;
            check_job_page_namespaces(page)
        }
        RemoteResponseResultV5::GetRunDetail(detail) => {
            RunQueryResultV5::GetRunDetail(detail.clone())
                .validate()
                .map_err(|()| GatewayProtocolError::Validation)?;
            check_handle(&detail.summary.run_handle, "run")?;
            Ok(())
        }
        RemoteResponseResultV5::GetRunTree(tree) => {
            RunQueryResultV5::GetRunTree(tree.clone())
                .validate()
                .map_err(|()| GatewayProtocolError::Validation)?;
            check_tree_namespaces(&tree.root)
        }
        RemoteResponseResultV5::ListRuntimes(result) => {
            if result.items.len() > MAX_CATALOG_ITEMS {
                return Err(GatewayProtocolError::Validation);
            }
            for item in &result.items {
                check_handle(&item.runtime_handle, "runtime")?;
                check_safe_text(&item.label, 120, 480)?;
                if item.capability_facets.len() > 16 {
                    return Err(GatewayProtocolError::Validation);
                }
                let facets = item
                    .capability_facets
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
                if facets.len() != item.capability_facets.len() {
                    return Err(GatewayProtocolError::Validation);
                }
            }
            Ok(())
        }
        RemoteResponseResultV5::ListWorkspaces(result) => {
            if result.items.len() > MAX_CATALOG_ITEMS {
                return Err(GatewayProtocolError::Validation);
            }
            for item in &result.items {
                check_handle(&item.workspace_handle, "workspace")?;
                check_safe_text(&item.label, 120, 480)?;
            }
            Ok(())
        }
        RemoteResponseResultV5::ListHarnessProfiles(result) => {
            if result.items.len() > MAX_CATALOG_ITEMS {
                return Err(GatewayProtocolError::Validation);
            }
            for item in &result.items {
                check_handle(&item.harness_profile_handle, "profile")?;
                check_safe_text(&item.label, 120, 480)?;
            }
            Ok(())
        }
        RemoteResponseResultV5::ListTaskPresets(result) => {
            if result.items.len() > MAX_CATALOG_ITEMS {
                return Err(GatewayProtocolError::Validation);
            }
            for item in &result.items {
                check_handle(&item.preset_handle, "preset")?;
                check_safe_text(&item.label, 120, 480)?;
            }
            Ok(())
        }
        RemoteResponseResultV5::StartRun(result) => {
            check_handle(&result.run_handle, "run")?;
            if result.accepted
                && (result.desired_state != RemoteDesiredStateV5::Running
                    || result.phase != RemoteRunPhaseV5::Pending)
            {
                return Err(GatewayProtocolError::Validation);
            }
            Ok(())
        }
        RemoteResponseResultV5::CancelRun(result) => {
            check_handle(&result.run_handle, "run")?;
            if result.accepted
                && (result.desired_state != RemoteDesiredStateV5::Stopped
                    || !matches!(
                        result.phase,
                        RemoteRunPhaseV5::Stopping
                            | RemoteRunPhaseV5::Finalizing
                            | RemoteRunPhaseV5::Finished
                    ))
            {
                return Err(GatewayProtocolError::Validation);
            }
            Ok(())
        }
    }
}

fn check_job_page_namespaces(page: &RemoteRunPageV5) -> Result<(), GatewayProtocolError> {
    for item in &page.items {
        check_handle(&item.run_handle, "run")?;
    }
    page.next_cursor
        .as_deref()
        .map_or(Ok(()), |value| check_handle(value, "cursor"))
}

fn check_tree_namespaces(
    node: &super::run::RemoteRunTreeNodeV5,
) -> Result<(), GatewayProtocolError> {
    check_handle(&node.run_handle, "run")?;
    for child in &node.children {
        check_tree_namespaces(child)?;
    }
    Ok(())
}

fn validate_server_frame(frame: &GatewayServerFrameV5) -> Result<(), GatewayProtocolError> {
    match frame {
        GatewayServerFrameV5::Welcome(frame) => {
            check_protocol_version(frame.protocol_version)?;
            check_uuid(&frame.connection_id)?;
            check_generation(frame.generation)?;
            if frame.heartbeat_seconds != GATEWAY_HEARTBEAT_SECONDS
                || frame.max_frame_bytes != MAX_GATEWAY_FRAME_BYTES
            {
                return Err(GatewayProtocolError::Validation);
            }
            check_timestamp(frame.server_time)
        }
        GatewayServerFrameV5::Ping(frame) => {
            check_uuid(&frame.ping_id)?;
            check_generation(frame.generation)?;
            check_timestamp(frame.sent_at)
        }
        GatewayServerFrameV5::Request(frame) => {
            check_uuid(&frame.request_id)?;
            check_generation(frame.generation)?;
            check_timestamp(frame.issued_at)?;
            if frame.expires_at <= frame.issued_at
                || frame.expires_at - frame.issued_at > MAX_GATEWAY_REQUEST_TTL_MS
            {
                return Err(GatewayProtocolError::Validation);
            }
            validate_request_action(&frame.action)
        }
        GatewayServerFrameV5::Superseded(frame) => check_generation(frame.generation),
        GatewayServerFrameV5::PolicyRevoked(frame) => check_generation(frame.generation),
    }
}

fn validate_request_action(action: &RemoteRequestActionV5) -> Result<(), GatewayProtocolError> {
    match action {
        RemoteRequestActionV5::GetDeviceInfo => Ok(()),
        RemoteRequestActionV5::ListRuns {
            page_size, cursor, ..
        } => {
            if *page_size != 10 {
                return Err(GatewayProtocolError::Validation);
            }
            cursor
                .as_deref()
                .map_or(Ok(()), |value| check_handle(value, "cursor"))
        }
        RemoteRequestActionV5::GetRunDetail { run_handle } => check_handle(run_handle, "run"),
        RemoteRequestActionV5::GetRunTree {
            run_handle,
            max_depth,
            max_nodes,
        } => {
            check_handle(run_handle, "run")?;
            if *max_depth <= 4 && (1..=100).contains(max_nodes) {
                Ok(())
            } else {
                Err(GatewayProtocolError::Validation)
            }
        }
        RemoteRequestActionV5::ListRuntimes | RemoteRequestActionV5::ListWorkspaces => Ok(()),
        RemoteRequestActionV5::ListHarnessProfiles {
            runtime_handle,
            workspace_handle,
        } => {
            check_handle(runtime_handle, "runtime")?;
            check_handle(workspace_handle, "workspace")
        }
        RemoteRequestActionV5::ListTaskPresets {
            runtime_handle,
            workspace_handle,
            harness_profile_handle,
        } => {
            check_handle(runtime_handle, "runtime")?;
            check_handle(workspace_handle, "workspace")?;
            check_handle(harness_profile_handle, "profile")
        }
        RemoteRequestActionV5::StartRun {
            runtime_handle,
            workspace_handle,
            harness_profile_handle,
            preset_handle,
            intent_id,
        } => {
            check_handle(runtime_handle, "runtime")?;
            check_handle(workspace_handle, "workspace")?;
            check_handle(harness_profile_handle, "profile")?;
            check_handle(preset_handle, "preset")?;
            check_intent_id(intent_id)
        }
        RemoteRequestActionV5::CancelRun {
            run_handle,
            intent_id,
        } => {
            check_handle(run_handle, "run")?;
            check_intent_id(intent_id)
        }
    }
}

pub fn validate_request_action_v5(
    action: &RemoteRequestActionV5,
) -> Result<(), GatewayProtocolError> {
    validate_request_action(action)
}

fn check_frame_size(bytes: &[u8]) -> Result<(), GatewayProtocolError> {
    if bytes.len() <= MAX_GATEWAY_FRAME_BYTES {
        Ok(())
    } else {
        Err(GatewayProtocolError::FrameTooLarge)
    }
}

fn check_protocol_version(version: u16) -> Result<(), GatewayProtocolError> {
    if version == GATEWAY_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(GatewayProtocolError::Validation)
    }
}

fn check_uuid(value: &str) -> Result<(), GatewayProtocolError> {
    let parsed = Uuid::parse_str(value).map_err(|_| GatewayProtocolError::Validation)?;
    if !parsed.is_nil() && parsed.to_string() == value {
        Ok(())
    } else {
        Err(GatewayProtocolError::Validation)
    }
}

fn check_generation(generation: u64) -> Result<(), GatewayProtocolError> {
    if generation > 0 {
        Ok(())
    } else {
        Err(GatewayProtocolError::Validation)
    }
}

fn check_timestamp(timestamp: i64) -> Result<(), GatewayProtocolError> {
    if timestamp >= 0 {
        Ok(())
    } else {
        Err(GatewayProtocolError::Validation)
    }
}

fn check_client_version(value: &str) -> Result<(), GatewayProtocolError> {
    if !value.is_empty()
        && value.len() <= MAX_CLIENT_VERSION_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        Ok(())
    } else {
        Err(GatewayProtocolError::Validation)
    }
}

fn check_capabilities(values: &[GatewayCapabilityV5]) -> Result<(), GatewayProtocolError> {
    if values.len() > MAX_CAPABILITIES {
        return Err(GatewayProtocolError::Validation);
    }
    let unique = values.iter().collect::<BTreeSet<_>>();
    if unique.len() == values.len() {
        Ok(())
    } else {
        Err(GatewayProtocolError::Validation)
    }
}

fn check_handle(value: &str, namespace: &str) -> Result<(), GatewayProtocolError> {
    let prefix = format!("{namespace}_");
    if value.starts_with(&prefix)
        && (32..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(GatewayProtocolError::Validation)
    }
}

fn check_intent_id(value: &str) -> Result<(), GatewayProtocolError> {
    if value.len() > MAX_INTENT_ID_BYTES || value.trim() != value || value.is_empty() {
        return Err(GatewayProtocolError::Validation);
    }
    if Uuid::parse_str(value).is_ok() {
        return Ok(());
    }
    check_handle(value, "intent")
}

fn check_safe_text(
    value: &str,
    max_chars: usize,
    max_bytes: usize,
) -> Result<(), GatewayProtocolError> {
    let char_count = value.chars().count();
    let valid = value.trim() == value
        && (1..=max_chars).contains(&char_count)
        && value.len() <= max_bytes
        && value.chars().all(|character| {
            !character.is_control()
                && !matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        });
    if valid {
        Ok(())
    } else {
        Err(GatewayProtocolError::Validation)
    }
}

#[cfg(test)]
mod capability_requirements_tests {
    use super::*;

    #[test]
    fn every_closed_remote_action_has_one_central_capability_requirement_set() {
        let action_requirements = [
            (
                RemoteRequestActionV5::GetDeviceInfo,
                GatewayCapabilityV5::DEVICE_INFO_REQUIREMENTS,
            ),
            (
                RemoteRequestActionV5::ListRuns {
                    filter: RemoteRunFilterV5::All,
                    page_size: 10,
                    cursor: None,
                },
                GatewayCapabilityV5::RUN_QUERY_REQUIREMENTS,
            ),
            (
                RemoteRequestActionV5::GetRunDetail {
                    run_handle: "run_aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                },
                GatewayCapabilityV5::RUN_QUERY_REQUIREMENTS,
            ),
            (
                RemoteRequestActionV5::GetRunTree {
                    run_handle: "run_aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    max_depth: 1,
                    max_nodes: 1,
                },
                GatewayCapabilityV5::RUN_QUERY_REQUIREMENTS,
            ),
            (
                RemoteRequestActionV5::ListRuntimes,
                GatewayCapabilityV5::CATALOG_REQUIREMENTS,
            ),
            (
                RemoteRequestActionV5::ListWorkspaces,
                GatewayCapabilityV5::CATALOG_REQUIREMENTS,
            ),
            (
                RemoteRequestActionV5::ListHarnessProfiles {
                    runtime_handle: "runtime_aaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    workspace_handle: "workspace_aaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                },
                GatewayCapabilityV5::CATALOG_REQUIREMENTS,
            ),
            (
                RemoteRequestActionV5::ListTaskPresets {
                    runtime_handle: "runtime_aaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    workspace_handle: "workspace_aaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    harness_profile_handle: "profile_aaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                },
                GatewayCapabilityV5::CATALOG_REQUIREMENTS,
            ),
            (
                RemoteRequestActionV5::StartRun {
                    runtime_handle: "runtime_aaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    workspace_handle: "workspace_aaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    harness_profile_handle: "profile_aaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    preset_handle: "preset_aaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    intent_id: "intent_aaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                },
                GatewayCapabilityV5::START_RUN_REQUIREMENTS,
            ),
            (
                RemoteRequestActionV5::CancelRun {
                    run_handle: "run_aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    intent_id: "intent_aaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                },
                GatewayCapabilityV5::CANCEL_RUN_REQUIREMENTS,
            ),
        ];
        for (action, expected) in action_requirements {
            assert_eq!(GatewayCapabilityV5::requirements_for(&action), expected);
        }

        let control = BTreeSet::from([GatewayCapabilityV5::RunControlV2]);
        assert!(GatewayCapabilityV5::has_all(
            &control,
            GatewayCapabilityV5::CANCEL_RUN_REQUIREMENTS
        ));
        assert!(!GatewayCapabilityV5::has_all(
            &control,
            GatewayCapabilityV5::START_RUN_REQUIREMENTS
        ));
    }
}
