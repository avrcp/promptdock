import type { ScenarioFixture } from './scenario-types'
import { makeCurrentAlert, makeWechatStatus, makeWechatSummary } from './fixture-factory'
import { healthyScenario } from './healthy'

const wechat = makeWechatStatus(
  'needs_reconnect',
  'mock-account-hint',
  'WECHAT_RECONNECT_REQUIRED',
  8,
)
const wechatSummary = makeWechatSummary(
  'needs_reconnect',
  'mock-account-hint',
  'WECHAT_RECONNECT_REQUIRED',
  8,
)
const alerts = [
  makeCurrentAlert(
    'iss-wx-need',
    'WECHAT_RECONNECT_REQUIRED',
    'warning',
    'wechat',
    '微信需要重新连接',
    60,
  ),
]

export const wechatNeedsActivationScenario: ScenarioFixture = {
  id: 'wechat-needs-activation',
  label: '微信需要重新连接',
  description: '凭据需要通过受控登录流程重新连接',
  data: {
    ...healthyScenario.data,
    wechat,
    overview: {
      ...healthyScenario.data.overview,
      wechat: wechatSummary,
      currentAlerts: alerts,
    },
    system: { ...healthyScenario.data.system, currentAlerts: alerts },
    alerts,
  },
  behavior: {},
}
