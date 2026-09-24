import type { ScenarioFixture } from './scenario-types'
import { fixedNow, makeCurrentAlert, makeDelivery } from './fixture-factory'
import { healthyScenario } from './healthy'

const deliveries = [
  makeDelivery('d-q-1', 'queued', 'run_event', 'device', '主控工作站', 2, 0, null),
  makeDelivery('d-q-2', 'queued', 'run_event', 'device', '主控工作站', 4, 0, null),
  makeDelivery(
    'd-q-3',
    'retrying',
    'run_event',
    'device',
    '移动协作机',
    7,
    3,
    'WECHAT_RATE_LIMITED',
  ),
  makeDelivery('d-q-4', 'queued', 'test', 'system', 'system', 1, 0, null),
  makeDelivery(
    'd-q-5',
    'retrying',
    'interactive_reply',
    'system',
    'system',
    10,
    2,
    'WECHAT_RECONNECT_REQUIRED',
  ),
]
const alerts = [
  makeCurrentAlert('iss-q-1', 'OUTBOX_BACKLOG', 'warning', 'outbox', '通知队列出现积压', 15),
]

export const queueBacklogScenario: ScenarioFixture = {
  id: 'queue-backlog',
  label: '队列积压',
  description: '通知投递出现明显积压',
  data: {
    ...healthyScenario.data,
    deliveries,
    overview: {
      ...healthyScenario.data.overview,
      queue: { pending: 5, sending: 0, retrying: 2, blocked: 0, failed: 0 },
      currentAlerts: alerts,
    },
    system: {
      ...healthyScenario.data.system,
      workers: healthyScenario.data.system.workers.map((w) =>
        w.name === 'outbox-worker'
          ? { ...w, state: 'degraded' as const, detail: 'pending=5 retrying=2' }
          : w,
      ),
      currentAlerts: alerts,
    },
    alerts,
  },
  behavior: {},
}

void fixedNow
