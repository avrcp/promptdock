//! Complete Admin API v2 wire model.
//!
//! These are transport-only shapes. Application and presentation models may
//! map from them, but they must never redefine HTTP request or response data.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::{ADMIN_API_MAJOR, ADMIN_SCHEMA_VERSION, AdminCapability, InboundCommand};

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminMetaV2 {
    #[schema(value_type = u32, minimum = 2, maximum = 2, example = 2)]
    pub schema_version: u32,
    pub generated_at: i64,
    #[schema(value_type = u32, minimum = 2, maximum = 2, example = 2)]
    pub admin_api_version: u32,
    pub relay_version: String,
    pub capabilities: Vec<AdminCapability>,
}

impl AdminMetaV2 {
    pub fn new(
        generated_at: i64,
        relay_version: impl Into<String>,
        capabilities: Vec<AdminCapability>,
    ) -> Self {
        Self {
            schema_version: ADMIN_SCHEMA_VERSION,
            generated_at,
            admin_api_version: ADMIN_API_MAJOR,
            relay_version: relay_version.into(),
            capabilities,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminErrorV2 {
    pub code: String,
    pub message: String,
    pub request_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminPageV2<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub total: Option<u64>,
    pub generated_at: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminDeviceStateV2 {
    Revoked,
    Disabled,
    Online,
    Offline,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminGatewayStateV2 {
    Connected,
    Offline,
    NotEnabled,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
pub enum DeviceScopeV2 {
    #[serde(rename = "notify:write")]
    NotifyWrite,
    #[serde(rename = "notify:read_own")]
    NotifyReadOwn,
    #[serde(rename = "channel:read")]
    ChannelRead,
    #[serde(rename = "channel:manage")]
    ChannelManage,
    #[serde(rename = "gateway:connect")]
    GatewayConnect,
    #[serde(rename = "job:query")]
    JobQuery,
    #[serde(rename = "job:control")]
    JobControl,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceListItemV2 {
    pub id: String,
    pub name: String,
    pub state: AdminDeviceStateV2,
    pub gateway_state: AdminGatewayStateV2,
    pub scopes: Vec<DeviceScopeV2>,
    pub token_format_version: u32,
    pub last_rotated_at: Option<i64>,
    pub client_version: Option<String>,
    pub last_seen_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceDetailV2 {
    pub id: String,
    pub name: String,
    pub state: AdminDeviceStateV2,
    pub gateway_state: AdminGatewayStateV2,
    pub scopes: Vec<DeviceScopeV2>,
    pub token_format_version: u32,
    pub last_rotated_at: Option<i64>,
    pub client_version: Option<String>,
    pub last_seen_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub full_device_uuid: String,
    pub capabilities: Vec<String>,
    pub last_heartbeat_at: Option<i64>,
    pub connected_at: Option<i64>,
    pub gateway_generation: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, IntoParams, Serialize, ToSchema)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DevicesQueryV2 {
    #[param(max_length = 64)]
    pub search: Option<String>,
    pub state: Option<AdminDeviceStateV2>,
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDeviceRequestV2 {
    #[schema(min_length = 1, max_length = 64)]
    pub name: String,
    #[schema(min_items = 1, max_items = 7)]
    pub scopes: Vec<DeviceScopeV2>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyMutationRequestV2 {}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCredentialActionV2 {
    Create,
    Rotate,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceCredentialReceiptV2 {
    pub receipt_id: String,
    pub action: DeviceCredentialActionV2,
    pub device_id: String,
    pub one_time_token: String,
    pub issued_at: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
pub enum DeviceActionV2 {
    #[serde(rename = "device.enable")]
    Enable,
    #[serde(rename = "device.disable")]
    Disable,
    #[serde(rename = "device.revoke")]
    Revoke,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceActionReceiptV2 {
    pub receipt_id: String,
    pub action: DeviceActionV2,
    pub completed_at: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelayHealthV2 {
    Healthy,
    Degraded,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WechatStateV2 {
    Disconnected,
    ConnectedAwaitingActivation,
    Ready,
    Degraded,
    NeedsReconnect,
    CredentialsUnreadable,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueSummaryV2 {
    pub pending: i64,
    pub sending: i64,
    pub retrying: i64,
    pub blocked: i64,
    pub failed: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelaySummaryV2 {
    pub health: RelayHealthV2,
    pub version: String,
    pub uptime_seconds: u64,
    pub last_check_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatSummaryV2 {
    pub state: WechatStateV2,
    pub account_hint: Option<String>,
    pub last_poll_at: Option<i64>,
    pub last_context_at: Option<i64>,
    pub last_provider_accepted_at: Option<i64>,
    pub queue: QueueSummaryV2,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatStatusV2 {
    #[schema(value_type = u32, minimum = 2, maximum = 2)]
    pub schema_version: u32,
    pub generated_at: i64,
    pub state: WechatStateV2,
    pub account_hint: Option<String>,
    pub last_poll_at: Option<i64>,
    pub last_context_at: Option<i64>,
    pub last_provider_accepted_at: Option<i64>,
    pub queue: QueueSummaryV2,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceStateCountV2 {
    pub online: i64,
    pub offline: i64,
    pub disabled: i64,
    pub revoked: i64,
    pub unknown: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceSummaryV2 {
    pub enabled_total: i64,
    pub online_total: i64,
    pub recently_offline_total: i64,
    pub states: DeviceStateCountV2,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayConnectionV2 {
    pub device_id: String,
    pub device_name: String,
    pub generation: u64,
    pub connected: bool,
    pub last_heartbeat_at: Option<i64>,
    pub client_version: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverityV2 {
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertComponentV2 {
    Relay,
    Wechat,
    Outbox,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CurrentAlertV2 {
    pub id: String,
    pub code: String,
    pub severity: AlertSeverityV2,
    pub component: AlertComponentV2,
    pub message: String,
    pub observed_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminOverviewV2 {
    #[schema(value_type = u32, minimum = 2, maximum = 2)]
    pub schema_version: u32,
    pub generated_at: i64,
    pub relay: RelaySummaryV2,
    pub wechat: WechatSummaryV2,
    pub devices: DeviceSummaryV2,
    pub queue: QueueSummaryV2,
    pub gateway_connections: Vec<GatewayConnectionV2>,
    pub current_alerts: Vec<CurrentAlertV2>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelEventKindV2 {
    Poll,
    Context,
    Login,
    LoginCancelled,
    LoginExpired,
    LoginFailed,
    Disconnect,
    Reconnect,
    Test,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelEventItemV2 {
    pub id: String,
    pub kind: ChannelEventKindV2,
    pub occurred_at: i64,
    pub safe_message: String,
}

#[derive(Clone, Debug, Default, Deserialize, IntoParams, Serialize, ToSchema)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatEventsQueryV2 {
    pub kinds: Option<Vec<ChannelEventKindV2>>,
    pub since_at: Option<i64>,
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WechatLoginStateV2 {
    FetchingQr,
    WaitingScan,
    Scanned,
    VerifyCodeRequired,
    RefreshingQr,
    Confirmed,
    AlreadyConnected,
    Expired,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatLoginV2 {
    pub login_id: String,
    pub state: WechatLoginStateV2,
    pub qr_content: Option<String>,
    pub expires_at: i64,
    pub can_submit_verify_code: bool,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatLoginStartRequestV2 {
    pub force_fresh: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatLoginVerifyRequestV2 {
    #[schema(pattern = "^[0-9]{1,16}$")]
    pub code: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisconnectWechatRequestV2 {
    #[schema(min_length = 1, max_length = 120)]
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WechatTestStateV2 {
    AcceptedByRelay,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
pub enum WechatActionV2 {
    #[serde(rename = "wechat.disconnect")]
    Disconnect,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatTestReceiptV2 {
    pub receipt_id: String,
    pub state: WechatTestStateV2,
    pub issued_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WechatDisconnectReceiptV2 {
    pub receipt_id: String,
    pub action: WechatActionV2,
    pub completed_at: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOriginV2 {
    Device,
    System,
    Admin,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKindV2 {
    RunEvent,
    Test,
    InteractiveReply,
    Activation,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStateV2 {
    Queued,
    Sending,
    Retrying,
    Blocked,
    Accepted,
    Failed,
    Cancelled,
    Expired,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InboundStateV2 {
    Received,
    Dispatching,
    WaitingGateway,
    ReplyQueued,
    Expired,
    DeadLetter,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryListItemV2 {
    pub id: String,
    pub origin: DeliveryOriginV2,
    pub origin_label: String,
    pub kind: DeliveryKindV2,
    pub state: DeliveryStateV2,
    pub priority: u8,
    pub attempt_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1, maximum = 128)]
    pub segment_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 0, maximum = 128)]
    pub accepted_segments: Option<u32>,
    pub error_code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InteractiveReplyListItemV2 {
    pub id: String,
    pub command_ref: String,
    pub target_fingerprint: String,
    pub state: DeliveryStateV2,
    pub occurred_at: i64,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboundCommandListItemV2 {
    pub id: String,
    pub command: InboundCommand,
    pub sender_hint: String,
    pub state: InboundStateV2,
    pub attempt_count: u32,
    pub error_code: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, Deserialize, IntoParams, Serialize, ToSchema)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveriesQueryV2 {
    pub state: Option<DeliveryStateV2>,
    pub origin: Option<DeliveryOriginV2>,
    pub since_at: Option<i64>,
    pub until_at: Option<i64>,
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, IntoParams, Serialize, ToSchema)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InteractiveRepliesQueryV2 {
    pub state: Option<DeliveryStateV2>,
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, IntoParams, Serialize, ToSchema)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboundCommandsQueryV2 {
    pub command: Option<InboundCommand>,
    pub state: Option<InboundStateV2>,
    pub since_at: Option<i64>,
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

/// A result-page projection deliberately excludes plaintext body, encrypted
/// storage, share bearer tokens and resolved URLs. Those values never belong
/// on an Admin list or detail response.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultPageStateV2 {
    Available,
    Revoked,
    Expired,
    ContentUnavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultNotificationStatusV2 {
    PendingChannel,
    SendingChannel,
    RetryWait,
    ProviderAccepted,
    BlockedActivation,
    BlockedReconnect,
    DeadLetter,
    Expired,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultListItemV2 {
    /// Relay's globally unique results-row identifier. Admin commands use this
    /// key because result_id is scoped to its owning device.
    pub result_row_id: String,
    pub result_id: String,
    pub owner_device_id: String,
    pub safe_title: String,
    pub source: String,
    pub content_mode: String,
    pub source_hash: String,
    pub accepted_at: i64,
    pub updated_at: i64,
    pub page_state: ResultPageStateV2,
    pub page_expires_at: i64,
    pub body_bytes: u64,
    pub body_purged_at: Option<i64>,
    pub notification_id: String,
    pub notification_status: ResultNotificationStatusV2,
    pub notification_attempt_count: u32,
    pub notification_last_error_code: Option<String>,
    pub target_account_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultDetailV2 {
    /// Relay's globally unique results-row identifier. Admin commands use this
    /// key because result_id is scoped to its owning device.
    pub result_row_id: String,
    pub result_id: String,
    pub owner_device_id: String,
    pub safe_title: String,
    pub source: String,
    pub content_mode: String,
    pub source_hash: String,
    pub accepted_at: i64,
    pub updated_at: i64,
    pub page_state: ResultPageStateV2,
    pub page_expires_at: i64,
    pub body_bytes: u64,
    pub body_purged_at: Option<i64>,
    pub notification_id: String,
    pub notification_status: ResultNotificationStatusV2,
    pub notification_attempt_count: u32,
    pub notification_last_error_code: Option<String>,
    pub target_account_fingerprint: Option<String>,
    pub correlation_key: String,
    pub result_revision: String,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub revoked_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, IntoParams, Serialize, ToSchema)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultsQueryV2 {
    pub page_state: Option<ResultPageStateV2>,
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultRevokeRequestV2 {
    #[schema(min_length = 1, max_length = 128)]
    pub request_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultReceiptV2 {
    #[schema(value_type = u32, minimum = 1, maximum = 1)]
    pub schema_version: u32,
    pub result_id: String,
    pub source_hash: String,
    pub accepted_at: i64,
    pub updated_at: i64,
    pub page_state: ResultPageStateV2,
    pub page_expires_at: i64,
    pub notification_id: String,
    pub notification_status: ResultNotificationStatusV2,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseIntegrityV2 {
    Ok,
    Failed,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WalStatusV2 {
    Enabled,
    Disabled,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComponentHealthV2 {
    Healthy,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
pub enum DatabaseSizeBucketV2 {
    #[serde(rename = "lt_10mb")]
    Lt10Mb,
    #[serde(rename = "10_to_100mb")]
    Between10And100Mb,
    #[serde(rename = "100_to_500mb")]
    Between100And500Mb,
    #[serde(rename = "gt_500mb")]
    Gt500Mb,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SafeWorkerStateV2 {
    Running,
    Idle,
    Degraded,
    Failed,
    Disabled,
    Stopping,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetentionResultV2 {
    Success,
    Failed,
    NotObserved,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicBindClassV2 {
    Loopback,
    Container,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminModeV2 {
    ReadOnly,
    Operator,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
pub enum MaintenanceActionV2 {
    #[serde(rename = "maintenance.retention")]
    Retention,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildSummaryV2 {
    pub relay_version: String,
    pub git_commit: Option<String>,
    pub build_time: Option<String>,
    pub rust_version: Option<String>,
    pub api_version: String,
    pub gateway_version: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatabaseSummaryV2 {
    pub schema_identity: String,
    pub schema_revision: i64,
    pub integrity_status: DatabaseIntegrityV2,
    pub wal_status: WalStatusV2,
    pub foreign_keys_enabled: bool,
    pub pool_health: ComponentHealthV2,
    pub size_bucket: DatabaseSizeBucketV2,
    pub last_retention_pass_at: Option<i64>,
}
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerSummaryV2 {
    pub name: String,
    pub state: SafeWorkerStateV2,
    pub last_tick_at: Option<i64>,
    pub detail: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetentionSummaryV2 {
    pub accepted_days: u64,
    pub dead_letter_days: u64,
    pub inbound_terminal_days: u64,
    pub inbound_expired_days: u64,
    pub last_run_at: Option<i64>,
    pub last_result: RetentionResultV2,
}
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafeConfigurationSummaryV2 {
    pub wechat_enabled: bool,
    pub public_bind_class: PublicBindClassV2,
    pub admin_bind_class: String,
    pub admin_mode: AdminModeV2,
    pub forwarded_https_observed: bool,
    pub feature_flags: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SystemSnapshotV2 {
    #[schema(value_type = u32, minimum = 2, maximum = 2)]
    pub schema_version: u32,
    pub generated_at: i64,
    pub build: BuildSummaryV2,
    pub database: DatabaseSummaryV2,
    pub workers: Vec<WorkerSummaryV2>,
    pub retention: RetentionSummaryV2,
    pub configuration: SafeConfigurationSummaryV2,
    pub current_alerts: Vec<CurrentAlertV2>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetentionReceiptV2 {
    pub receipt_id: String,
    pub action: MaintenanceActionV2,
    pub outbox_deleted: u64,
    pub inbound_deleted: u64,
    pub selection_deleted: u64,
    pub started_at: i64,
    pub completed_at: i64,
}

macro_rules! page_type {
    ($name:ident, $item:ty) => {
        #[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name {
            pub items: Vec<$item>,
            pub next_cursor: Option<String>,
            pub total: Option<u64>,
            pub generated_at: i64,
        }
    };
}

page_type!(DevicesPageV2, DeviceListItemV2);
page_type!(WechatEventsPageV2, ChannelEventItemV2);
page_type!(DeliveriesPageV2, DeliveryListItemV2);
page_type!(InteractiveRepliesPageV2, InteractiveReplyListItemV2);
page_type!(InboundCommandsPageV2, InboundCommandListItemV2);
page_type!(ResultsPageV2, ResultListItemV2);
