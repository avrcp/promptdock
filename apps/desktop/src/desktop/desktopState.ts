import { computed, inject, provide, ref, type InjectionKey } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { StatusTone } from '../components/StatusChip.vue'
import { type Delivery } from './dictionaries'
import {
  parseHookHealth,
  verificationLabels,
  type HookHealth,
} from './hookHealth'
import { viewMeta, type ViewKey } from './views'

export type Policy = {
  observe_turns: boolean
  notify_started: boolean
  notify_ended: boolean
  completion_quiet_ms: number
  result_content_mode: 'status_only' | 'redacted_excerpt' | 'full_final'
  notify_attention: boolean
  include_task_input: boolean
}

export type DesktopStatus = {
  dataDirectory: string
  policy: Policy
  hookHome: string | null
  hookState?: string
  hookScope?: string
  inboxPresent: boolean
  policyRevision?: number
  policyApplyStatus?: 'saved' | 'pending' | 'unchanged' | 'saved_pending_apply'
}

export type LauncherConfig = {
  proxy: { enabled: boolean; host: string; port: number; noProxy: string[] }
  desktop: { selectedExecutable: string | null; refuseIfRunning: boolean }
}

export type LauncherStatus = {
  config: LauncherConfig
  candidates: { executable: string; productLabel: string; packageVersion: string | null }[]
  selectedRunning: boolean
  discoveryIssue: { message: string } | null
  selectionIssue?: { message: string } | null
}

type PolicyState = { policy: Policy; captureGeneration: number; revision: number }
type PolicySaveReceipt = {
  status: 'saved' | 'unchanged' | 'saved_pending_apply' | 'conflict'
  state: PolicyState
  errorCode?: string | null
}

export type HookPlan = {
  registrationId: string
  sourceFingerprint: string | null
  expectedPolicy: Policy
  changedEvents: string[]
  definitionChanged: boolean
  outcome: 'installed' | 'repaired' | 'no_change'
  reviewRequired: boolean | null
  externalReviewRequired: boolean
  operationId: string
}

export type RelayState =
  | 'not_configured'
  | 'checking'
  | 'ready'
  | 'unreachable'
  | 'auth_failed'
  | 'permission_denied'
  | 'capability_missing'
  | 'stale'

export type RelayStatusCore = {
  runtimeEpoch: string
  revision: number
  state: RelayState
  canSubmit: boolean | null
  canReadOwn: boolean | null
  configured: boolean
  reachable: boolean
  authenticated: boolean
  lastErrorCode: string | null
}

export type RelayStatus = RelayStatusCore & {
  baseUrl?: string | null
}

const RELAY_STATUS_EVENT = 'relay-status-changed'
const DELIVERIES_EVENT = 'deliveries-changed'
const CAPTURE_STATUS_EVENT = 'promptdock://capture-status-changed'
const RELAY_FALLBACK_INTERVAL_MS = 30_000
const LOCAL_STATUS_FALLBACK_INTERVAL_MS = 10_000
const DELIVERY_PAGE_LIMIT = 20
const DELIVERY_CACHE_LIMIT = 300

type RelayVersion = Pick<RelayStatusCore, 'runtimeEpoch' | 'revision'>
type CaptureStatus = { status: string; lastErrorCode?: string | null }
type DeliveryPage = { items: Delivery[]; nextCursor: string | null }
type OperationResource =
  'global' | 'launcher' | 'hook' | 'relay' | 'policy' | 'deliveries' | 'diagnostics'

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

