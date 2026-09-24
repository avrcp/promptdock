import type { ScenarioFixture } from './scenario-types'
import type { CurrentAlert } from '@/contracts/common'
import {
  daysAgo,
  fixedNow,
  makeChannelEvent,
  makeDelivery,
  makeDevice,
  makeDeviceDetail,
  makeInbound,
  makeReply,
  makeWechatStatus,
  makeWechatSummary,
  MOCK_DEVICE_IDS,
  minutesAgo,
  publicUuidFor,
} from './fixture-factory'

const healthyDevices = [
  makeDevice({
    slug: 'alpha',
    name: '主控工作站',
    state: 'online',
    gatewayState: 'connected',
    scopes: ['outbox.read', 'outbox.ack', 'command.respond'],
    lastSeenMinutesAgo: 1,
    clientVersion: 'mock-client/0.4.2',
    lastRotatedMinutesAgo: 20,
  }),
  makeDevice({
    slug: 'beta',
    name: '移动协作机',
    state: 'online',
    gatewayState: 'connected',
    scopes: ['outbox.read', 'command.respond'],
    lastSeenMinutesAgo: 3,
    clientVersion: 'mock-client/0.4.2',
    lastRotatedMinutesAgo: 45,
  }),
  makeDevice({
    slug: 'gamma',
    name: '备用采集节点',
    state: 'offline',
    gatewayState: 'offline',
    scopes: ['outbox.read'],
    lastSeenMinutesAgo: 90,
    clientVersion: 'mock-client/0.3.9',
    lastRotatedMinutesAgo: null,
  }),
]

const healthyDeviceDetails: Record<string, ReturnType<typeof makeDeviceDetail>> = {}
for (const item of healthyDevices) {
  healthyDeviceDetails[item.id] = makeDeviceDetail(item)
}

const healthyDeliveries: ReturnType<typeof makeDelivery>[] = [
  makeDelivery('d-1', 'accepted', 'run_event', 'device', '主控工作站', 4, 1, null),
  makeDelivery('d-2', 'accepted', 'test', 'admin', 'Admin 控制台', 8, 1, null),
]

const healthyReplies: ReturnType<typeof makeReply>[] = [
  makeReply('r-1', 'accepted', 'list_recent', 'fp:7a2b', 7, null),
]

const healthyInbound: ReturnType<typeof makeInbound>[] = [
  makeInbound('i-1', 'help', 'reply_queued', 1, 12, null, 'session:7a2b'),
  makeInbound('i-2', 'list_recent', 'reply_queued', 1, 30, null, 'session:7a2b'),
]

const healthyEvents: ReturnType<typeof makeChannelEvent>[] = [
  makeChannelEvent('e-1', 'poll', '微信通道轮询正常', 2),
  makeChannelEvent('e-2', 'context', '上下文同步成功', 6),
  makeChannelEvent('e-3', 'test', '发送测试消息已通过 Relay 接管', 10),
]

const healthyAlerts: CurrentAlert[] = []

const healthySystem = {
  schemaVersion: 2 as const,
  generatedAt: fixedNow,
  build: {
    relayVersion: 'mock-relay/0.1.0',
    gitCommit: 'mock-commit',
    buildTime: '2026-08-25T00:00:00Z',
    rustVersion: 'mock-rustc/1.82.0',
    apiVersion: 'mock-api/v1',
    gatewayVersion: 'mock-gateway/0.1.0',
  },
  database: {
    schemaIdentity: 'mock-schema',
    schemaRevision: 1,
    integrityStatus: 'ok' as const,
    walStatus: 'enabled' as const,
    foreignKeysEnabled: true,
    poolHealth: 'healthy' as const,
    sizeBucket: 'lt_10mb' as const,
    lastRetentionPassAt: daysAgo(1),
  },
  workers: [
    { name: 'outbox-worker', state: 'running' as const, lastTickAt: minutesAgo(1), detail: null },
    { name: 'retention-worker', state: 'idle' as const, lastTickAt: daysAgo(1), detail: null },
    { name: 'wechat-monitor', state: 'running' as const, lastTickAt: minutesAgo(2), detail: null },
    { name: 'inbound-worker', state: 'running' as const, lastTickAt: minutesAgo(3), detail: null },
    {
      name: 'gateway-supervisor',
      state: 'running' as const,
      lastTickAt: minutesAgo(1),
      detail: null,
    },
    { name: 'shutdown', state: 'idle' as const, lastTickAt: null, detail: null },
  ],
  retention: {
    acceptedDays: 30,
    deadLetterDays: 30,
    inboundTerminalDays: 30,
    inboundExpiredDays: 7,
    lastRunAt: daysAgo(1),
    lastResult: 'success' as const,
  },
  configuration: {
    wechatEnabled: true,
    publicBindClass: 'loopback' as const,
    adminBindClass: 'loopback' as const,
    adminMode: 'read_only' as const,
    forwardedHttpsObserved: true,
    featureFlags: ['mock:scenario-selector'],
  },
  currentAlerts: healthyAlerts,
}

const healthyOverview = {
  schemaVersion: 2 as const,
  generatedAt: fixedNow,
  relay: {
    health: 'healthy' as const,
    version: 'mock-relay/0.1.0',
    uptimeSeconds: 86_400,
    lastCheckAt: minutesAgo(1),
  },
  wechat: makeWechatSummary('ready', 'mock-account-hint', null, 1),
  devices: {
    enabledTotal: 3,
    onlineTotal: 2,
    recentlyOfflineTotal: 1,
    states: { online: 2, offline: 1, disabled: 0, revoked: 0, unknown: 0 },
  },
  queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
  gatewayConnections: [
    {
      deviceId: MOCK_DEVICE_IDS.alpha,
      deviceName: '主控工作站',
      generation: 7,
      connected: true,
      lastHeartbeatAt: minutesAgo(1),
      clientVersion: 'mock-client/0.4.2',
    },
    {
      deviceId: MOCK_DEVICE_IDS.beta,
      deviceName: '移动协作机',
      generation: 4,
      connected: true,
      lastHeartbeatAt: minutesAgo(3),
      clientVersion: 'mock-client/0.4.2',
    },
    {
      deviceId: MOCK_DEVICE_IDS.gamma,
      deviceName: '备用采集节点',
      generation: 2,
      connected: false,
      lastHeartbeatAt: minutesAgo(90),
      clientVersion: 'mock-client/0.3.9',
    },
  ],
  currentAlerts: healthyAlerts,
}

export const healthyScenario: ScenarioFixture = {
  id: 'healthy',
  label: '正常运行',
  description: 'Relay 正常、微信就绪、队列无堆积',
  data: {
    overview: healthyOverview,
    wechat: makeWechatStatus('ready', 'mock-account-hint', null, 1),
    devices: healthyDevices,
    deviceDetails: healthyDeviceDetails,
    channelEvents: healthyEvents,
    deliveries: healthyDeliveries,
    replies: healthyReplies,
    inbound: healthyInbound,
    system: healthySystem,
    alerts: healthyAlerts,
  },
  behavior: {},
}

void publicUuidFor
