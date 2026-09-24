import {
  computed,
  getCurrentScope,
  inject,
  onScopeDispose,
  provide,
  ref,
  type ComputedRef,
} from 'vue'

import { useAdminRepository } from './useAdminRepository'
import { toAdminError, type AdminError } from '@/contracts/error'
import type { AdminCapability, AdminMeta } from '@promptdock/relay-admin-api-generated'
import type { AdminRepositoryBundle } from '@/data/create-admin-repository'

export const ADMIN_CAPABILITY_TTL_MS = 60_000

const MUTATION_CAPABILITIES: readonly AdminCapability[] = [
  'admin_device_manage_v2',
  'admin_wechat_manage_v2',
  'admin_wechat_login_v2',
  'admin_maintenance_v2',
  'admin_results_manage_v2',
]

const CAPABILITY_LABELS: Record<AdminCapability, string> = {
  admin_read_v2: '读取',
  admin_device_manage_v2: '设备管理',
  admin_wechat_status_v2: '微信状态读取',
  admin_wechat_manage_v2: '微信通道管理',
  admin_maintenance_v2: '系统维护',
  admin_wechat_login_v2: '微信登录',
  admin_results_manage_v2: '结果页管理',
}

export type AdminSessionMode = 'operator' | 'partial' | 'read-only' | 'unavailable'

export type AdminCapabilityState =
  | { kind: 'unsupported' }
  | { kind: 'loading' }
  | { kind: 'ready'; meta: AdminMeta }
  | { kind: 'unavailable'; error: AdminError }

export interface AdminSessionContext {
  state: ComputedRef<AdminCapabilityState>
  meta: ComputedRef<AdminMeta | null>
  error: ComputedRef<AdminError | null>
  loading: ComputedRef<boolean>
  refreshing: ComputedRef<boolean>
  lastSuccessAt: ComputedRef<number | null>
  stale: ComputedRef<boolean>
  mode: ComputedRef<AdminSessionMode | 'unsupported' | 'loading'>
  isReadOnly: ComputedRef<boolean>
  readOnlyReason: ComputedRef<string | null>
  capabilityReason: (capability: AdminCapability) => string | null
  can: (capability: AdminCapability) => boolean
  /** Manual retries supersede an older in-flight receipt; stale responses are ignored. */
  refresh: () => Promise<void>
  /** Explicit lifecycle hook for non-component hosts and unit tests. */
  dispose: () => void
}

export interface CreateAdminSessionOptions {
  repository?: AdminRepositoryBundle
  production?: boolean
  capabilityTtlMs?: number
  now?: () => number
  eventTarget?: EventTarget
  visibilitySource?: Pick<Document, 'visibilityState' | 'addEventListener' | 'removeEventListener'>
  setInterval?: (
    handler: () => void,
    intervalMs: number,
  ) => ReturnType<typeof globalThis.setInterval>
  clearInterval?: (timer: ReturnType<typeof globalThis.setInterval>) => void
}

const AdminSessionKey = Symbol('AdminSession')

/**
 * Session state is deliberately latest-wins. It preserves the last confirmed
 * capability receipt during a background failure, rather than briefly granting
 * no permissions or letting an obsolete request restore old permissions.
 */
