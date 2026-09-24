import type { ScenarioFixture } from './scenario-types'
import { SCENARIO_DESCRIPTORS, type ScenarioId } from './scenario-ids'

import { healthyScenario } from './healthy'
import { wechatDisconnectedScenario } from './wechat-disconnected'
import { wechatLoginScenario } from './wechat-login'
import { wechatNeedsActivationScenario } from './wechat-needs-activation'
import { gatewayPartialOfflineScenario } from './gateway-partial-offline'
import { queueBacklogScenario } from './queue-backlog'
import { deadLetterScenario } from './dead-letter'
import { databaseDegradedScenario } from './database-degraded'
import { relayUnavailableScenario } from './relay-unavailable'
import { emptyFirstRunScenario } from './empty-first-run'
import { slowNetworkScenario } from './slow-network'
import { partialFailureScenario } from './partial-failure'

const SCENARIOS: Record<ScenarioId, ScenarioFixture> = {
  healthy: healthyScenario,
  'wechat-disconnected': wechatDisconnectedScenario,
  'wechat-login': wechatLoginScenario,
  'wechat-needs-activation': wechatNeedsActivationScenario,
  'gateway-partial-offline': gatewayPartialOfflineScenario,
  'queue-backlog': queueBacklogScenario,
  'dead-letter': deadLetterScenario,
  'database-degraded': databaseDegradedScenario,
  'relay-unavailable': relayUnavailableScenario,
  'empty-first-run': emptyFirstRunScenario,
  'slow-network': slowNetworkScenario,
  'partial-failure': partialFailureScenario,
}

export function getAllScenarios(): readonly ScenarioFixture[] {
  return SCENARIO_DESCRIPTORS.map((d) => SCENARIOS[d.id])
}

export function getScenarioById(id: ScenarioId): ScenarioFixture {
  return SCENARIOS[id]
}

export { SCENARIO_DESCRIPTORS, type ScenarioId } from './scenario-ids'
export { isScenarioId } from './scenario-ids'
export type {
  ScenarioFixture,
  ScenarioBehavior,
  ScenarioSlice,
  ScenarioViewFailure,
} from './scenario-types'
