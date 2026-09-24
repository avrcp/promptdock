import { describe, it, expect } from 'vitest'

import { adminOverviewSchema } from '@/contracts/overview'
import {
  createDeviceInputSchema,
  DEVICE_SCOPE_VALUES,
  deviceListItemSchema,
  deviceListQuerySchema,
} from '@/contracts/device'
import {
  deliveryListItemSchema,
  deliveryListQuerySchema,
  inboundCommandListItemSchema,
  inboundCommandListQuerySchema,
  interactiveReplyListQuerySchema,
} from '@/contracts/queue'
import { systemSnapshotSchema } from '@/contracts/system'
import {
  channelEventQuerySchema,
  wechatAdminStatusSchema,
  wechatLoginSessionSchema,
} from '@/contracts/wechat'
import { currentAlertSchema } from '@/contracts/common'
import { adminErrorSchema } from '@/contracts/error'

describe('zod contracts (strict)', () => {
  it('createDevice accepts only Relay-owned scopes and request fields', () => {
    expect(
      createDeviceInputSchema.parse({ name: 'gateway', scopes: [...DEVICE_SCOPE_VALUES] }),
    ).toEqual({ name: 'gateway', scopes: [...DEVICE_SCOPE_VALUES] })
    expect(() => createDeviceInputSchema.parse({ name: 'bad', scopes: ['outbox.read'] })).toThrow()
    expect(() =>
      createDeviceInputSchema.parse({
        name: 'bad',
        scopes: ['gateway:connect'],
        clientVersion: 'silently-dropped-field',
      }),
    ).toThrow()
  })

  it('adminOverview rejects schemaVersion != 2', () => {
    const base = {
      schemaVersion: 1,
      generatedAt: 0,
      relay: { health: 'healthy', version: 'v', uptimeSeconds: 0, lastCheckAt: 0 },
      wechat: {
        state: 'ready',
        accountHint: null,
        lastPollAt: null,
        lastContextAt: null,
        lastProviderAcceptedAt: null,
        queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
        lastErrorCode: null,
      },
      devices: {
        enabledTotal: 0,
        onlineTotal: 0,
        recentlyOfflineTotal: 0,
        states: { online: 0, offline: 0, disabled: 0, revoked: 0, unknown: 0 },
      },
      queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
      gatewayConnections: [],
      currentAlerts: [],
    }
    expect(() => adminOverviewSchema.parse(base)).toThrow()
  })

  it('adminOverview rejects unknown extra fields in strict mode', () => {
    const base = {
      schemaVersion: 2,
      generatedAt: 0,
      relay: { health: 'healthy', version: 'v', uptimeSeconds: 0, lastCheckAt: 0 },
      wechat: {
        state: 'ready',
        accountHint: null,
        lastPollAt: null,
        lastContextAt: null,
        lastProviderAcceptedAt: null,
        queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
        lastErrorCode: null,
      },
      devices: {
        enabledTotal: 0,
        onlineTotal: 0,
        recentlyOfflineTotal: 0,
        states: { online: 0, offline: 0, disabled: 0, revoked: 0, unknown: 0 },
      },
      queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
      gatewayConnections: [],
      currentAlerts: [],
    }
    const withExtra = { ...base, mystery: 'value' } as unknown as Parameters<
      typeof adminOverviewSchema.parse
    >[0]
    expect(() => adminOverviewSchema.parse(withExtra)).toThrow()
  })

  it('wechatAdminStatus rejects unsupported state value', () => {
    const bad = {
      schemaVersion: 2,
      generatedAt: 0,
      state: 'not-a-state',
      accountHint: null,
      lastPollAt: null,
      lastContextAt: null,
      lastProviderAcceptedAt: null,
      queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
      lastErrorCode: null,
    }
    expect(() => wechatAdminStatusSchema.parse(bad)).toThrow()
  })

  it('deliveryListItem rejects bad origin', () => {
    const bad = {
      id: 'x',
      origin: 'other',
      originLabel: 'x',
      kind: 'test',
      state: 'queued',
      priority: 1,
      attemptCount: 0,
      errorCode: null,
      createdAt: 0,
      updatedAt: 0,
    }
    expect(() => deliveryListItemSchema.parse(bad)).toThrow()
  })

  it('delivery priority supports Relay values through 255 without clamping', () => {
    const base = {
      id: 'x',
      origin: 'system',
      originLabel: 'system',
      kind: 'test',
      state: 'queued',
      priority: 255,
      attemptCount: 0,
      errorCode: null,
      createdAt: 0,
      updatedAt: 0,
    }
    expect(() => deliveryListItemSchema.parse(base)).not.toThrow()
    expect(() => deliveryListItemSchema.parse({ ...base, priority: 256 })).toThrow()
  })

  it('inboundCommandListItem rejects unknown command', () => {
    const bad = {
      id: 'x',
      command: 'fake-command',
      senderHint: 's',
      state: 'received',
      attemptCount: 0,
      errorCode: null,
      createdAt: 0,
      expiresAt: 0,
      updatedAt: 0,
    }
    expect(() => inboundCommandListItemSchema.parse(bad)).toThrow()
  })

  it('inboundCommandListItem rejects the UI-only completed state', () => {
    const base = {
      id: 'x',
      command: 'help',
      senderHint: 's',
      state: 'completed',
      attemptCount: 0,
      errorCode: null,
      createdAt: 0,
      expiresAt: 0,
      updatedAt: 0,
    }
    expect(() => inboundCommandListItemSchema.parse(base)).toThrow()
  })

  it('deviceListItem rejects invalid state', () => {
    const bad = {
      id: 'x',
      name: 'x',
      state: 'broken',
      gatewayState: 'connected',
      scopes: [],
      tokenFormatVersion: 2,
      lastRotatedAt: null,
      clientVersion: null,
      lastSeenAt: null,
      createdAt: 0,
      updatedAt: 0,
    }
    expect(() => deviceListItemSchema.parse(bad)).toThrow()
  })

  it('keeps unknown as a defensive response state but rejects it as a Relay list filter', () => {
    expect(
      deviceListItemSchema.parse({
        id: 'x',
        name: 'x',
        state: 'unknown',
        gatewayState: 'offline',
        scopes: [],
        tokenFormatVersion: 2,
        lastRotatedAt: null,
        clientVersion: null,
        lastSeenAt: null,
        createdAt: 0,
        updatedAt: 0,
      }).state,
    ).toBe('unknown')
    expect(() => deviceListQuerySchema.parse({ state: 'unknown' })).toThrow()
  })

  it('caps every public page query at the Relay maximum of 100', () => {
    const pageQueries = [
      deviceListQuerySchema,
      channelEventQuerySchema,
      deliveryListQuerySchema,
      interactiveReplyListQuerySchema,
      inboundCommandListQuerySchema,
    ]
    for (const schema of pageQueries) {
      expect(() => schema.parse({ limit: 100 })).not.toThrow()
      expect(() => schema.parse({ limit: 101 })).toThrow()
    }
  })

  it('systemSnapshot rejects unsupported size bucket', () => {
    const bad = {
      schemaVersion: 2,
      generatedAt: 0,
      build: {
        relayVersion: 'v',
        gitCommit: null,
        buildTime: null,
        rustVersion: null,
        apiVersion: 'v',
        gatewayVersion: 'v',
      },
      database: {
        schemaIdentity: 'v',
        schemaRevision: 0,
        integrityStatus: 'ok',
        walStatus: 'enabled',
        foreignKeysEnabled: true,
        poolHealth: 'healthy',
        sizeBucket: 'unknown',
        lastRetentionPassAt: null,
      },
      workers: [],
      retention: {
        acceptedDays: 0,
        deadLetterDays: 0,
        inboundTerminalDays: 0,
        inboundExpiredDays: 0,
        lastRunAt: null,
        lastResult: 'success',
      },
      configuration: {
        wechatEnabled: true,
        publicBindClass: 'loopback',
        adminBindClass: 'loopback',
        adminMode: 'read_only',
        forwardedHttpsObserved: true,
        featureFlags: [],
      },
      currentAlerts: [],
    }
    expect(() => systemSnapshotSchema.parse(bad)).toThrow()
  })

  it('currentAlert rejects legacy incident-history fields', () => {
    const bad = {
      id: 'x',
      code: 'X',
      severity: 'info',
      component: 'relay',
      message: 'm',
      observedAt: 0,
      occurrenceCount: 1,
    }
    expect(() => currentAlertSchema.parse(bad)).toThrow()
  })

  it('wechat login snapshot rejects synthetic QR fields and omits unavailable sensitive fields', () => {
    const base = {
      loginId: 'login-1',
      state: 'waiting_scan',
      expiresAt: 0,
      canSubmitVerifyCode: false,
    }
    expect(() => wechatLoginSessionSchema.parse(base)).not.toThrow()
    expect(() =>
      wechatLoginSessionSchema.parse({ ...base, syntheticQrPayload: 'mock://' }),
    ).toThrow()
  })

  it('adminError rejects unknown code', () => {
    expect(() =>
      adminErrorSchema.parse({ code: 'NOPE', message: 'm', retryable: false, requestId: null }),
    ).toThrow()
  })

  it('adminError accepts every documented code', () => {
    const codes = [
      'RELAY_UNAVAILABLE',
      'WECHAT_RECONNECT_REQUIRED',
      'DATABASE_DEGRADED',
      'GATEWAY_TIMEOUT',
      'OUTBOX_BACKLOG',
      'RATE_LIMITED',
      'DEVICE_NOT_FOUND',
      'DEVICE_REVOKED',
      'INVALID_INPUT',
      'INTERNAL',
    ] as const
    for (const code of codes) {
      expect(() =>
        adminErrorSchema.parse({ code, message: 'm', retryable: false, requestId: null }),
      ).not.toThrow()
    }
  })
})
