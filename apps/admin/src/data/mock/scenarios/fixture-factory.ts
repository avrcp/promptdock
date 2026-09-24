import { MOCK_NOW } from '../mock-clock'
import type { DeviceDetail, DeviceListItem, GatewayState, DeviceState } from '@/contracts/device'
import type { ChannelEventItem, WechatAdminStatus } from '@/contracts/wechat'
import type { WechatSummary } from '@/contracts/overview'
import type {
  DeliveryListItem,
  InboundCommandListItem,
  InteractiveReplyListItem,
} from '@/contracts/queue'
import type { CurrentAlert } from '@/contracts/common'

const NOW = MOCK_NOW
const MIN = 60_000
const HOUR = 60 * MIN
const DAY = 24 * HOUR

export const fixedNow = NOW

export function minutesAgo(min: number): number {
  return NOW - min * MIN
}

export function hoursAgo(h: number): number {
  return NOW - h * HOUR
}

export function daysAgo(d: number): number {
  return NOW - d * DAY
}

export function minutesFromNow(min: number): number {
  return NOW + min * MIN
}

export const SYNTHETIC_DEVICE_TOKEN_PREFIX = 'mock-pdv2.not-a-real-secret'

export function makeSyntheticToken(suffix: string): string {
  return `${SYNTHETIC_DEVICE_TOKEN_PREFIX}.${suffix}`
}

export const MOCK_DEVICE_IDS = {
  alpha: 'mock-device-alpha',
  beta: 'mock-device-beta',
  gamma: 'mock-device-gamma',
  delta: 'mock-device-delta',
} as const

const UUID_TAILS: Record<keyof typeof MOCK_DEVICE_IDS, string> = {
  alpha: 'a1b2c3d4',
  beta: 'b2c3d4e5',
  gamma: 'c3d4e5f6',
  delta: 'd4e5f6a7',
}

export function publicUuidFor(slug: keyof typeof MOCK_DEVICE_IDS): string {
  return `mock-uuid-${UUID_TAILS[slug]}-0000-0000-0000-000000000000`
}

export function makeDevice(opts: {
  slug: keyof typeof MOCK_DEVICE_IDS
  name: string
  state: DeviceState
  gatewayState: GatewayState
  scopes: string[]
  lastSeenMinutesAgo: number | null
  clientVersion: string | null
  lastRotatedMinutesAgo: number | null
}): DeviceListItem {
  const lastSeenAt = opts.lastSeenMinutesAgo === null ? null : minutesAgo(opts.lastSeenMinutesAgo)
  return {
    id: MOCK_DEVICE_IDS[opts.slug],
    name: opts.name,
    state: opts.state,
    gatewayState: opts.gatewayState,
    scopes: opts.scopes,
    tokenFormatVersion: 2,
    lastRotatedAt:
      opts.lastRotatedMinutesAgo === null ? null : minutesAgo(opts.lastRotatedMinutesAgo),
    clientVersion: opts.clientVersion,
    lastSeenAt,
    createdAt: daysAgo(30),
    updatedAt: minutesAgo(5),
  }
}

export function makeDeviceDetail(item: DeviceListItem): DeviceDetail {
  return {
    ...item,
    fullDeviceUuid: publicUuidFor(
      (Object.keys(MOCK_DEVICE_IDS) as Array<keyof typeof MOCK_DEVICE_IDS>).find(
        (k) => MOCK_DEVICE_IDS[k] === item.id,
      ) ?? 'alpha',
    ),
    capabilities: ['outbox.read', 'outbox.ack', 'command.respond'],
    lastHeartbeatAt: item.state === 'online' ? minutesAgo(1) : item.lastSeenAt,
    connectedAt: item.state === 'online' ? minutesAgo(3) : null,
    gatewayGeneration: item.state === 'online' ? 1 : null,
  }
}

export function makeChannelEvent(
  id: string,
  kind: ChannelEventItem['kind'],
  message: string,
  minutesBack: number,
): ChannelEventItem {
  return {
    id,
    kind,
    occurredAt: minutesAgo(minutesBack),
    safeMessage: message,
  }
}

export function makeDelivery(
  id: string,
  state: DeliveryListItem['state'],
  kind: DeliveryListItem['kind'],
  origin: DeliveryListItem['origin'],
  originLabel: string,
  minutesBack: number,
  attemptCount: number,
  errorCode: string | null,
): DeliveryListItem {
  return {
    id,
    origin,
    originLabel,
    kind,
    state,
    priority: kind === 'activation' ? 200 : 50,
    attemptCount,
    errorCode,
    createdAt: minutesAgo(minutesBack + 5),
    updatedAt: minutesAgo(minutesBack),
  }
}

export function makeReply(
  id: string,
  state: InteractiveReplyListItem['state'],
  commandRef: string,
  fingerprint: string,
  minutesBack: number,
  errorCode: string | null,
): InteractiveReplyListItem {
  return {
    id,
    commandRef,
    targetFingerprint: fingerprint,
    state,
    occurredAt: minutesAgo(minutesBack),
    errorCode,
  }
}

export function makeInbound(
  id: string,
  command: InboundCommandListItem['command'],
  state: InboundCommandListItem['state'],
  attemptCount: number,
  minutesBack: number,
  errorCode: string | null,
  senderHint: string,
): InboundCommandListItem {
  return {
    id,
    command,
    senderHint,
    state,
    attemptCount,
    errorCode,
    createdAt: minutesAgo(minutesBack),
    updatedAt: minutesAgo(Math.max(0, minutesBack - 1)),
    expiresAt: minutesFromNow(15),
  }
}

export function makeWechatStatus(
  state: WechatAdminStatus['state'],
  accountHint: string | null,
  lastErrorCode: string | null,
  lastPollMinutesAgo: number | null,
): WechatAdminStatus {
  return {
    schemaVersion: 2,
    generatedAt: NOW,
    state,
    accountHint,
    lastPollAt: lastPollMinutesAgo === null ? null : minutesAgo(lastPollMinutesAgo),
    lastContextAt: lastPollMinutesAgo === null ? null : minutesAgo(lastPollMinutesAgo + 1),
    lastProviderAcceptedAt:
      state === 'ready' && lastPollMinutesAgo !== null ? minutesAgo(lastPollMinutesAgo + 2) : null,
    queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
    lastErrorCode,
  }
}

export function makeWechatSummary(
  state: WechatSummary['state'],
  accountHint: string | null,
  lastErrorCode: string | null,
  lastPollMinutesAgo: number | null,
): WechatSummary {
  return {
    state,
    accountHint,
    lastPollAt: lastPollMinutesAgo === null ? null : minutesAgo(lastPollMinutesAgo),
    lastContextAt: lastPollMinutesAgo === null ? null : minutesAgo(lastPollMinutesAgo + 1),
    lastProviderAcceptedAt:
      state === 'ready' && lastPollMinutesAgo !== null ? minutesAgo(lastPollMinutesAgo + 2) : null,
    queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
    lastErrorCode,
  }
}

export function makeCurrentAlert(
  id: string,
  code: string,
  severity: CurrentAlert['severity'],
  component: CurrentAlert['component'],
  message: string,
  observedMinutesAgo: number,
): CurrentAlert {
  return {
    id,
    code,
    severity,
    component,
    message,
    observedAt: minutesAgo(observedMinutesAgo),
  }
}
