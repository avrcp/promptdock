import { z } from 'zod'

import { currentAlertSchema } from './common'

export const relaySummarySchema = z
  .object({
    health: z.enum(['healthy', 'degraded', 'failed', 'unknown']),
    version: z.string().min(1),
    uptimeSeconds: z.number().int().nonnegative(),
    lastCheckAt: z.number().int().nonnegative(),
  })
  .strict()
export type RelaySummary = z.infer<typeof relaySummarySchema>

export const deviceStateCountSchema = z
  .object({
    online: z.number().int().nonnegative(),
    offline: z.number().int().nonnegative(),
    disabled: z.number().int().nonnegative(),
    revoked: z.number().int().nonnegative(),
    unknown: z.number().int().nonnegative(),
  })
  .strict()
export type DeviceStateCount = z.infer<typeof deviceStateCountSchema>

export const deviceSummarySchema = z
  .object({
    enabledTotal: z.number().int().nonnegative(),
    onlineTotal: z.number().int().nonnegative(),
    recentlyOfflineTotal: z.number().int().nonnegative(),
    states: deviceStateCountSchema,
  })
  .strict()
export type DeviceSummary = z.infer<typeof deviceSummarySchema>

export const queueSummarySchema = z
  .object({
    pending: z.number().int().nonnegative(),
    sending: z.number().int().nonnegative(),
    retrying: z.number().int().nonnegative(),
    blocked: z.number().int().nonnegative(),
    failed: z.number().int().nonnegative(),
  })
  .strict()
export type QueueSummary = z.infer<typeof queueSummarySchema>

export const wechatSummarySchema = z
  .object({
    state: z.enum([
      'disconnected',
      'connected_awaiting_activation',
      'ready',
      'degraded',
      'needs_reconnect',
      'credentials_unreadable',
    ]),
    accountHint: z.string().nullable(),
    lastPollAt: z.number().int().nonnegative().nullable(),
    lastContextAt: z.number().int().nonnegative().nullable(),
    lastProviderAcceptedAt: z.number().int().nonnegative().nullable(),
    queue: queueSummarySchema,
    lastErrorCode: z.string().nullable(),
  })
  .strict()
export type WechatSummary = z.infer<typeof wechatSummarySchema>

export const gatewayConnectionSchema = z
  .object({
    deviceId: z.string().min(1),
    deviceName: z.string().min(1),
    generation: z.number().int().nonnegative(),
    connected: z.boolean(),
    lastHeartbeatAt: z.number().int().nonnegative().nullable(),
    clientVersion: z.string().nullable(),
  })
  .strict()
export type GatewayConnection = z.infer<typeof gatewayConnectionSchema>

export const adminOverviewSchema = z
  .object({
    schemaVersion: z.literal(2),
    generatedAt: z.number().int().nonnegative(),
    relay: relaySummarySchema,
    wechat: wechatSummarySchema,
    devices: deviceSummarySchema,
    queue: queueSummarySchema,
    gatewayConnections: z.array(gatewayConnectionSchema),
    currentAlerts: z.array(currentAlertSchema),
  })
  .strict()
export type AdminOverview = z.infer<typeof adminOverviewSchema>
