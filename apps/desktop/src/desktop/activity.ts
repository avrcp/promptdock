import { computed, inject, provide, ref, type InjectionKey } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export const activityFilters = [
  'attention',
  'started',
  'results',
  'deliveryIssues',
  'recent',
] as const
export type ActivityFilter = (typeof activityFilters)[number]

export type ActivityCounts = Record<ActivityFilter, number>
export type ActivityItem = {
  runKey: string
  workspaceLabel: string
  displayTitle: string
  activityRevision: number
  phase:
    | 'started'
    | 'settling'
    | 'ended_observed'
    | 'interrupted'
    | 'failed_observed'
    | 'cancelled'
    | 'unknown'
  startedAt: number | null
  lastObservedAt: number
  attention: null | {
    revision: number
    acknowledgedRevision: number
    label: string
    historical: boolean
    observationExpiresAt: number | null
  }
  result: null | {
    outboxId: string
    resultRevision: number
    pageState: string | null
    expiresAt: number | null
    seenResultRevision: number
  }
  delivery: null | { state: string; lastErrorCode: string | null; heldUntil: number | null }
}
export type ActivityEvent = { id: string; kind: string; occurredAt: number; observedAt: number }
export type ActivityDelivery = {
  id: string
  kind: string
  state: string
  pageState: string | null
  resultRevision: number | null
}
export type ActivityDetail = {
  item: ActivityItem
  events: ActivityEvent[]
  deliveries: ActivityDelivery[]
}
export type ActivityPage = {
  items: ActivityItem[]
  nextCursor: string | null
  counts: ActivityCounts
}