function sameValue(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function isPolicy(value: unknown): value is Policy {
  if (!isRecord(value)) return false
  return (
    typeof value.observe_turns === 'boolean' &&
    typeof value.notify_started === 'boolean' &&
    typeof value.notify_ended === 'boolean' &&
    Number.isInteger(value.completion_quiet_ms) &&
    typeof value.completion_quiet_ms === 'number' &&
    value.completion_quiet_ms >= 500 &&
    value.completion_quiet_ms <= 20_000 &&
    ['status_only', 'redacted_excerpt', 'full_final'].includes(
      value.result_content_mode as string,
    ) &&
    typeof value.notify_attention === 'boolean' &&
    typeof value.include_task_input === 'boolean'
  )
}

function isLauncherConfig(value: unknown): value is LauncherConfig {
  if (!isRecord(value) || !isRecord(value.proxy) || !isRecord(value.desktop)) return false
  return (
    typeof value.proxy.enabled === 'boolean' &&
    typeof value.proxy.host === 'string' &&
    typeof value.proxy.port === 'number' &&
    Number.isInteger(value.proxy.port) &&
    value.proxy.port >= 1 &&
    value.proxy.port <= 65_535 &&
    Array.isArray(value.proxy.noProxy) &&
    value.proxy.noProxy.every((item) => typeof item === 'string') &&
    (value.desktop.selectedExecutable === null ||
      typeof value.desktop.selectedExecutable === 'string') &&
    typeof value.desktop.refuseIfRunning === 'boolean'
  )
}

function isHookPlan(value: unknown): value is HookPlan {
  if (!isRecord(value)) return false
  return (
    typeof value.registrationId === 'string' &&
    value.registrationId.length > 0 &&
    typeof value.operationId === 'string' &&
    value.operationId.length > 0 &&
    (value.sourceFingerprint === null || typeof value.sourceFingerprint === 'string') &&
    isPolicy(value.expectedPolicy) &&
    Array.isArray(value.changedEvents) &&
    value.changedEvents.every((item) => typeof item === 'string') &&
    typeof value.definitionChanged === 'boolean' &&
    ['installed', 'repaired', 'no_change'].includes(value.outcome as string) &&
    (value.reviewRequired === null || typeof value.reviewRequired === 'boolean') &&
    typeof value.externalReviewRequired === 'boolean'
  )
}

function isPolicySaveReceipt(value: unknown): value is PolicySaveReceipt {
  if (!isRecord(value) || !isRecord(value.state)) return false
  const state = value.state
  return (
    ['saved', 'unchanged', 'saved_pending_apply', 'conflict'].includes(value.status as string) &&
    isPolicy(state.policy) &&
    Number.isSafeInteger(state.captureGeneration) &&
    Number.isSafeInteger(state.revision) &&
    (value.errorCode === undefined || value.errorCode === null || typeof value.errorCode === 'string')
  )
}

/**
 * 桌面运行时状态。
 * 以工厂函数而非模块单例提供：每次 mount 得到独立实例，
 * 测试之间不会互相污染。
 */
export function createDesktopState() {
  const status = ref<DesktopStatus | null>(null)
  const hookHealth = ref<HookHealth | null>(null)
  const hookHealthError = ref('')
  const launcher = ref<LauncherStatus | null>(null)
  const launcherDraft = ref<LauncherConfig | null>(null)
  const launcherBaseline = ref<LauncherConfig | null>(null)
  const launcherBaselineRevision = ref(0)
  const launcherBaselineChanged = ref(false)
  const relay = ref<RelayStatus | null>(null)
  const deliveries = ref<Delivery[]>([])
  const deliveriesNextCursor = ref<string | null>(null)
  const deliveriesCapped = ref(false)
  const deliveriesPagePending = ref(false)
  const activeView = ref<ViewKey>('overview')
  const home = ref('')
  const homeBaseline = ref('')
  const homeBaselineRevision = ref(0)
  const homeBaselineChanged = ref(false)
  const hookPlan = ref<HookPlan | null>(null)
  const hookPlanHome = ref('')
  const policyDraft = ref<Policy | null>(null)
  const policyBaseline = ref<Policy | null>(null)
  // The CAS revision belongs to the saved baseline, not the latest status
  // snapshot.  A dirty draft must never inherit a newer external revision.
  const policyBaselinePolicyRevision = ref<number | null>(null)
  const policyBaselineRevision = ref(0)
  const policyBaselineChanged = ref(false)
  const relayUrl = ref('')
  const deviceToken = ref('')
  const pending = ref<Record<OperationResource, number>>({
    global: 0,
    launcher: 0,
    hook: 0,
    relay: 0,
    policy: 0,
    deliveries: 0,
    diagnostics: 0,
  })
  const message = ref('')
  const messageTone = ref<'success' | 'danger'>('success')
  const autostart = ref(false)
  const diagnostics = ref('')
  const loadFailed = ref(false)
  let disposed = false
  let subscriptionsStarted = false
  let relaySubscriptionFailed = false
  let deliverySubscriptionFailed = false
  let relayEventGeneration = 0
  let desktopRequestGeneration = 0
  let launcherRequestGeneration = 0
  let deliveryHeadGeneration = 0
  let deliveryMoreGeneration = 0
  const deliveryDatasetEpoch = 0
  let deliveryPageDepth = 0
  let relayVersion: RelayVersion | null = null
  let relayFallbackTimer: ReturnType<typeof setInterval> | null = null
  let deliveryFallbackTimer: ReturnType<typeof setInterval> | null = null
  let localStatusFallbackTimer: ReturnType<typeof setInterval> | null = null
  let fallbackInFlight = false
  let deliveryFallbackInFlight = false
  let hookHealthInFlight: Promise<void> | null = null
  const unlisteners: UnlistenFn[] = []
  const recoveryListeners: Array<() => void> = []

  const currentMeta = computed(() => viewMeta[activeView.value])
  const busy = computed(() => Object.values(pending.value).some((count) => count > 0))
  const hookRefreshing = computed(() => pending.value.hook > 0)
  const hookHealthPending = computed(() => pending.value.hook > 0)
  const launcherDirty = computed(
    () => launcherDraft.value !== null && !sameValue(launcherDraft.value, launcherBaseline.value),
  )
  const policyDirty = computed(
    () => policyDraft.value !== null && !sameValue(policyDraft.value, policyBaseline.value),
  )
  const homeDirty = computed(() => home.value !== homeBaseline.value)
  const hookPlanCurrent = computed(
    () =>
      hookPlan.value !== null &&
      hookPlanHome.value === home.value.trim() &&
      sameValue(hookPlan.value.expectedPolicy, status.value?.policy),
  )
  const launcherErrors = computed(() => {
    const errors: { proxyHost?: string; proxyPort?: string; executable?: string } = {}
    const draft = launcherDraft.value
    if (!draft) return errors
    if (draft.proxy.enabled && !draft.proxy.host.trim()) errors.proxyHost = '请输入本地代理地址。'
    if (!Number.isInteger(draft.proxy.port) || draft.proxy.port < 1 || draft.proxy.port > 65_535)
      errors.proxyPort = '端口必须是 1–65535 的整数。'
    return errors
  })
  const launcherValid = computed(() => Object.keys(launcherErrors.value).length === 0)
  const policyErrors = computed(() => {
    const errors: { completionQuietMs?: string } = {}
    const value = policyDraft.value?.completion_quiet_ms
    if (value !== undefined && (!Number.isInteger(value) || value < 500 || value > 20_000))
      errors.completionQuietMs = '静默窗口必须是 500–20000 毫秒的整数。'
    return errors
  })
  const policyValid = computed(() => Object.keys(policyErrors.value).length === 0)

  const hookLabel = computed(() => {
    if (hookHealthError.value) return hookHealthError.value
    const health = hookHealth.value
    if (!health) return '正在读取 Hook 状态'
    if (health.installation === 'absent') return '尚未安装 Hook'
    if (health.installation === 'needs_repair' || health.installation === 'error')
      return health.diagnosticCode || 'Hook 配置需要修复'
    if (!health.observationEnabled) return '已暂停接收'
    if (!health.fresh) return health.diagnosticCode || 'Hook 证据已过期'
    if (health.handlers.some((handler) => handler.trust === 'explicitly_disabled'))
      return '已在宿主禁用'
    if (health.handlers.some((handler) => handler.lastErrorCode)) return '最近接收发生错误'
    if (health.handlers.some((handler) => handler.observationCurrent)) return '已收到当前 Hook 事件'
    return verificationLabels[health.verification.state]
  })

  const hookTone = computed<StatusTone>(() => {
    const health = hookHealth.value
    if (!health || health.installation === 'absent' || !health.observationEnabled) return 'neutral'
    if (
      hookHealthError.value ||
      !health.fresh ||
      health.installation === 'needs_repair' ||
      health.installation === 'error' ||
      health.handlers.some((handler) => handler.lastErrorCode)
    )
      return 'danger'
    if (health.handlers.some((handler) => handler.observationCurrent)) return 'success'
    return 'warning'
  })

  const hostLabel = computed(() => {
    if (!launcher.value?.config.desktop.selectedExecutable) return '未配置'
    return launcher.value.selectedRunning ? '正在运行' : '已选择，未运行'
  })

  const hostTone = computed<StatusTone>(() => {
    if (!launcher.value?.config.desktop.selectedExecutable) return 'neutral'
    return launcher.value.selectedRunning ? 'success' : 'warning'
  })

  const relayLabel = computed(() => {
    if (!relay.value?.configured) return '未配置'
    switch (relay.value.state) {
      case 'checking':
        return '正在检查连接'
      case 'ready':
        if (relay.value.canSubmit === false) return '缺少通知发送权限'
        if (relay.value.canSubmit === true && relay.value.canReadOwn === false)
          return '可发送，回执读取权限不足'
        if (relay.value.canSubmit === true && relay.value.canReadOwn === true)
          return '连接与权限已验证'
        return '连接已验证，权限待确认'
      case 'unreachable':
        return '服务器不可达'
      case 'auth_failed':
        return '认证失败'
      case 'permission_denied':
        if (relay.value.canSubmit === false) return '缺少通知发送权限'
        if (relay.value.canSubmit === true && relay.value.canReadOwn === false)
          return '可发送，回执读取权限不足'
        return '权限不足'
      case 'capability_missing':
        return '服务器能力缺失'
      case 'stale':
        return '状态已过期'
      case 'not_configured':
        return '未配置'
      default:
        return '状态未知'
    }
  })

  const relayTone = computed<StatusTone>(() => {
    if (!relay.value?.configured) return 'neutral'
    if (relay.value.state === 'ready') {
      return relay.value.canSubmit === true && relay.value.canReadOwn === true
        ? 'success'
        : 'warning'
    }
    if (
      relay.value.state === 'auth_failed' ||
      relay.value.state === 'permission_denied' ||
      relay.value.state === 'capability_missing' ||
      relay.value.state === 'unreachable'
    )
      return 'danger'
    return 'warning'
  })

  function compareRelayVersion(left: RelayVersion, right: RelayVersion) {
    if (left.runtimeEpoch !== right.runtimeEpoch) return null
    return left.revision - right.revision
  }

  function applyRelayEvent(next: RelayStatusCore) {
    if (disposed) return
    if (relayVersion) {
      const sameEpochComparison = compareRelayVersion(next, relayVersion)
      if (sameEpochComparison !== null && sameEpochComparison <= 0) return
    }
    relayVersion = { runtimeEpoch: next.runtimeEpoch, revision: next.revision }
    relayEventGeneration += 1
    relay.value = { ...relay.value, ...next }
  }

  function applyRelaySnapshot(next: RelayStatus, startedAtGeneration: number) {
    if (disposed) return
    const current = relayVersion
    if (current && startedAtGeneration !== relayEventGeneration) {
      // An event arrived while the command was in flight. A snapshot from an
      // older epoch is necessarily late; same-epoch revisions still go through
      // the normal monotonic check below.
      if (next.runtimeEpoch !== current.runtimeEpoch) return
    }
    if (current) {
      const sameEpochComparison = compareRelayVersion(next, current)
      if (sameEpochComparison !== null && sameEpochComparison < 0) return
    }
    relayVersion = { runtimeEpoch: next.runtimeEpoch, revision: next.revision }
    relay.value = next
    if (!relayUrl.value) relayUrl.value = next.baseUrl ?? ''
  }

  function applyPolicyBaseline(next: Policy, revision: number | undefined) {
    const wasDirty = policyDirty.value
    const changed = policyBaseline.value !== null && !sameValue(policyBaseline.value, next)
    if (!wasDirty) {
      policyDraft.value = clone(next)
      policyBaseline.value = clone(next)
      policyBaselinePolicyRevision.value =
        typeof revision === 'number' && Number.isSafeInteger(revision) ? revision : null
      policyBaselineChanged.value = false
    } else if (changed) policyBaselineChanged.value = true
    if (changed) policyBaselineRevision.value += 1
  }

  function applyHomeBaseline(next: string) {
    const wasDirty = homeDirty.value
    const changed = homeBaseline.value !== next
    if (!wasDirty) {
      home.value = next
      homeBaseline.value = next
      homeBaselineChanged.value = false
    } else if (changed) homeBaselineChanged.value = true
    if (changed) homeBaselineRevision.value += 1
  }

  function applyLauncherSnapshot(next: LauncherStatus, requestGeneration: number) {
    if (disposed || requestGeneration !== launcherRequestGeneration) return
    const wasDirty = launcherDirty.value
    const changed =
      launcherBaseline.value !== null && !sameValue(launcherBaseline.value, next.config)
    launcher.value = next
    if (!wasDirty) {
      launcherDraft.value = clone(next.config)
      launcherBaseline.value = clone(next.config)
      launcherBaselineChanged.value = false
    } else if (changed) launcherBaselineChanged.value = true
    if (changed) launcherBaselineRevision.value += 1
  }

  function applyDesktopSnapshot(next: DesktopStatus, requestGeneration: number) {
    if (disposed) return
    if (requestGeneration !== desktopRequestGeneration) return
    status.value = next
    applyPolicyBaseline(next.policy, next.policyRevision)
    applyHomeBaseline(next.hookHome ?? '')
  }

  function setPending(resource: OperationResource, delta: 1 | -1) {
    pending.value = {
      ...pending.value,
      [resource]: Math.max(0, pending.value[resource] + delta),
    }
  }

  function isPending(resource: OperationResource) {
    return pending.value[resource] > 0
  }

  async function withPending<T>(resource: OperationResource, action: () => Promise<T>): Promise<T> {
    if (isPending(resource)) throw new Error('该操作正在进行，请稍候。')
    setPending(resource, 1)
    try {
      return await action()
    } finally {
      setPending(resource, -1)
    }
  }

  function isVisible() {
    return typeof document === 'undefined' || document.visibilityState === 'visible'
  }

  function stopRelayFallbackTimer() {
    if (relayFallbackTimer !== null) {
      clearInterval(relayFallbackTimer)
      relayFallbackTimer = null
    }
  }

  function stopDeliveryFallbackTimer() {
    if (deliveryFallbackTimer !== null) {
      clearInterval(deliveryFallbackTimer)
      deliveryFallbackTimer = null
    }
  }

  function stopLocalStatusFallbackTimer() {
    if (localStatusFallbackTimer !== null) {
      clearInterval(localStatusFallbackTimer)
      localStatusFallbackTimer = null
    }
  }

  function startRelayFallbackTimer() {
    stopRelayFallbackTimer()
    if (disposed || !isVisible()) return
    relayFallbackTimer = setInterval(() => {
      void refreshRelaySnapshot(false)
    }, RELAY_FALLBACK_INTERVAL_MS)
  }

  async function refreshDeliveryHistoryFallback() {
    if (disposed || !isVisible() || deliveryFallbackInFlight) return
    deliveryFallbackInFlight = true
    try {
      await refreshDeliveries(false)
    } finally {
      deliveryFallbackInFlight = false
    }
  }

  function startDeliveryFallbackTimer() {
    stopDeliveryFallbackTimer()
    if (disposed || !isVisible()) return
    deliveryFallbackTimer = setInterval(() => {
      void refreshDeliveryHistoryFallback()
    }, RELAY_FALLBACK_INTERVAL_MS)
  }

  function startLocalStatusFallbackTimer() {
    stopLocalStatusFallbackTimer()
    if (disposed || !isVisible()) return
    localStatusFallbackTimer = setInterval(() => {
      void refreshHookStatus(false)
    }, LOCAL_STATUS_FALLBACK_INTERVAL_MS)
  }

  async function refreshHookStatus(showError = true) {
    if (disposed || !isVisible()) return
    if (hookHealthInFlight) return hookHealthInFlight
    if (showError) setPending('hook', 1)
    hookHealthInFlight = (async () => {
      try {
        const next = parseHookHealth(await invoke('desktop_hook_health'))
        if (!next) throw new Error('HOOK_DTO_UNKNOWN')
        if (disposed) return
        hookHealth.value = next
        hookHealthError.value = ''
      } catch {
        if (disposed) return
        if (hookHealth.value) hookHealth.value = { ...hookHealth.value, fresh: false }
        hookHealthError.value = 'Hook 状态暂不可读，请重新检查。'
        if (showError) {
          messageTone.value = 'danger'
          message.value = hookHealthError.value
        }
      }
    })().finally(() => {
      hookHealthInFlight = null
      if (showError) setPending('hook', -1)
    })
    return hookHealthInFlight
  }

  async function refreshRelaySnapshot(showError: boolean) {
    if (disposed || !isVisible() || fallbackInFlight) return
    fallbackInFlight = true
    const startedAtGeneration = relayEventGeneration
    try {
      const next = await invoke<RelayStatus>('desktop_relay_status')
      applyRelaySnapshot(next, startedAtGeneration)
    } catch {
      if (showError) {
        messageTone.value = 'danger'
        message.value = 'Relay 状态读取失败，请稍后重试。'
      }
    } finally {
      fallbackInFlight = false
    }
  }

  function onRecovery() {
    if (disposed || !isVisible()) return
    void refreshHookStatus(false)
    void refreshRelaySnapshot(false)
    void refreshDeliveryHistoryFallback()
    if (relaySubscriptionFailed) startRelayFallbackTimer()
    if (deliverySubscriptionFailed) startDeliveryFallbackTimer()
    startLocalStatusFallbackTimer()
  }

  function onVisibilityChange() {
    if (!isVisible()) {
      stopRelayFallbackTimer()
      stopDeliveryFallbackTimer()
      stopLocalStatusFallbackTimer()
      return
    }
    onRecovery()
  }

  function installRecoveryListeners() {
    if (typeof window !== 'undefined') {
      window.addEventListener('focus', onRecovery)
      recoveryListeners.push(() => window.removeEventListener('focus', onRecovery))
    }
    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', onVisibilityChange)
      recoveryListeners.push(() =>
        document.removeEventListener('visibilitychange', onVisibilityChange),
      )
    }
  }

  async function startSubscriptions() {
    if (subscriptionsStarted || disposed) return
    subscriptionsStarted = true
    const register = async (pending: Promise<UnlistenFn>) => {
      try {
        const unlisten = await pending
        if (disposed) {
          // A listen promise may resolve after unmount. Dispose it immediately
          // instead of retaining a listener in the native event loop.
          void unlisten()
        } else {
          unlisteners.push(unlisten)
        }
        return true
      } catch {
        return false
      }
    }
    const [relayRegistered, deliveryRegistered] = await Promise.all([
      register(
        listen<RelayStatusCore>(RELAY_STATUS_EVENT, (event) => applyRelayEvent(event.payload)),
      ),
      register(listen(DELIVERIES_EVENT, () => void refreshDeliveries(false))),
      register(
        listen<CaptureStatus>(CAPTURE_STATUS_EVENT, (event) => {
          void event.payload
          void refreshHookStatus(false)
        }),
      ),
    ])
    relaySubscriptionFailed = !relayRegistered
    deliverySubscriptionFailed = !deliveryRegistered
    if (disposed) return
    installRecoveryListeners()
    if (relaySubscriptionFailed) startRelayFallbackTimer()
    if (deliverySubscriptionFailed) startDeliveryFallbackTimer()
    // Trust/configuration changes are independent from capture events. Keep a
    // bounded visible-only poll even when the capture listener is healthy.
    startLocalStatusFallbackTimer()
  }

  function navigate(view: ViewKey) {
    activeView.value = view
    if (view === 'deliveries') void refreshDeliveries()
  }

  function mergeDeliveries(primary: Delivery[], retained: Delivery[]) {
    const merged = new Map<string, Delivery>()
    for (const item of [...primary, ...retained]) {
      const current = merged.get(item.id)
      const nextUpdatedAt = (item as Delivery & { updatedAt?: number }).updatedAt ?? 0
      const currentUpdatedAt = (current as (Delivery & { updatedAt?: number }) | undefined)?.updatedAt ?? -1
      if (!current || nextUpdatedAt >= currentUpdatedAt) merged.set(item.id, item)
    }
    const result = [...merged.values()]
    deliveriesCapped.value = result.length > DELIVERY_CACHE_LIMIT
    return result.slice(0, DELIVERY_CACHE_LIMIT)
  }

  async function refreshDeliveries(showError = true) {
    const requestGeneration = ++deliveryHeadGeneration
    const datasetEpoch = deliveryDatasetEpoch
    try {
      const page = await invoke<DeliveryPage>('desktop_deliveries', {
        limit: DELIVERY_PAGE_LIMIT,
        cursor: null,
      })
      if (disposed || datasetEpoch !== deliveryDatasetEpoch || requestGeneration !== deliveryHeadGeneration)
        return
      deliveries.value =
        deliveryPageDepth > 1 ? mergeDeliveries(page.items, deliveries.value) : page.items
      if (deliveryPageDepth <= 1) {
        deliveryPageDepth = 1
        deliveriesNextCursor.value = page.nextCursor
      }
    } catch {
      if (showError && !disposed && requestGeneration === deliveryHeadGeneration) {
        messageTone.value = 'danger'
        message.value = '投递状态读取失败，请稍后重试。'
      }
    }
  }

  async function loadMoreDeliveries() {
    const cursor = deliveriesNextCursor.value
    if (!cursor || deliveriesPagePending.value || disposed) return
    const requestGeneration = ++deliveryMoreGeneration
    const datasetEpoch = deliveryDatasetEpoch
    deliveriesPagePending.value = true
    setPending('deliveries', 1)
    try {
      const page = await invoke<DeliveryPage>('desktop_deliveries', {
        limit: DELIVERY_PAGE_LIMIT,
        cursor,
      })
      if (disposed || datasetEpoch !== deliveryDatasetEpoch || requestGeneration !== deliveryMoreGeneration)
        return
      deliveries.value = mergeDeliveries(deliveries.value, page.items)
      deliveriesNextCursor.value = page.nextCursor
      deliveryPageDepth += 1
    } catch {
      if (!disposed && requestGeneration === deliveryMoreGeneration) {
        messageTone.value = 'danger'
        message.value = '更多投递记录读取失败，请稍后重试。'
      }
    } finally {
      deliveriesPagePending.value = false
      setPending('deliveries', -1)
    }
  }

  async function refresh() {
    const relaySnapshotGeneration = relayEventGeneration
    const desktopSnapshotGeneration = ++desktopRequestGeneration
    const nextLauncherGeneration = ++launcherRequestGeneration
    const [nextStatus, nextLauncher, nextRelay, nextAutostart] = await Promise.allSettled([
      invoke<DesktopStatus>('desktop_status'),
      invoke<LauncherStatus>('desktop_launcher_status'),
      invoke<RelayStatus>('desktop_relay_status'),
      invoke<boolean>('desktop_autostart_status'),
    ])

    if (nextStatus.status === 'fulfilled') {
      applyDesktopSnapshot(nextStatus.value, desktopSnapshotGeneration)
    }
    if (nextLauncher.status === 'fulfilled')
      applyLauncherSnapshot(nextLauncher.value, nextLauncherGeneration)
    if (nextRelay.status === 'fulfilled')
      applyRelaySnapshot(nextRelay.value, relaySnapshotGeneration)
    if (!disposed && nextAutostart.status === 'fulfilled') autostart.value = nextAutostart.value

    if (!status.value || !launcher.value) throw new Error('核心桌面状态不可用')
    loadFailed.value = false
    return [nextStatus, nextLauncher, nextRelay, nextAutostart].some(
      (result) => result.status === 'rejected',
    )
  }

  async function refreshDesktopSnapshot() {
    const generation = ++desktopRequestGeneration
    const next = await invoke<DesktopStatus>('desktop_status')
    applyDesktopSnapshot(next, generation)
  }

  async function refreshLauncherSnapshot() {
    const generation = ++launcherRequestGeneration
    const next = await invoke<LauncherStatus>('desktop_launcher_status')
    applyLauncherSnapshot(next, generation)
  }

  async function refreshLauncherDiscovery(): Promise<void> {
    if (isPending('launcher')) return
    setPending('launcher', 1)
    const generation = ++launcherRequestGeneration
    try {
      const next = await invoke<LauncherStatus>('desktop_launcher_refresh')
      applyLauncherSnapshot(next, generation)
    } catch {
      if (!disposed) {
        messageTone.value = 'danger'
        message.value = '桌面应用发现失败；已保留当前启动设置。'
      }
    } finally {
      setPending('launcher', -1)
    }
  }

  async function refreshAll() {
    return withPending('global', async () => {
      message.value = ''
      try {
        const [partial] = await Promise.all([refresh(), refreshDeliveries(false)])
        if (partial && !disposed) {
          messageTone.value = 'danger'
          message.value = '部分状态暂时无法读取；已显示当前可用信息。'
        }
      } catch {
        if (disposed) return
        messageTone.value = 'danger'
        message.value = '状态刷新失败，请稍后重试。'
      }
    })
  }

  function clearMessage() {
    message.value = ''
  }

  function operationResource(command: string): OperationResource {
    if (command.includes('launcher') || command === 'desktop_launch') return 'launcher'
    if (command.includes('hook')) return 'hook'
    if (command.includes('relay')) return 'relay'
    if (command.includes('policy')) return 'policy'
    return 'global'
  }

  function operationError(error: unknown) {
    if (typeof error === 'string') return error
    if (error instanceof Error && error.message) return error.message
    return '操作失败，请检查配置后重试。'
  }

  function reportUnknownReceipt() {
    if (disposed) return
    messageTone.value = 'danger'
    message.value = '请求已执行但回执无法确认，请刷新状态后核对。'
  }

  async function refreshAfterMutation(
    successMessage: string,
    resources: Array<'desktop' | 'hook' | 'launcher' | 'relay' | 'autostart' | 'deliveries'>,
  ) {
    try {
      const results = await Promise.allSettled(
        resources.map((resource) => {
          if (resource === 'desktop') return refreshDesktopSnapshot()
          if (resource === 'hook') return refreshHookStatus(false)
          if (resource === 'launcher') return refreshLauncherSnapshot()
          if (resource === 'relay') return refreshRelaySnapshot(false)
          if (resource === 'autostart')
            return invoke<boolean>('desktop_autostart_status').then((next) => {
              if (!disposed) autostart.value = next
            })
          return refreshDeliveries(false)
        }),
      )
      const partial = results.some((result) => result.status === 'rejected')
      if (disposed) return
      messageTone.value = 'success'
      message.value = partial
        ? `${successMessage}，但状态刷新失败；无需重复执行操作。`
        : `${successMessage}。`
    } catch {
      if (disposed) return
      messageTone.value = 'success'
      message.value = `${successMessage}，但状态刷新失败；无需重复执行操作。`
    }
  }

  async function runMutation(
    command: string,
    args: Record<string, unknown>,
    successMessage = '操作已完成',
  ): Promise<boolean> {
    const resource = operationResource(command)
    if (isPending(resource)) return false
    message.value = ''
    setPending(resource, 1)
    try {
      await invoke(command, args)
    } catch (error) {
      if (!disposed) {
        messageTone.value = 'danger'
        message.value = operationError(error)
      }
      setPending(resource, -1)
      return false
    }
    const resources: Array<'desktop' | 'hook' | 'launcher' | 'relay' | 'autostart' | 'deliveries'> =
      resource === 'launcher'
        ? ['launcher']
        : resource === 'hook'
          ? ['desktop', 'hook']
          : resource === 'relay'
            ? ['relay', 'deliveries']
            : command.includes('autostart')
              ? ['autostart']
              : ['desktop']
    await refreshAfterMutation(successMessage, resources)
    setPending(resource, -1)
    return true
  }

  function acceptLauncherBaseline(saved: LauncherConfig) {
    launcherBaseline.value = clone(saved)
    launcherDraft.value = clone(saved)
    launcherBaselineRevision.value += 1
    launcherBaselineChanged.value = false
    if (launcher.value) launcher.value = { ...launcher.value, config: clone(saved) }
  }

  function resetLauncherDraft() {
    if (!launcher.value) return
    launcherBaseline.value = clone(launcher.value.config)
    launcherDraft.value = clone(launcher.value.config)
    launcherBaselineChanged.value = false
  }

  async function saveLauncher(): Promise<boolean> {
    if (
      isPending('launcher') ||
      !launcherDraft.value ||
      !launcherBaseline.value ||
      !launcherValid.value
    )
      return false
    if (!launcherDirty.value) return true
    const config = clone(launcherDraft.value)
    const expectedConfig = clone(launcherBaseline.value)
    message.value = ''
    let saved: LauncherConfig
    setPending('launcher', 1)
    try {
      const receipt = await invoke<unknown>('desktop_launcher_save', {
        config,
        expectedConfig,
      })
      if (!isLauncherConfig(receipt)) {
        setPending('launcher', -1)
        reportUnknownReceipt()
        return false
      }
      saved = receipt
    } catch (error) {
      if (!disposed) {
        messageTone.value = 'danger'
        message.value = operationError(error)
      }
      setPending('launcher', -1)
      return false
    }
    if (disposed) {
      setPending('launcher', -1)
      return false
    }
    acceptLauncherBaseline(saved)
    await refreshAfterMutation('启动设置已保存', ['launcher'])
    setPending('launcher', -1)
    return true
  }

  async function launchDesktop(): Promise<boolean> {
    if (launcherDirty.value && !(await saveLauncher())) return false
    return runMutation('desktop_launch', {}, '桌面宿主启动请求已提交')
  }

  function acceptPolicyBaseline(saved: Policy, revision: number) {
    policyBaseline.value = clone(saved)
    policyDraft.value = clone(saved)
    policyBaselinePolicyRevision.value = revision
    policyBaselineRevision.value += 1
    policyBaselineChanged.value = false
    if (status.value) status.value = { ...status.value, policy: clone(saved) }
  }

  function resetPolicyDraft() {
    if (!status.value) return
    policyBaseline.value = clone(status.value.policy)
    policyDraft.value = clone(status.value.policy)
    policyBaselinePolicyRevision.value =
      Number.isSafeInteger(status.value.policyRevision) ? status.value.policyRevision! : null
    policyBaselineChanged.value = false
  }

  function resetHomeDraft() {
    homeBaseline.value = status.value?.hookHome ?? ''
    home.value = homeBaseline.value
    homeBaselineChanged.value = false
  }

  async function planHook(): Promise<boolean> {
    const targetHome = home.value.trim()
    if (!targetHome || isPending('hook')) return false
    message.value = ''
    try {
      const receipt = await withPending('hook', () =>
        invoke<unknown>('desktop_hook_plan', { home: targetHome }),
      )
      if (disposed) return false
      if (!isHookPlan(receipt)) {
        messageTone.value = 'danger'
        message.value = 'Hook 变更计划回执无法解释，请刷新状态后重新检查。'
        return false
      }
      hookPlan.value = receipt
      hookPlanHome.value = targetHome
      return true
    } catch (error) {
      if (!disposed) {
        messageTone.value = 'danger'
        message.value = operationError(error)
      }
      return false
    }
  }

  async function applyHookPlan(): Promise<boolean> {
    const plan = hookPlan.value
    const targetHome = home.value.trim()
    if (!plan || !hookPlanCurrent.value || isPending('hook')) return false
    message.value = ''
    let receipt: HookPlan
    setPending('hook', 1)
    try {
      const rawReceipt = await invoke<unknown>('desktop_install_hook', {
        home: targetHome,
        registrationId: plan.registrationId,
        expectedFingerprint: plan.sourceFingerprint,
        expectedPolicy: plan.expectedPolicy,
      })
      if (!isHookPlan(rawReceipt)) {
        setPending('hook', -1)
        reportUnknownReceipt()
        return false
      }
      receipt = rawReceipt
    } catch (error) {
      if (!disposed) {
        messageTone.value = 'danger'
        message.value = operationError(error)
      }
      setPending('hook', -1)
      return false
    }
    if (disposed) {
      setPending('hook', -1)
      return false
    }
    hookPlan.value = receipt
    hookPlanHome.value = targetHome
    homeBaseline.value = targetHome
    homeBaselineRevision.value += 1
    homeBaselineChanged.value = false
    const successMessage = receipt.reviewRequired
      ? 'Hook 配置已应用，请在桌面宿主中审阅'
      : receipt.outcome === 'no_change'
        ? 'Hook 配置未变化'
        : 'Hook 配置已应用'
    await refreshAfterMutation(successMessage, ['desktop', 'hook'])
    setPending('hook', -1)
    return true
  }

  async function savePolicy(): Promise<boolean> {
    if (isPending('policy') || !policyDraft.value || !policyBaseline.value || !policyValid.value)
      return false
    if (!policyDirty.value) return true
    const policy = clone(policyDraft.value)
    const expectedRevision = policyBaselinePolicyRevision.value
    if (!Number.isSafeInteger(expectedRevision)) return false
    message.value = ''
    let receipt: PolicySaveReceipt
    setPending('policy', 1)
    try {
      const rawReceipt = await invoke<unknown>('desktop_save_policy', {
        policy,
        expectedRevision,
      })
      if (!isPolicySaveReceipt(rawReceipt)) {
        setPending('policy', -1)
        reportUnknownReceipt()
        return false
      }
      if (rawReceipt.status === 'conflict') {
        setPending('policy', -1)
        messageTone.value = 'danger'
        message.value = '通知设置已变化，请重新读取后再保存。'
        return false
      }
      receipt = rawReceipt
    } catch (error) {
      if (!disposed) {
        messageTone.value = 'danger'
        message.value = operationError(error)
      }
      setPending('policy', -1)
      return false
    }
    if (disposed) {
      setPending('policy', -1)
      return false
    }
    acceptPolicyBaseline(receipt.state.policy, receipt.state.revision)
    if (status.value) {
      status.value = {
        ...status.value,
        policy: clone(receipt.state.policy),
        policyRevision: receipt.state.revision,
        policyApplyStatus:
          receipt.status === 'conflict' ? undefined : receipt.status,
      }
    }
    hookPlan.value = null
    hookPlanHome.value = ''
    await refreshAfterMutation(
      receipt.status === 'saved_pending_apply' ? '通知设置已保存，等待运行时应用' : '通知设置已保存',
      ['desktop', 'hook'],
    )
    setPending('policy', -1)
    return true
  }

  async function act(command: string, args: Record<string, unknown> = {}) {
    if (command === 'desktop_launch') return launchDesktop()
    return runMutation(command, args)
  }

  async function configureRelay() {
    const completed = await runMutation(
      'desktop_relay_configure',
      {
        baseUrl: relayUrl.value,
        deviceToken: deviceToken.value,
      },
      'Relay 配置已保存',
    )
    if (completed) deviceToken.value = ''
  }

  async function showDiagnostics() {
    if (isPending('diagnostics')) return
    setPending('diagnostics', 1)
    try {
      const next = await invoke('desktop_diagnostics')
      if (!disposed) diagnostics.value = JSON.stringify(next, null, 2)
    } catch {
      if (!disposed) {
        messageTone.value = 'danger'
        message.value = '诊断状态读取失败，请稍后重试。'
      }
    } finally {
      setPending('diagnostics', -1)
    }
  }

  async function initialize() {
    try {
      // Register first so a status transition cannot be lost while the initial
      // command snapshot is in flight.
      await startSubscriptions()
      if (disposed) return
      const [partial, health] = await Promise.allSettled([
        refresh(),
        refreshDeliveries(false),
        refreshHookStatus(false),
      ])
      if (partial.status === 'rejected') throw partial.reason
      if (partial.status === 'fulfilled' && partial.value) {
        messageTone.value = 'danger'
        message.value = '部分状态暂时无法读取；已显示当前可用信息。'
      }
      if (health.status === 'rejected') throw health.reason
    } catch {
      loadFailed.value = true
      messageTone.value = 'danger'
      message.value = '请在 PromptDock Desktop 应用内使用设置。'
    }
  }

  function dispose() {
    if (disposed) return
    disposed = true
    stopRelayFallbackTimer()
    stopDeliveryFallbackTimer()
    stopLocalStatusFallbackTimer()
    for (const remove of recoveryListeners.splice(0)) remove()
    for (const unlisten of unlisteners.splice(0)) void unlisten()
  }

  return {
    status,
    hookHealth,
    hookHealthPending,
    launcher,
    launcherDraft,
    launcherBaseline,
    launcherBaselineRevision,
    launcherBaselineChanged,
    launcherDirty,
    launcherErrors,
    launcherValid,
    relay,
    deliveries,
    deliveriesNextCursor,
    deliveriesCapped,
    deliveriesPagePending,
    activeView,
    home,
    homeBaselineRevision,
    homeBaselineChanged,
    homeDirty,
    hookPlan,
    hookPlanCurrent,
    policyDraft,
    policyBaseline,
    policyBaselineRevision,
    policyBaselineChanged,
    policyDirty,
    policyErrors,
    policyValid,
    relayUrl,
    deviceToken,
    busy,
    hookRefreshing,
    message,
    messageTone,
    autostart,
    diagnostics,
    loadFailed,
    currentMeta,
    hookLabel,
    hookTone,
    hostLabel,
    hostTone,
    relayLabel,
    relayTone,
    navigate,
    refreshAll,
    refreshHookStatus,
    refreshDeliveries,
    refreshLauncherDiscovery,
    loadMoreDeliveries,
    isPending,
    act,
    saveLauncher,
    resetLauncherDraft,
    launchDesktop,
    savePolicy,
    resetPolicyDraft,
    resetHomeDraft,
    planHook,
    applyHookPlan,
    configureRelay,
    showDiagnostics,
    clearMessage,
    initialize,
    dispose,
  }
}

export type DesktopState = ReturnType<typeof createDesktopState>

const DesktopStateKey = Symbol('promptdock:desktop-state') as InjectionKey<DesktopState>

export function provideDesktopState(): DesktopState {
  const state = createDesktopState()
  provide(DesktopStateKey, state)
  return state
}

export function useDesktopState(): DesktopState {
  const state = inject(DesktopStateKey, null)
  if (!state) throw new Error('useDesktopState 必须在 provideDesktopState 的后代组件中调用')
  return state
}
