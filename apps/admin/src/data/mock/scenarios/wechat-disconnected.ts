import type { ScenarioFixture } from './scenario-types'
import { makeCurrentAlert, makeWechatStatus, makeWechatSummary } from './fixture-factory'
import { healthyScenario } from './healthy'

const wechat = makeWechatStatus('disconnected', null, 'WECHAT_RECONNECT_REQUIRED', null)
const wechatSummary = makeWechatSummary('disconnected', null, 'WECHAT_RECONNECT_REQUIRED', null)
const alerts = [
  makeCurrentAlert(
    'iss-wx-disc',
    'WECHAT_RECONNECT_REQUIRED',
    'warning',
    'wechat',
    '微信通道未连接',
    30,
  ),
]

export const wechatDisconnectedScenario: ScenarioFixture = {
  id: 'wechat-disconnected',
  label: '微信未连接',
  description: 'WeChat 通道未登录，通知会保留在队列中',
  data: {
    ...healthyScenario.data,
    wechat,
    overview: {
      ...healthyScenario.data.overview,
      wechat: wechatSummary,
      queue: { pending: 6, sending: 0, retrying: 0, blocked: 0, failed: 0 },
      currentAlerts: alerts,
    },
    system: { ...healthyScenario.data.system, currentAlerts: alerts },
    alerts,
  },
  behavior: {},
}
