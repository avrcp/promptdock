import type { ScenarioFixture } from './scenario-types'
import { healthyScenario } from './healthy'

export const emptyFirstRunScenario: ScenarioFixture = {
  id: 'empty-first-run',
  label: '空状态首次运行',
  description: '无设备、无队列、无问题',
  data: {
    overview: {
      ...healthyScenario.data.overview,
      devices: {
        enabledTotal: 0,
        onlineTotal: 0,
        recentlyOfflineTotal: 0,
        states: { online: 0, offline: 0, disabled: 0, revoked: 0, unknown: 0 },
      },
      queue: { pending: 0, sending: 0, retrying: 0, blocked: 0, failed: 0 },
      gatewayConnections: [],
      currentAlerts: [],
    },
    wechat: {
      ...healthyScenario.data.wechat,
      state: 'disconnected',
      accountHint: null,
      lastErrorCode: null,
    },
    devices: [],
    deviceDetails: {},
    channelEvents: [],
    deliveries: [],
    replies: [],
    inbound: [],
    system: { ...healthyScenario.data.system, currentAlerts: [] },
    alerts: [],
  },
  behavior: {},
}
