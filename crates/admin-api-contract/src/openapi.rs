//! Deterministic OpenAPI 3.1 document generated from the Rust wire model.

use utoipa::OpenApi;

use crate::{AdminCapability, InboundCommand, wire::*};

#[utoipa::path(get, path = "/admin/api/v2/meta", operation_id = "getAdminMetaV2", responses((status = 200, body = AdminMetaV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn get_meta() {}
#[utoipa::path(get, path = "/admin/api/v2/overview", operation_id = "getAdminOverviewV2", responses((status = 200, body = AdminOverviewV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn get_overview() {}
#[utoipa::path(get, path = "/admin/api/v2/devices", operation_id = "listAdminDevicesV2", params(DevicesQueryV2), responses((status = 200, body = DevicesPageV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn list_devices() {}
#[utoipa::path(post, path = "/admin/api/v2/devices", operation_id = "createAdminDeviceV2", request_body(content = CreateDeviceRequestV2, content_type = "application/json"), responses((status = 201, body = DeviceCredentialReceiptV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn create_device() {}
#[utoipa::path(get, path = "/admin/api/v2/devices/{device_id}", operation_id = "getAdminDeviceV2", params(("device_id" = String, Path, description = "Device UUID")), responses((status = 200, body = DeviceDetailV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn get_device() {}
#[utoipa::path(post, path = "/admin/api/v2/devices/{device_id}/rotate", operation_id = "rotateAdminDeviceV2", params(("device_id" = String, Path)), request_body = EmptyMutationRequestV2, responses((status = 200, body = DeviceCredentialReceiptV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn rotate_device() {}
#[utoipa::path(post, path = "/admin/api/v2/devices/{device_id}/enable", operation_id = "enableAdminDeviceV2", params(("device_id" = String, Path)), request_body = EmptyMutationRequestV2, responses((status = 200, body = DeviceActionReceiptV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn enable_device() {}
#[utoipa::path(post, path = "/admin/api/v2/devices/{device_id}/disable", operation_id = "disableAdminDeviceV2", params(("device_id" = String, Path)), request_body = EmptyMutationRequestV2, responses((status = 200, body = DeviceActionReceiptV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn disable_device() {}
#[utoipa::path(post, path = "/admin/api/v2/devices/{device_id}/revoke", operation_id = "revokeAdminDeviceV2", params(("device_id" = String, Path)), request_body = EmptyMutationRequestV2, responses((status = 200, body = DeviceActionReceiptV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn revoke_device() {}
#[utoipa::path(get, path = "/admin/api/v2/wechat/status", operation_id = "getAdminWechatStatusV2", responses((status = 200, body = WechatStatusV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn get_wechat_status() {}
#[utoipa::path(get, path = "/admin/api/v2/wechat/events", operation_id = "listAdminWechatEventsV2", params(WechatEventsQueryV2), responses((status = 200, body = WechatEventsPageV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn list_wechat_events() {}
#[utoipa::path(post, path = "/admin/api/v2/wechat/test", operation_id = "testAdminWechatV2", request_body = EmptyMutationRequestV2, responses((status = 200, body = WechatTestReceiptV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn test_wechat() {}
#[utoipa::path(post, path = "/admin/api/v2/wechat/disconnect", operation_id = "disconnectAdminWechatV2", request_body = DisconnectWechatRequestV2, responses((status = 200, body = WechatDisconnectReceiptV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn disconnect_wechat() {}
#[utoipa::path(post, path = "/admin/api/v2/wechat/login", operation_id = "startAdminWechatLoginV2", request_body = WechatLoginStartRequestV2, responses((status = 200, body = WechatLoginV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn start_wechat_login() {}
#[utoipa::path(get, path = "/admin/api/v2/wechat/login/{login_id}", operation_id = "getAdminWechatLoginV2", params(("login_id" = String, Path)), responses((status = 200, body = WechatLoginV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn get_wechat_login() {}
#[utoipa::path(delete, path = "/admin/api/v2/wechat/login/{login_id}", operation_id = "cancelAdminWechatLoginV2", params(("login_id" = String, Path)), request_body = EmptyMutationRequestV2, responses((status = 200, body = WechatLoginV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn cancel_wechat_login() {}
#[utoipa::path(post, path = "/admin/api/v2/wechat/login/{login_id}/verify", operation_id = "verifyAdminWechatLoginV2", params(("login_id" = String, Path)), request_body = WechatLoginVerifyRequestV2, responses((status = 200, body = WechatLoginV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn verify_wechat_login() {}
#[utoipa::path(get, path = "/admin/api/v2/deliveries", operation_id = "listAdminDeliveriesV2", params(DeliveriesQueryV2), responses((status = 200, body = DeliveriesPageV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn list_deliveries() {}
#[utoipa::path(get, path = "/admin/api/v2/interactive-replies", operation_id = "listAdminInteractiveRepliesV2", params(InteractiveRepliesQueryV2), responses((status = 200, body = InteractiveRepliesPageV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn list_interactive_replies() {}
#[utoipa::path(get, path = "/admin/api/v2/inbound-commands", operation_id = "listAdminInboundCommandsV2", params(InboundCommandsQueryV2), responses((status = 200, body = InboundCommandsPageV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn list_inbound_commands() {}
#[utoipa::path(get, path = "/admin/api/v2/results", operation_id = "listAdminResultsV2", params(ResultsQueryV2), responses((status = 200, body = ResultsPageV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn list_results() {}
#[utoipa::path(get, path = "/admin/api/v2/results/{result_row_id}", operation_id = "getAdminResultV2", params(("result_row_id" = String, Path)), responses((status = 200, body = ResultDetailV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn get_result() {}
#[utoipa::path(post, path = "/admin/api/v2/results/{result_row_id}/revoke", operation_id = "revokeAdminResultV2", params(("result_row_id" = String, Path)), request_body(content = ResultRevokeRequestV2, content_type = "application/json"), responses((status = 200, body = ResultReceiptV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn revoke_result() {}
#[utoipa::path(get, path = "/admin/api/v2/system", operation_id = "getAdminSystemV2", responses((status = 200, body = SystemSnapshotV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn get_system() {}
#[utoipa::path(post, path = "/admin/api/v2/maintenance/retention", operation_id = "runAdminRetentionV2", request_body = EmptyMutationRequestV2, responses((status = 200, body = RetentionReceiptV2), (status = "default", description = "Admin API error", body = AdminErrorV2)), tag = "admin-v2")]
pub fn run_retention() {}

