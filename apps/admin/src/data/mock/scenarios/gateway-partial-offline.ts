import type { ScenarioFixture } from './scenario-types'
import {
  daysAgo,
  fixedNow,
  makeDevice,
  makeDeviceDetail,
  makeCurrentAlert,
  MOCK_DEVICE_IDS,
  minutesAgo,
} from './fixture-factory'
import { healthyScenario } from './healthy'

const devices = [
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
    state: 'offline',
    gatewayState: 'reconnecting',
    scopes: ['outbox.read', 'command.respond'],
    lastSeenMinutesAgo: 5,
    clientVersion: 'mock-client/0.4.2',
    lastRotatedMinutesAgo: 45,
  }),
  makeDevice({
    slug: 'gamma',
    name: '备用采集节点',
    state: 'offline',
    gatewayState: 'offline',
    scopes: ['outbox.read'],
    lastSeenMinutesAgo: 45,
    clientVersion: 'mock-client/0.3.9',
    lastRotatedMinutesAgo: null,
  }),
]

const deviceDetails: Record<string, ReturnType<typeof makeDeviceDetail>> = {}
for (const item of devices) {
  deviceDetails[item.id] = makeDeviceDetail(item)
}
const alerts = [
  makeCurrentAlert('iss-gw-1', 'GATEWAY_TIMEOUT', 'warning', 'gateway', 'Gateway 连接超时', 10),
]

export const gatewayPartialOfflineScenario: ScenarioFixture = {
  id: 'gateway-partial-offline',
  label: 'Gateway 部分离线',
  description: '部分设备 Gateway 连接断开',
  data: {
    ...healthyScenario.data,
    devices,
    deviceDetails,
    overview: {
      ...healthyScenario.data.overview,
      devices: {
        enabledTotal: 3,
        onlineTotal: 1,
        recentlyOfflineTotal: 2,
        states: { online: 1, offline: 2, disabled: 0, revoked: 0, unknown: 0 },
      },
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
          connected: false,
          lastHeartbeatAt: minutesAgo(5),
          clientVersion: 'mock-client/0.4.2',
        },
        {
          deviceId: MOCK_DEVICE_IDS.gamma,
          deviceName: '备用采集节点',
          generation: 2,
          connected: false,
          lastHeartbeatAt: minutesAgo(45),
          clientVersion: 'mock-client/0.3.9',
        },
      ],
      currentAlerts: alerts,
    },
    system: {
      ...healthyScenario.data.system,
      generatedAt: fixedNow,
      workers: healthyScenario.data.system.workers.map((w) =>
        w.name === 'gateway-supervisor'
          ? { ...w, state: 'degraded' as const, detail: '1/3 connected' }
          : w,
      ),
      currentAlerts: alerts,
    },
    alerts,
  },
  behavior: {},
}

void daysAgo
