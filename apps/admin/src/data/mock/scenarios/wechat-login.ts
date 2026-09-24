import type { ScenarioFixture } from './scenario-types'
import { makeWechatStatus, makeWechatSummary } from './fixture-factory'
import { healthyScenario } from './healthy'

const wechat = makeWechatStatus('disconnected', null, null, null)
const wechatSummary = makeWechatSummary('disconnected', null, null, null)

export const wechatLoginScenario: ScenarioFixture = {
  id: 'wechat-login',
  label: '微信等待登录授权',
  description: '展示通道断开时受控登录入口的前置状态',
  data: {
    ...healthyScenario.data,
    wechat,
    overview: {
      ...healthyScenario.data.overview,
      wechat: wechatSummary,
    },
  },
  behavior: {},
}
