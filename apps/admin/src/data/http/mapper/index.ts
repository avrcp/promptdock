import { pageSchema, currentAlertSchema, type CurrentAlert, type Page } from '@/contracts/common'
import {
  deviceDetailSchema,
  deviceListItemSchema,
  type DeviceDetail,
  type DeviceListItem,
} from '@/contracts/device'
import { adminOverviewSchema, type AdminOverview } from '@/contracts/overview'
import {
  deliveryListItemSchema,
  inboundCommandListItemSchema,
  interactiveReplyListItemSchema,
  type DeliveryListItem,
  type InboundCommandListItem,
  type InteractiveReplyListItem,
} from '@/contracts/queue'
import { systemSnapshotSchema, type SystemSnapshot } from '@/contracts/system'
import {
  channelEventItemSchema,
  wechatAdminStatusSchema,
  wechatLoginSessionSchema,
  type ChannelEventItem,
  type WechatAdminStatus,
  type WechatLoginSession,
} from '@/contracts/wechat'
import {
  adminOverviewWireSchema,
  deviceDetailWireSchema,
  devicesPageWireSchema,
  deliveriesPageWireSchema,
  inboundCommandsPageWireSchema,
  interactiveRepliesPageWireSchema,
  systemWireSchema,
  wechatEventsPageWireSchema,
  wechatLoginWireSchema,
  wechatStatusWireSchema,
  type CurrentAlertWire,
  type DeviceDetailWire,
  type DeviceListItemWire,
} from '@promptdock/relay-admin-api-generated'

export function mapOverview(value: unknown): AdminOverview {
  const parsed = adminOverviewWireSchema.parse(value)
  return adminOverviewSchema.parse({
    ...parsed,
    wechat: normalizeWechatSummary(parsed.wechat),
    gatewayConnections: parsed.gatewayConnections.map((connection) => ({
      ...connection,
      clientVersion: connection.clientVersion ?? null,
      lastHeartbeatAt: connection.lastHeartbeatAt ?? null,
    })),
    currentAlerts: parsed.currentAlerts.map(mapCurrentAlert),
  })
}

export function mapDevicesPage(value: unknown): Page<DeviceListItem> {
  const parsed = devicesPageWireSchema.parse(value)
  return pageSchema(deviceListItemSchema).parse({
    ...normalizePage(parsed),
    items: parsed.items.map(mapDeviceListItem),
  })
}

export function mapDeviceDetail(value: unknown): DeviceDetail {
  return mapDeviceDetailValue(deviceDetailWireSchema.parse(value))
}

export function mapWechatStatus(value: unknown): WechatAdminStatus {
  const parsed = wechatStatusWireSchema.parse(value)
  return wechatAdminStatusSchema.parse(normalizeWechatSummary(parsed))
}

export function mapWechatEventsPage(value: unknown): Page<ChannelEventItem> {
  const parsed = wechatEventsPageWireSchema.parse(value)
  return pageSchema(channelEventItemSchema).parse(normalizePage(parsed))
}

export function mapDeliveriesPage(value: unknown): Page<DeliveryListItem> {
  const parsed = deliveriesPageWireSchema.parse(value)
  return pageSchema(deliveryListItemSchema).parse({
    ...normalizePage(parsed),
    items: parsed.items.map((item) => ({ ...item, errorCode: item.errorCode ?? null })),
  })
}

export function mapInteractiveRepliesPage(value: unknown): Page<InteractiveReplyListItem> {
  const parsed = interactiveRepliesPageWireSchema.parse(value)
  return pageSchema(interactiveReplyListItemSchema).parse({
    ...normalizePage(parsed),
    items: parsed.items.map((item) => ({ ...item, errorCode: item.errorCode ?? null })),
  })
}

export function mapInboundCommandsPage(value: unknown): Page<InboundCommandListItem> {
  const parsed = inboundCommandsPageWireSchema.parse(value)
  return pageSchema(inboundCommandListItemSchema).parse({
    ...normalizePage(parsed),
    items: parsed.items.map((item) => ({ ...item, errorCode: item.errorCode ?? null })),
  })
}

export function mapSystem(value: unknown): SystemSnapshot {
  const parsed = systemWireSchema.parse(value)
  return systemSnapshotSchema.parse({
    ...parsed,
    build: {
      ...parsed.build,
      buildTime: parsed.build.buildTime ?? null,
      gitCommit: parsed.build.gitCommit ?? null,
      rustVersion: parsed.build.rustVersion ?? null,
    },
    database: {
      ...parsed.database,
      lastRetentionPassAt: parsed.database.lastRetentionPassAt ?? null,
    },
    workers: parsed.workers.map((worker) => ({
      ...worker,
      detail: worker.detail ?? null,
      lastTickAt: worker.lastTickAt ?? null,
    })),
    retention: {
      ...parsed.retention,
      lastRunAt: parsed.retention.lastRunAt ?? null,
    },
    currentAlerts: parsed.currentAlerts.map(mapCurrentAlert),
  })
}

export function mapWechatLogin(value: unknown): WechatLoginSession {
  const parsed = wechatLoginWireSchema.parse(value)
  return wechatLoginSessionSchema.parse({
    loginId: parsed.loginId,
    state: parsed.state,
    expiresAt: parsed.expiresAt,
    canSubmitVerifyCode: parsed.canSubmitVerifyCode,
    ...(parsed.qrContent == null ? {} : { qrContent: parsed.qrContent }),
    ...(parsed.errorCode == null ? {} : { errorCode: parsed.errorCode }),
  })
}

function normalizePage<T>(value: {
  items: T[]
  nextCursor?: string | null
  total?: number | null
  generatedAt: number
}): Page<T> {
  return {
    items: value.items,
    nextCursor: value.nextCursor ?? null,
    total: value.total ?? null,
    generatedAt: value.generatedAt,
  }
}

function mapDeviceListItem(value: DeviceListItemWire): DeviceListItem {
  return deviceListItemSchema.parse({
    ...value,
    clientVersion: value.clientVersion ?? null,
    lastRotatedAt: value.lastRotatedAt ?? null,
    lastSeenAt: value.lastSeenAt ?? null,
  })
}

function mapDeviceDetailValue(value: DeviceDetailWire): DeviceDetail {
  return deviceDetailSchema.parse({
    ...value,
    clientVersion: value.clientVersion ?? null,
    connectedAt: value.connectedAt ?? null,
    gatewayGeneration: value.gatewayGeneration ?? null,
    lastHeartbeatAt: value.lastHeartbeatAt ?? null,
    lastRotatedAt: value.lastRotatedAt ?? null,
    lastSeenAt: value.lastSeenAt ?? null,
    capabilities: value.capabilities.map((capability) => capability),
  })
}

function normalizeWechatSummary<
  T extends {
    accountHint?: string | null
    lastContextAt?: number | null
    lastErrorCode?: string | null
    lastPollAt?: number | null
    lastProviderAcceptedAt?: number | null
  },
>(
  value: T,
): T & {
  accountHint: string | null
  lastContextAt: number | null
  lastErrorCode: string | null
  lastPollAt: number | null
  lastProviderAcceptedAt: number | null
} {
  return {
    ...value,
    accountHint: value.accountHint ?? null,
    lastContextAt: value.lastContextAt ?? null,
    lastErrorCode: value.lastErrorCode ?? null,
    lastPollAt: value.lastPollAt ?? null,
    lastProviderAcceptedAt: value.lastProviderAcceptedAt ?? null,
  }
}

function mapCurrentAlert(value: CurrentAlertWire): CurrentAlert {
  return currentAlertSchema.parse(value)
}
