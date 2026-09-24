import { readonly, ref } from 'vue'

import { createAdminRepository, type AdminRepositoryBundle } from '@/data/create-admin-repository'
import { isScenarioId, type ScenarioId } from '@/data/mock/scenarios'

const STORAGE_KEY = 'promptdock-relay-admin:dev-scenario'
const DEFAULT_SCENARIO: ScenarioId = 'healthy'
// Static mock previews seed this storage before loading the app in acceptance
// tests. Real production builds replace this constant with false.
const scenarioSelectorEnabled = __ADMIN_MOCK_BUILD__

function readPersistedScenario(): ScenarioId {
  if (!scenarioSelectorEnabled || typeof window === 'undefined') {
    return DEFAULT_SCENARIO
  }
  const raw = window.sessionStorage.getItem(STORAGE_KEY)
  return raw && isScenarioId(raw) ? raw : DEFAULT_SCENARIO
}

function writePersistedScenario(id: ScenarioId): void {
  if (!scenarioSelectorEnabled || typeof window === 'undefined') {
    return
  }
  window.sessionStorage.setItem(STORAGE_KEY, id)
}

const scenarioRef = ref<ScenarioId>(readPersistedScenario())

const bundle: AdminRepositoryBundle = createAdminRepository({
  initialScenario: scenarioRef.value,
  getScenarioId: () => scenarioRef.value,
  testMode: true,
})

export function setActiveScenario(id: ScenarioId): void {
  scenarioRef.value = id
  writePersistedScenario(id)
  bundle.reset()
}

export function useScenarioSelector() {
  return {
    active: readonly(scenarioRef),
    setActive: setActiveScenario,
  }
}

export const adminRepository: AdminRepositoryBundle = bundle