const ACTIVITY_EVENT = 'activity-changed'
const DEFAULT_PAGE_LIMIT = 20
const DEFAULT_CACHE_LIMIT = 100
const HARD_CACHE_LIMIT = 300
const wireFilter: Record<ActivityFilter, string> = {
  attention: 'attention',
  started: 'started',
  results: 'results',
  deliveryIssues: 'delivery_issues',
  recent: 'recent',
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function integerOrNull(value: unknown): number | null {
  return value === null
    ? null
    : typeof value === 'number' && Number.isSafeInteger(value)
      ? value
      : null
}

function isActivityItem(value: unknown): value is ActivityItem {
  if (!isRecord(value)) return false
  return (
    typeof value.runKey === 'string' &&
    value.runKey.length > 0 &&
    typeof value.workspaceLabel === 'string' &&
    typeof value.displayTitle === 'string' &&
    Number.isSafeInteger(value.activityRevision) &&
    [
      'started',
      'settling',
      'ended_observed',
      'interrupted',
      'failed_observed',
      'cancelled',
      'unknown',
    ].includes(value.phase as string) &&
    integerOrNull(value.startedAt) === value.startedAt &&
    Number.isSafeInteger(value.lastObservedAt) &&
    (value.attention === null ||
      (isRecord(value.attention) &&
        Number.isSafeInteger(value.attention.revision) &&
        Number.isSafeInteger(value.attention.acknowledgedRevision) &&
        typeof value.attention.label === 'string' &&
        typeof value.attention.historical === 'boolean' &&
        integerOrNull(value.attention.observationExpiresAt) ===
          value.attention.observationExpiresAt)) &&
    (value.result === null ||
      (isRecord(value.result) &&
        typeof value.result.outboxId === 'string' &&
        Number.isSafeInteger(value.result.resultRevision) &&
        (value.result.pageState === null || typeof value.result.pageState === 'string') &&
        integerOrNull(value.result.expiresAt) === value.result.expiresAt &&
        Number.isSafeInteger(value.result.seenResultRevision))) &&
    (value.delivery === null ||
      (isRecord(value.delivery) &&
        typeof value.delivery.state === 'string' &&
        (value.delivery.lastErrorCode === null ||
          typeof value.delivery.lastErrorCode === 'string') &&
        integerOrNull(value.delivery.heldUntil) === value.delivery.heldUntil))
  )
}

function isCounts(value: unknown): value is ActivityCounts {
  return (
    isRecord(value) &&
    activityFilters.every(
      (filter) => Number.isSafeInteger(value[filter]) && (value[filter] as number) >= 0,
    )
  )
}

function isPage(value: unknown): value is ActivityPage {
  return (
    isRecord(value) &&
    Array.isArray(value.items) &&
    value.items.every(isActivityItem) &&
    (value.nextCursor === null || typeof value.nextCursor === 'string') &&
    isCounts(value.counts)
  )
}

function isDetail(value: unknown): value is ActivityDetail {
  return (
    isRecord(value) &&
    isActivityItem(value.item) &&
    Array.isArray(value.events) &&
    value.events.every(
      (event) =>
        isRecord(event) &&
        typeof event.id === 'string' &&
        typeof event.kind === 'string' &&
        Number.isSafeInteger(event.occurredAt) &&
        Number.isSafeInteger(event.observedAt),
    ) &&
    Array.isArray(value.deliveries) &&
    value.deliveries.every(
      (delivery) =>
        isRecord(delivery) &&
        typeof delivery.id === 'string' &&
        typeof delivery.kind === 'string' &&
        typeof delivery.state === 'string' &&
        (delivery.pageState === null || typeof delivery.pageState === 'string') &&
        integerOrNull(delivery.resultRevision) === delivery.resultRevision,
    )
  )
}

function emptyCounts(): ActivityCounts {
  return { attention: 0, started: 0, results: 0, deliveryIssues: 0, recent: 0 }
}

/** Application-level, metadata-only activity projection. */
export function createActivityController(options: { cacheLimit?: number } = {}) {
  const cacheLimit = Math.min(
    Math.max(options.cacheLimit ?? DEFAULT_CACHE_LIMIT, 1),
    HARD_CACHE_LIMIT,
  )
  const filter = ref<ActivityFilter>('attention')
  const items = ref<ActivityItem[]>([])
  const counts = ref<ActivityCounts>(emptyCounts())
  const nextCursor = ref<string | null>(null)
  const pagePending = ref(false)
  const detailPending = ref(false)
  const actionPending = ref(false)
  const error = ref('')
  const selected = ref<ActivityDetail | null>(null)
  const selectedRunKey = ref<string | null>(null)
  const capped = ref(false)
  let disposed = false
  let requestEpoch = 0
  let detailEpoch = 0
  let pageInFlight: Promise<void> | null = null
  let pageDirty = false
  let initialSnapshotDecided = false
  let userSelectedFilter = false
  let unlisten: UnlistenFn | null = null
  let focusListener: (() => void) | null = null
  let timer: ReturnType<typeof setInterval> | null = null

  const selectedItem = computed(() => selected.value?.item ?? null)

  function mergePage(page: ActivityPage, append: boolean) {
    if (!append) {
      const next = page.items.slice(0, cacheLimit)
      // A keyed DOM node preserves identity, but inserting a newer row ahead of a
      // focused action still moves the user's reading position. Keep that card at
      // its current index until focus leaves it; the following refresh restores
      // repository ordering.
      const focusedRunKey = (document.activeElement as Element | null)?.closest<HTMLElement>(
        '[data-run-key]',
      )?.dataset.runKey
      if (focusedRunKey) {
        const previousIndex = items.value.findIndex((item) => item.runKey === focusedRunKey)
        const nextIndex = next.findIndex((item) => item.runKey === focusedRunKey)
        if (previousIndex >= 0 && nextIndex >= 0 && previousIndex !== nextIndex) {
          const [focused] = next.splice(nextIndex, 1)
          if (focused) next.splice(Math.min(previousIndex, next.length), 0, focused)
        }
      }
      items.value = next
      capped.value = page.items.length > cacheLimit
    } else {
      const merged = new Map(items.value.map((item) => [item.runKey, item]))
      for (const item of page.items) merged.set(item.runKey, item)
      const next = [...merged.values()].slice(0, cacheLimit)
      capped.value = merged.size > cacheLimit || next.length >= cacheLimit
      items.value = next
    }
    nextCursor.value = capped.value ? null : page.nextCursor
    counts.value = page.counts
    if (!initialSnapshotDecided && !userSelectedFilter) {
      initialSnapshotDecided = true
      if (filter.value === 'attention' && page.counts.attention === 0) {
        filter.value = 'recent'
        pageDirty = true
      }
    }
  }

  async function readPage(append: boolean) {
    const cursor = append ? nextCursor.value : null
    if (append && (!cursor || capped.value)) return
    const epoch = ++requestEpoch
    const page = await invoke<unknown>('desktop_activity_page', {
      filter: wireFilter[filter.value],
      cursor,
      limit: DEFAULT_PAGE_LIMIT,
    })
    if (disposed || epoch !== requestEpoch) return
    if (!isPage(page)) throw new Error('活动记录回执无法确认，请刷新后重试。')
    mergePage(page, append)
  }

  async function refresh() {
    if (disposed) return
    if (pageInFlight) {
      pageDirty = true
      return pageInFlight
    }
    pagePending.value = true
    error.value = ''
    pageInFlight = (async () => {
      do {
        pageDirty = false
        try {
          await readPage(false)
        } catch (reason) {
          if (!disposed)
            error.value = reason instanceof Error ? reason.message : '活动记录暂不可读取。'
        }
      } while (pageDirty && !disposed)
    })().finally(() => {
      pageInFlight = null
      pagePending.value = false
    })
    return pageInFlight
  }

  async function loadMore() {
    if (disposed || !nextCursor.value || capped.value || pageInFlight) return
    pagePending.value = true
    try {
      await readPage(true)
    } catch (reason) {
      if (!disposed)
        error.value = reason instanceof Error ? reason.message : '更多活动记录暂不可读取。'
    } finally {
      pagePending.value = false
    }
  }

  async function setFilter(next: ActivityFilter) {
    userSelectedFilter = true
    if (filter.value === next) return
    filter.value = next
    selected.value = null
    selectedRunKey.value = null
    nextCursor.value = null
    items.value = []
    await refresh()
  }

  async function openDetail(runKey: string) {
    selectedRunKey.value = runKey
    const epoch = ++detailEpoch
    detailPending.value = true
    try {
      const result = await invoke<unknown>('desktop_activity_detail', { runKey })
      if (disposed || epoch !== detailEpoch || selectedRunKey.value !== runKey) return
      if (!isDetail(result)) throw new Error('活动详情回执无法确认。')
      selected.value = result
    } catch (reason) {
      if (!disposed && epoch === detailEpoch)
        error.value = reason instanceof Error ? reason.message : '活动详情暂不可读取。'
    } finally {
      if (!disposed && epoch === detailEpoch) detailPending.value = false
    }
  }

  function closeDetail() {
    detailEpoch++
    selectedRunKey.value = null
    selected.value = null
    detailPending.value = false
  }

  async function acknowledge(item: ActivityItem) {
    if (actionPending.value || item.attention === null) return
    actionPending.value = true
    try {
      await invoke('desktop_attention_ack', {
        runKey: item.runKey,
        observedRevision: item.attention.revision,
      })
      await refresh()
      if (selectedRunKey.value === item.runKey) await openDetail(item.runKey)
    } catch {
      if (!disposed) error.value = '确认注意事项未完成，请稍后重试。'
    } finally {
      if (!disposed) actionPending.value = false
    }
  }

  async function markSeen(item: ActivityItem) {
    if (actionPending.value || item.result === null) return
    actionPending.value = true
    try {
      await invoke('desktop_result_mark_seen', {
        runKey: item.runKey,
        resultRevision: item.result.resultRevision,
      })
      await refresh()
      if (selectedRunKey.value === item.runKey) await openDetail(item.runKey)
    } catch {
      if (!disposed) error.value = '标记已查看未完成，请稍后重试。'
    } finally {
      if (!disposed) actionPending.value = false
    }
  }

  async function start() {
    if (disposed || unlisten || focusListener) return
    try {
      unlisten = await listen(ACTIVITY_EVENT, () => void refresh())
      if (disposed) {
        await unlisten()
        unlisten = null
        return
      }
    } catch {
      // Focus recovery still keeps this small, local projection current.
    }
    focusListener = () => void refresh()
    window.addEventListener('focus', focusListener)
    timer = setInterval(() => void refresh(), 30_000)
    await refresh()
  }

  function dispose() {
    disposed = true
    requestEpoch++
    detailEpoch++
    if (focusListener) window.removeEventListener('focus', focusListener)
    focusListener = null
    if (timer) clearInterval(timer)
    timer = null
    if (unlisten) void unlisten()
    unlisten = null
  }

  return {
    filter,
    items,
    counts,
    nextCursor,
    pagePending,
    detailPending,
    actionPending,
    error,
    selected,
    selectedItem,
    capped,
    refresh,
    loadMore,
    setFilter,
    openDetail,
    closeDetail,
    acknowledge,
    markSeen,
    start,
    dispose,
  }
}

export type ActivityController = ReturnType<typeof createActivityController>
const ActivityControllerKey = Symbol('promptdock:activity') as InjectionKey<ActivityController>

export function provideActivityController(): ActivityController {
  const controller = createActivityController()
  provide(ActivityControllerKey, controller)
  return controller
}

export function useActivityController(): ActivityController {
  const controller = inject(ActivityControllerKey, null)
  if (!controller)
    throw new Error('useActivityController 必须在 provideActivityController 的后代组件中调用')
  return controller
}