#[derive(OpenApi)]
#[openapi(
    info(title = "PromptDock Relay Admin API", version = "2.0.0", description = "Restricted same-origin Relay operator API. Rust wire types are the only editable contract authority."),
    paths(get_meta, get_overview, list_devices, create_device, get_device, rotate_device, enable_device, disable_device, revoke_device, get_wechat_status, list_wechat_events, test_wechat, disconnect_wechat, start_wechat_login, get_wechat_login, cancel_wechat_login, verify_wechat_login, list_deliveries, list_interactive_replies, list_inbound_commands, list_results, get_result, revoke_result, get_system, run_retention),
    components(schemas(
        AdminCapability, InboundCommand, AdminMetaV2, AdminErrorV2, AdminPageV2<String>,
        DevicesPageV2, WechatEventsPageV2, DeliveriesPageV2, InteractiveRepliesPageV2,
        InboundCommandsPageV2,
        AdminDeviceStateV2, AdminGatewayStateV2, DeviceScopeV2, DeviceListItemV2, DeviceDetailV2, DevicesQueryV2,
        CreateDeviceRequestV2, EmptyMutationRequestV2, DeviceCredentialActionV2, DeviceCredentialReceiptV2,
        DeviceActionV2, DeviceActionReceiptV2, RelayHealthV2, WechatStateV2, QueueSummaryV2,
        RelaySummaryV2, WechatSummaryV2, WechatStatusV2, DeviceStateCountV2, DeviceSummaryV2, GatewayConnectionV2,
        AlertSeverityV2, AlertComponentV2, CurrentAlertV2, AdminOverviewV2, ChannelEventKindV2,
        ChannelEventItemV2, WechatEventsQueryV2, WechatLoginStateV2, WechatLoginV2,
        WechatLoginStartRequestV2, WechatLoginVerifyRequestV2, DisconnectWechatRequestV2,
        WechatTestStateV2, WechatActionV2, WechatTestReceiptV2, WechatDisconnectReceiptV2, DeliveryOriginV2,
        DeliveryKindV2, DeliveryStateV2, InboundStateV2, DeliveryListItemV2,
        InteractiveReplyListItemV2, InboundCommandListItemV2, DeliveriesQueryV2,
        InteractiveRepliesQueryV2, InboundCommandsQueryV2, DatabaseIntegrityV2, WalStatusV2,
        ResultPageStateV2, ResultNotificationStatusV2, ResultListItemV2, ResultDetailV2,
        ResultsQueryV2, ResultsPageV2, ResultRevokeRequestV2, ResultReceiptV2,
        ComponentHealthV2, DatabaseSizeBucketV2, SafeWorkerStateV2, RetentionResultV2,
        PublicBindClassV2, AdminModeV2, MaintenanceActionV2, BuildSummaryV2, DatabaseSummaryV2, WorkerSummaryV2,
        RetentionSummaryV2, SafeConfigurationSummaryV2, SystemSnapshotV2, RetentionReceiptV2
    )),
    tags((name = "admin-v2", description = "Relay operator console contract"))
)]
pub struct AdminApiDoc;
