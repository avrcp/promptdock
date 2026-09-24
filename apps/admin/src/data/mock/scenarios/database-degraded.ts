import type { ScenarioFixture } from './scenario-types'
import { makeCurrentAlert } from './fixture-factory'
import { healthyScenario } from './healthy'

const alerts = [
  makeCurrentAlert('iss-db-1', 'DATABASE_DEGRADED', 'error', 'database', '数据库健康度下降', 30),
]

export const databaseDegradedScenario: ScenarioFixture = {
  id: 'database-degraded',
  label: '数据库降级',
  description: '数据库健康度下降但仍可读',
  data: {
    ...healthyScenario.data,
    overview: {
      ...healthyScenario.data.overview,
      currentAlerts: alerts,
    },
    system: {
      ...healthyScenario.data.system,
      database: {
        ...healthyScenario.data.system.database,
        integrityStatus: 'degraded',
        poolHealth: 'degraded',
        sizeBucket: '100_to_500mb',
      },
      workers: healthyScenario.data.system.workers.map((w) =>
        w.name === 'retention-worker'
          ? { ...w, state: 'degraded' as const, detail: 'slow checkpoint' }
          : w,
      ),
      currentAlerts: alerts,
    },
    alerts,
  },
  behavior: {},
}
