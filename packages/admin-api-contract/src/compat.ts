/**
 * Stable consumer names backed exclusively by Hey API generated artifacts.
 * This module contains aliases and derived enum inventories only; it must not
 * define a request or response schema.
 */
import {
  AdminCapability as AdminCapabilityValues,
  AdminDeviceStateV2,
  AdminGatewayStateV2,
  AdminModeV2,
  AlertComponentV2,
  AlertSeverityV2,
  ChannelEventKindV2,
  ComponentHealthV2,
  DatabaseIntegrityV2,
  DatabaseSizeBucketV2,
  DeliveryKindV2,
  DeliveryOriginV2,
  DeliveryStateV2,
  DeviceActionV2,
  DeviceCredentialActionV2,
  InboundCommand,
  InboundStateV2,
  MaintenanceActionV2,
  PublicBindClassV2,
  RelayHealthV2,
  RetentionResultV2,
  ResultNotificationStatusV2,
  ResultPageStateV2,
  SafeWorkerStateV2,
  WalStatusV2,
  WechatActionV2,
  WechatLoginStateV2,
  WechatStateV2,
  WechatTestStateV2,
  type AdminCapability as GeneratedAdminCapability,
  type AdminMetaV2,
  type AdminOverviewV2,
  type CurrentAlertV2,
  type DeliveriesPageV2,
  type DeviceActionReceiptV2,
  type DeviceCredentialReceiptV2,
  type DeviceDetailV2,
  type DeviceListItemV2,
  type DevicesPageV2,
  type InboundCommandsPageV2,
  type InteractiveRepliesPageV2,
  type RetentionReceiptV2,
  type ResultDetailV2,
  type ResultListItemV2,
  type ResultReceiptV2,
  type ResultsPageV2,
  type SystemSnapshotV2,
  type WechatDisconnectReceiptV2,
  type WechatEventsPageV2,
  type WechatLoginV2,
  type WechatStatusV2,
  type WechatTestReceiptV2,
} from "./generated/types.gen";
import {
  zAdminCapability,
  zAdminDeviceStateV2,
  zAdminErrorV2,
  zAdminGatewayStateV2,
  zAdminMetaV2,
  zAdminOverviewV2,
  zChannelEventItemV2,
  zCreateAdminDeviceV2Body,
  zCurrentAlertV2,
  zDeliveriesPageV2,
  zDeliveryKindV2,
  zDeliveryListItemV2,
  zDeliveryOriginV2,
  zDeliveryStateV2,
  zDeviceActionReceiptV2,
  zDeviceCredentialReceiptV2,
  zDeviceDetailV2,
  zDeviceListItemV2,
  zDevicesPageV2,
  zDisconnectAdminWechatV2Body,
  zEmptyMutationRequestV2,
  zInboundCommand,
  zInboundCommandListItemV2,
  zInboundCommandsPageV2,
  zInteractiveRepliesPageV2,
  zInteractiveReplyListItemV2,
  zListAdminDeliveriesV2Query,
  zListAdminDevicesV2Query,
  zListAdminInboundCommandsV2Query,
  zListAdminInteractiveRepliesV2Query,
  zListAdminWechatEventsV2Query,
  zQueueSummaryV2,
  zRetentionReceiptV2,
  zResultDetailV2,
  zResultListItemV2,
  zResultReceiptV2,
  zResultRevokeRequestV2,
  zResultsPageV2,
  zResultsQueryV2,
  zStartAdminWechatLoginV2Body,
  zSystemSnapshotV2,
  zVerifyAdminWechatLoginV2Body,
  zWechatDisconnectReceiptV2,
  zWechatEventsPageV2,
  zWechatLoginStateV2,
  zWechatLoginV2,
  zWechatStatusV2,
  zWechatTestReceiptV2,
} from "./generated/zod.gen";

export type AdminCapability = GeneratedAdminCapability;
export type AdminMeta = AdminMetaV2;
export type AdminOverviewWire = AdminOverviewV2;
export type CurrentAlertWire = CurrentAlertV2;
export type DeviceListItemWire = DeviceListItemV2;
export type DeviceDetailWire = DeviceDetailV2;
export type DevicesPageWire = DevicesPageV2;
export type DeliveriesPageWire = DeliveriesPageV2;
export type InteractiveRepliesPageWire = InteractiveRepliesPageV2;
export type InboundCommandsPageWire = InboundCommandsPageV2;
export type WechatStatusWire = WechatStatusV2;
export type WechatEventsPageWire = WechatEventsPageV2;
export type WechatLoginWire = WechatLoginV2;
export type WechatTestWire = WechatTestReceiptV2;
export type DeviceCredentialWire = DeviceCredentialReceiptV2;
export type DeviceActionWire = DeviceActionReceiptV2;
export type RetentionWire = RetentionReceiptV2;
export type WechatDisconnectWire = WechatDisconnectReceiptV2;
export type SystemWire = SystemSnapshotV2;
export type ResultListItemWire = ResultListItemV2;
export type ResultDetailWire = ResultDetailV2;
export type ResultsPageWire = ResultsPageV2;
export type ResultReceiptWire = ResultReceiptV2;

