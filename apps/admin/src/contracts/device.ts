import { z } from 'zod'

export const deviceStateSchema = z.enum(['online', 'offline', 'disabled', 'revoked', 'unknown'])
export type DeviceState = z.infer<typeof deviceStateSchema>

/**
 * The UI can defensively render an `unknown` state returned by an older or
 * degraded Relay, but the v2 list endpoint does not accept it as a filter.
 */
export const deviceStateFilterSchema = z.enum(['online', 'offline', 'disabled', 'revoked'])
export type DeviceStateFilter = z.infer<typeof deviceStateFilterSchema>

export const gatewayStateSchema = z.enum([
  'connected',
  'reconnecting',
  'offline',
  'superseded',
  'not_enabled',
])
export type GatewayState = z.infer<typeof gatewayStateSchema>

export const deviceListItemSchema = z
  .object({
    id: z.string().min(1),
    name: z.string().min(1),
    state: deviceStateSchema,
    gatewayState: gatewayStateSchema,
    scopes: z.array(z.string().min(1)),
    tokenFormatVersion: z.number().int().positive(),
    lastRotatedAt: z.number().int().nonnegative().nullable(),
    clientVersion: z.string().nullable(),
    lastSeenAt: z.number().int().nonnegative().nullable(),
    createdAt: z.number().int().nonnegative(),
    updatedAt: z.number().int().nonnegative(),
  })
  .strict()
export type DeviceListItem = z.infer<typeof deviceListItemSchema>

export const deviceListQuerySchema = z
  .object({
    search: z.string().max(64).optional(),
    state: deviceStateFilterSchema.optional(),
    limit: z.number().int().positive().max(100).default(50),
    cursor: z.string().nullable().optional(),
  })
  .strict()
export type DeviceListQuery = z.infer<typeof deviceListQuerySchema>

export const deviceDetailSchema = deviceListItemSchema.extend({
  fullDeviceUuid: z.string().min(1),
  capabilities: z.array(z.string().min(1)),
  lastHeartbeatAt: z.number().int().nonnegative().nullable(),
  connectedAt: z.number().int().nonnegative().nullable(),
  gatewayGeneration: z.number().int().nonnegative().nullable(),
})
export type DeviceDetail = z.infer<typeof deviceDetailSchema>

export const DEVICE_SCOPE_VALUES = [
  'notify:write',
  'notify:read_own',
  'channel:read',
  'channel:manage',
  'gateway:connect',
  'job:query',
  'job:control',
] as const
export const deviceScopeSchema = z.enum(DEVICE_SCOPE_VALUES)
export type DeviceScope = z.infer<typeof deviceScopeSchema>

export const createDeviceInputSchema = z
  .object({
    name: z.string().min(1).max(64),
    scopes: z.array(deviceScopeSchema).min(1).max(DEVICE_SCOPE_VALUES.length),
  })
  .strict()
export type CreateDeviceInput = z.infer<typeof createDeviceInputSchema>

export const rotateDeviceInputSchema = z
  .object({
    deviceId: z.string().min(1),
  })
  .strict()
export type RotateDeviceInput = z.infer<typeof rotateDeviceInputSchema>

export const setDeviceEnabledInputSchema = z
  .object({
    deviceId: z.string().min(1),
    enabled: z.boolean(),
  })
  .strict()
export type SetDeviceEnabledInput = z.infer<typeof setDeviceEnabledInputSchema>

export const revokeDeviceInputSchema = z
  .object({
    deviceId: z.string().min(1),
  })
  .strict()
export type RevokeDeviceInput = z.infer<typeof revokeDeviceInputSchema>

export const deviceCredentialReceiptSchema = z
  .object({
    receiptId: z.string().min(1),
    action: z.enum(['create', 'rotate']),
    deviceId: z.string().min(1),
    oneTimeToken: z.string().min(1),
    issuedAt: z.number().int().nonnegative(),
  })
  .strict()
export type DeviceCredentialReceipt = z.infer<typeof deviceCredentialReceiptSchema>
