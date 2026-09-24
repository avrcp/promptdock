import type { ScenarioFixture } from './scenario-types'
import { makeCurrentAlert, makeInbound } from './fixture-factory'
import { healthyScenario } from './healthy'

const inbound = [
  makeInbound('i-dl-1', 'list_devices', 'dead_letter', 5, 60, 'INBOUND_EXPIRED', 'session:7a2b'),
  makeInbound('i-dl-2', 'get_detail', 'dead_letter', 4, 120, 'INBOUND_EXPIRED', 'session:91de'),
  makeInbound('i-dl-3', 'list_recent', 'expired', 2, 45, 'GATEWAY_TIMEOUT', 'session:7a2b'),
]
const alerts = [
  makeCurrentAlert('iss-dl-1', 'INBOUND_EXPIRED', 'error', 'inbound', '入站命令进入死信', 60),
]

export const deadLetterScenario: ScenarioFixture = {
  id: 'dead-letter',
  label: '死信条目',
  description: '存在 dead_letter 状态的入站命令',
  data: {
    ...healthyScenario.data,
    inbound,
    overview: {
      ...healthyScenario.data.overview,
      currentAlerts: alerts,
    },
    system: { ...healthyScenario.data.system, currentAlerts: alerts },
    alerts,
  },
  behavior: {},
}