export function createAdminSession(options: CreateAdminSessionOptions = {}): AdminSessionContext {
  const repository = options.repository ?? useAdminRepository()
  const state = ref<AdminCapabilityState>({ kind: 'loading' })
  const isProduction = options.production ?? __ADMIN_PRODUCTION_BUILD__
  const now = options.now ?? Date.now
  const capabilityTtlMs = options.capabilityTtlMs ?? ADMIN_CAPABILITY_TTL_MS
  if (!Number.isSafeInteger(capabilityTtlMs) || capabilityTtlMs <= 0) {
    throw new RangeError('capabilityTtlMs must be a positive safe integer')
  }

  const refreshing = ref(false)
  const lastSuccessAt = ref<number | null>(null)
  const refreshError = ref<AdminError | null>(null)
  let epoch = 0
  let active: { epoch: number; controller: AbortController; promise: Promise<void> } | null = null
  let disposed = false
  const cleanup: Array<() => void> = []

  function currentMeta(): AdminMeta | null {
    return state.value.kind === 'ready' ? state.value.meta : null
  }

  function load({ force, supersede }: { force: boolean; supersede: boolean }): Promise<void> {
    if (!isProduction) {
      state.value = { kind: 'unsupported' }
      return Promise.resolve()
    }
    if (typeof repository.read.getMeta !== 'function') {
      state.value = {
        kind: 'unavailable',
        error: {
          code: 'INTERNAL',
          message: 'Admin repository 不支持能力查询。',
          retryable: false,
          requestId: null,
        },
      }
      return Promise.resolve()
    }
    if (active && !supersede) return active.promise
    if (active) active.controller.abort()

    const controller = new AbortController()
    const localEpoch = ++epoch
    const hadLastSuccess = currentMeta() !== null
    if (!hadLastSuccess) state.value = { kind: 'loading' }
    else refreshing.value = true
    refreshError.value = null

    const promise = (async () => {
      try {
        const meta = await repository.read.getMeta?.({ signal: controller.signal, force })
        if (!meta || controller.signal.aborted || disposed || localEpoch !== epoch) return
        state.value = { kind: 'ready', meta }
        lastSuccessAt.value = now()
      } catch (error) {
        if (controller.signal.aborted || disposed || localEpoch !== epoch) return
        const mapped = toAdminError(error, {
          code: 'INTERNAL',
          message: '读取 Admin 能力失败。',
          retryable: true,
          requestId: null,
        })
        if (hadLastSuccess) {
          // Preserve the last known good receipt and surface its age through stale.
          refreshError.value = mapped
        } else {
          state.value = { kind: 'unavailable', error: mapped }
        }
      } finally {
        if (localEpoch === epoch) refreshing.value = false
      }
    })()
    active = { epoch: localEpoch, controller, promise }
    void promise.finally(() => {
      if (active?.epoch === localEpoch) active = null
    })
    return promise
  }

  const meta = computed(() => currentMeta())
  const error = computed(() =>
    state.value.kind === 'unavailable' ? state.value.error : refreshError.value,
  )
  const loading = computed(() => state.value.kind === 'loading')
  const stale = computed(() => {
    if (meta.value === null || lastSuccessAt.value === null)
      return state.value.kind === 'unavailable'
    return refreshError.value !== null || now() - lastSuccessAt.value >= capabilityTtlMs
  })
  const mode = computed<AdminSessionMode | 'unsupported' | 'loading'>(() => {
    const current = state.value
    if (current.kind === 'unsupported') return 'unsupported'
    if (current.kind === 'loading') return 'loading'
    if (current.kind === 'unavailable') return 'unavailable'
    const capabilities = current.meta.capabilities
    return MUTATION_CAPABILITIES.some((capability) => capabilities.includes(capability))
      ? MUTATION_CAPABILITIES.every((capability) => capabilities.includes(capability))
        ? 'operator'
        : 'partial'
      : 'read-only'
  })
  const isReadOnly = computed(() => mode.value === 'read-only' || mode.value === 'unavailable')
  const readOnlyReason = computed<string | null>(() => {
    if (state.value.kind === 'unsupported') return '当前数据源为 Mock，不存在能力限制。'
    if (state.value.kind === 'loading') return '正在读取 Relay 声明的能力，所有写操作将暂时不可用。'
    if (state.value.kind === 'unavailable') {
      return `无法读取 Relay 能力，所有写操作将按只读模式运行：${state.value.error.message}`
    }
    if (mode.value === 'read-only') {
      return `当前账号或 Relay 版本未授予任何写操作能力（声明：${state.value.meta.capabilities.join('、') || '（无）'}）。`
    }
    return null
  })

  function capabilityReason(capability: AdminCapability): string | null {
    if (
      state.value.kind === 'unsupported' ||
      (state.value.kind === 'ready' && state.value.meta.capabilities.includes(capability))
    ) {
      return null
    }
    if (state.value.kind === 'loading') return '正在读取能力，完成前暂不可用。'
    if (state.value.kind === 'unavailable')
      return `能力读取失败，暂不可用：${state.value.error.message}`
    return `当前账号未授予“${CAPABILITY_LABELS[capability]}”能力。`
  }

  function can(capability: AdminCapability): boolean {
    if (state.value.kind === 'unsupported') return true
    return state.value.kind === 'ready' && state.value.meta.capabilities.includes(capability)
  }

  function refreshIfStale(): Promise<void> {
    if (!stale.value && state.value.kind === 'ready') return Promise.resolve()
    return load({ force: true, supersede: false })
  }

  function registerLifecycleRefreshes(): void {
    if (!isProduction) return
    const eventTarget =
      options.eventTarget ?? (typeof window === 'undefined' ? undefined : (window as EventTarget))
    const visibilitySource =
      options.visibilitySource ?? (typeof document === 'undefined' ? undefined : document)
    const refreshOnActivity = () => void refreshIfStale()
    eventTarget?.addEventListener('focus', refreshOnActivity)
    eventTarget?.addEventListener('online', refreshOnActivity)
    if (eventTarget) {
      cleanup.push(() => eventTarget.removeEventListener('focus', refreshOnActivity))
      cleanup.push(() => eventTarget.removeEventListener('online', refreshOnActivity))
    }
    if (visibilitySource) {
      const refreshOnVisible = () => {
        if (visibilitySource.visibilityState === 'visible') refreshOnActivity()
      }
      visibilitySource.addEventListener('visibilitychange', refreshOnVisible)
      cleanup.push(() => visibilitySource.removeEventListener('visibilitychange', refreshOnVisible))
    }
    if (repository.capabilityMetadata) {
      cleanup.push(
        repository.capabilityMetadata.subscribeInvalidation(() => {
          void load({ force: true, supersede: true })
        }),
      )
    }
    const setInterval = options.setInterval ?? globalThis.setInterval.bind(globalThis)
    const clearInterval = options.clearInterval ?? globalThis.clearInterval.bind(globalThis)
    const timer = setInterval(() => void refreshIfStale(), capabilityTtlMs)
    cleanup.push(() => clearInterval(timer))
  }

  function dispose(): void {
    if (disposed) return
    disposed = true
    epoch += 1
    active?.controller.abort()
    for (const release of cleanup.splice(0)) release()
  }

  registerLifecycleRefreshes()
  void load({ force: false, supersede: false })
  if (getCurrentScope()) onScopeDispose(dispose)

  return {
    state: computed(() => state.value),
    meta,
    error,
    loading,
    refreshing: computed(() => refreshing.value),
    lastSuccessAt: computed(() => lastSuccessAt.value),
    stale,
    mode,
    isReadOnly,
    readOnlyReason,
    capabilityReason,
    can,
    refresh: () => load({ force: true, supersede: true }),
    dispose,
  }
}

export function provideAdminSession(session: AdminSessionContext): void {
  provide(AdminSessionKey, session)
}

export function useAdminSession(): AdminSessionContext {
  const existing = inject<AdminSessionContext | null>(AdminSessionKey, null)
  if (existing) return existing
  const created = createAdminSession()
  provideAdminSession(created)
  return created
}
