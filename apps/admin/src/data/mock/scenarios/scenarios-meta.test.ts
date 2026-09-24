import { describe, it, expect } from 'vitest'

import { SCENARIO_DESCRIPTORS, SCENARIO_IDS, isScenarioId } from './scenario-ids'
import { getAllScenarios, getScenarioById } from './index'
import { healthyScenario } from './healthy'

describe('scenario ids', () => {
  it('lists the 12 fixed ids in the documented order', () => {
    expect(SCENARIO_IDS).toEqual([
      'healthy',
      'wechat-disconnected',
      'wechat-login',
      'wechat-needs-activation',
      'gateway-partial-offline',
      'queue-backlog',
      'dead-letter',
      'database-degraded',
      'relay-unavailable',
      'empty-first-run',
      'slow-network',
      'partial-failure',
    ])
  })

  it('isScenarioId accepts only known ids', () => {
    expect(isScenarioId('healthy')).toBe(true)
    expect(isScenarioId('not-real')).toBe(false)
  })

  it('SCENARIO_DESCRIPTORS covers every id', () => {
    expect(SCENARIO_DESCRIPTORS).toHaveLength(12)
    for (const id of SCENARIO_IDS) {
      expect(SCENARIO_DESCRIPTORS.find((d) => d.id === id)).toBeDefined()
    }
  })
})

describe('scenario lookup', () => {
  it('getScenarioById returns the matching fixture', () => {
    expect(getScenarioById('healthy')).toBe(healthyScenario)
  })

  it('getAllScenarios returns 12 fixtures', () => {
    const all = getAllScenarios()
    expect(all).toHaveLength(12)
    const ids = all.map((s) => s.id)
    expect(new Set(ids).size).toBe(12)
  })

  it('every scenario references its own descriptor label', () => {
    for (const scenario of getAllScenarios()) {
      const descriptor = SCENARIO_DESCRIPTORS.find((d) => d.id === scenario.id)
      expect(descriptor, `descriptor for ${scenario.id}`).toBeDefined()
      expect(scenario.label).toBe(descriptor?.label)
    }
  })
})