export const adminCapabilitySchema = zAdminCapability;
export const adminMetaSchema = zAdminMetaV2;
export const wireErrorSchema = zAdminErrorV2;
export const currentAlertWireSchema = zCurrentAlertV2;
export const adminDeviceStateSchema = zAdminDeviceStateV2;
export const adminGatewayStateSchema = zAdminGatewayStateV2;
export const deviceListItemWireSchema = zDeviceListItemV2;
export const deviceDetailWireSchema = zDeviceDetailV2;
export const devicesPageWireSchema = zDevicesPageV2;
export const devicesQueryWireSchema = zListAdminDevicesV2Query;
export const queueSummaryWireSchema = zQueueSummaryV2;
export const adminOverviewWireSchema = zAdminOverviewV2;
export const deliveryKindWireSchema = zDeliveryKindV2;
export const deliveryOriginWireSchema = zDeliveryOriginV2;
export const deliveryStateWireSchema = zDeliveryStateV2;
export const inboundCommandWireSchema = zInboundCommand;
export const deliveryListItemWireSchema = zDeliveryListItemV2;
export const interactiveReplyListItemWireSchema = zInteractiveReplyListItemV2;
export const inboundCommandListItemWireSchema = zInboundCommandListItemV2;
export const deliveriesPageWireSchema = zDeliveriesPageV2;
export const interactiveRepliesPageWireSchema = zInteractiveRepliesPageV2;
export const inboundCommandsPageWireSchema = zInboundCommandsPageV2;
export const deliveriesQueryWireSchema = zListAdminDeliveriesV2Query;
export const interactiveRepliesQueryWireSchema =
  zListAdminInteractiveRepliesV2Query;
export const inboundCommandsQueryWireSchema = zListAdminInboundCommandsV2Query;
export const systemWireSchema = zSystemSnapshotV2;
export const resultListItemWireSchema = zResultListItemV2;
export const resultDetailWireSchema = zResultDetailV2;
export const resultsPageWireSchema = zResultsPageV2;
export const resultsQueryWireSchema = zResultsQueryV2;
export const resultReceiptWireSchema = zResultReceiptV2;
export const resultRevokeRequestWireSchema = zResultRevokeRequestV2;
export const channelEventItemWireSchema = zChannelEventItemV2;
export const wechatStatusWireSchema = zWechatStatusV2;
export const wechatEventsPageWireSchema = zWechatEventsPageV2;
export const wechatEventsQueryWireSchema = zListAdminWechatEventsV2Query;
export const loginStateWireSchema = zWechatLoginStateV2;
export const wechatLoginWireSchema = zWechatLoginV2;
export const wechatTestWireSchema = zWechatTestReceiptV2;
export const deviceCredentialWireSchema = zDeviceCredentialReceiptV2;
export const deviceActionWireSchema = zDeviceActionReceiptV2;
export const retentionWireSchema = zRetentionReceiptV2;
export const wechatDisconnectWireSchema = zWechatDisconnectReceiptV2;
export const createDeviceRequestWireSchema = zCreateAdminDeviceV2Body;
export const emptyMutationRequestWireSchema = zEmptyMutationRequestV2;
export const disconnectWechatRequestWireSchema = zDisconnectAdminWechatV2Body;
export const wechatLoginStartRequestWireSchema = zStartAdminWechatLoginV2Body;
export const wechatLoginVerifyRequestWireSchema = zVerifyAdminWechatLoginV2Body;

const values = <T extends Record<string, string>>(value: T) =>
  Object.values(value) as T[keyof T][];
export const ADMIN_CAPABILITY_VALUES = values(AdminCapabilityValues);
export const ADMIN_DEVICE_STATE_VALUES = values(AdminDeviceStateV2);
export const ADMIN_GATEWAY_STATE_VALUES = values(AdminGatewayStateV2);
export const ADMIN_MODE_VALUES = values(AdminModeV2);
export const ALERT_COMPONENT_VALUES = values(AlertComponentV2);
export const ALERT_SEVERITY_VALUES = values(AlertSeverityV2);
export const CHANNEL_EVENT_KIND_VALUES = values(ChannelEventKindV2);
export const COMPONENT_HEALTH_STATE_VALUES = values(ComponentHealthV2);
export const DATABASE_INTEGRITY_STATE_VALUES = values(DatabaseIntegrityV2);
export const DATABASE_SIZE_BUCKET_VALUES = values(DatabaseSizeBucketV2);
export const DELIVERY_KIND_VALUES = values(DeliveryKindV2);
export const DELIVERY_ORIGIN_VALUES = values(DeliveryOriginV2);
export const DELIVERY_STATE_VALUES = values(DeliveryStateV2);
export const DEVICE_ACTION_VALUES = values(DeviceActionV2);
export const DEVICE_CREDENTIAL_ACTION_VALUES = values(DeviceCredentialActionV2);
export const INBOUND_COMMAND_VALUES = values(InboundCommand);
export const INBOUND_STATE_VALUES = values(InboundStateV2);
export const LOGIN_STATE_VALUES = values(WechatLoginStateV2);
export const MAINTENANCE_ACTION_VALUES = values(MaintenanceActionV2);
export const PUBLIC_BIND_CLASS_VALUES = values(PublicBindClassV2);
export const RELAY_HEALTH_STATE_VALUES = values(RelayHealthV2);
export const RETENTION_RESULT_VALUES = values(RetentionResultV2);
export const RESULT_NOTIFICATION_STATUS_VALUES = values(ResultNotificationStatusV2);
export const RESULT_PAGE_STATE_VALUES = values(ResultPageStateV2);
export const SAFE_WORKER_STATE_VALUES = values(SafeWorkerStateV2);
export const WAL_STATE_VALUES = values(WalStatusV2);
export const WECHAT_ACTION_VALUES = values(WechatActionV2);
export const WECHAT_STATE_VALUES = values(WechatStateV2);
export const WECHAT_TEST_STATE_VALUES = values(WechatTestStateV2);
