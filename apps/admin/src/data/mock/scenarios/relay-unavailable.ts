import type { ScenarioFixture } from './scenario-types'
import { healthyScenario } from './healthy'

export const relayUnavailableScenario: ScenarioFixture = {
  id: 'relay-unavailable',
  label: 'Relay 不可用',
  description: 'Overview 顶层获取失败（section-level fallback）',
  data: {
    ...healthyScenario.data,
  },
  behavior: {
    failureProfile: {
      overview: 'failed',
    },
    failingSections: ['overview'],
  },
}
