import type { ScenarioFixture } from './scenario-types'
import { healthyScenario } from './healthy'

export const partialFailureScenario: ScenarioFixture = {
  id: 'partial-failure',
  label: '部分失败',
  description: 'Overview 多个 section 同时出现 section-level 错误',
  data: healthyScenario.data,
  behavior: {
    failureProfile: {
      wechat: 'failed',
      queue: 'degraded',
    },
    failingSections: ['wechat', 'queue'],
  },
}
