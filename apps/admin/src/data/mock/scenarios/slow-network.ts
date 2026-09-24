import type { ScenarioFixture } from './scenario-types'
import { healthyScenario } from './healthy'

export const slowNetworkScenario: ScenarioFixture = {
  id: 'slow-network',
  label: '慢网络',
  description: '所有读取走 1.5–2.5s 慢延迟',
  data: healthyScenario.data,
  behavior: { delayProfile: 'slow' },
}
