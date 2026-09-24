import { computed, onBeforeUnmount, ref, type ComputedRef, type Ref } from 'vue'

import { toAdminError, type AdminError } from '@/contracts/error'

/**
 * The phase a resource is in.  `stale` is only ever entered after at least
 * one successful read; it means the data on screen is from the last success
 * but the next refresh has either failed or has not been attempted yet within
 * the freshness window.
 */
export type AsyncResourcePhase =
  | 'idle'
  | 'initial-loading'
  | 'ready'
  | 'refreshing'
  | 'stale'
  | 'error'

export interface ResourceRunResult {
  /**
   * True when the request completed and produced a value that is now in
   * `data`.  False for aborted, errored, or suppressed (overwritten by a
   * newer request) outcomes.
   */
  ok: boolean
  /** True when this attempt was aborted by a newer request or unmount. */
  aborted: boolean
  /** Populated when ok=false and aborted=false. */
  error: AdminError | null
  /** Timestamp captured when the run settled, regardless of outcome. */
  completedAt: number
}

export interface AsyncResource<T> {
  data: Ref<T | null>
  loading: Ref<boolean>
  refreshing: Ref<boolean>
  error: Ref<AdminError | null>
  /**
   * Latest known phase.  Derived from `data` / `loading` / `refreshing` /
   * `error` so consumers can drive UIs from a single source of truth.
   */
  phase: ComputedRef<AsyncResourcePhase>
  /** Last attempt completion timestamp (success or error).  Null until first run. */
  lastAttemptAt: Ref<number | null>
  /** Last successful completion timestamp.  Null until first success. */
  lastSuccessAt: Ref<number | null>
  /**
   * True when we have data but the lastSuccessAt is older than the supplied
   * threshold (ms).  Useful for "this may be out of date" hints.
   */
  isStale: (thresholdMs: number, now?: number) => boolean
  /**
   * Force a refresh.  The returned result reflects THIS attempt only; a
   * newer attempt may subsequently replace it.  Callers that need to count
   * succeeded vs. failed partitions must inspect the result, never the
   * resolved/rejected status of the promise.
   */
  refresh: (mode?: 'initial' | 'refresh') => Promise<ResourceRunResult>
}

interface UseAsyncResourceOptions<T> {
  fetcher: (signal: AbortSignal) => Promise<T>
  immediate?: boolean
}

export function useAsyncResource<T>(options: UseAsyncResourceOptions<T>): AsyncResource<T> {
  const data = ref<T | null>(null) as Ref<T | null>
  const loading = ref(false)
  const refreshing = ref(false)
  const error = ref<AdminError | null>(null) as Ref<AdminError | null>
  const lastAttemptAt = ref<number | null>(null) as Ref<number | null>
  const lastSuccessAt = ref<number | null>(null) as Ref<number | null>

  let controller: AbortController | null = null
  let requestId = 0

  async function run(mode: 'initial' | 'refresh' = 'initial'): Promise<ResourceRunResult> {
    if (controller) {
      controller.abort()
    }
    const localController = new AbortController()
    controller = localController
    const localId = ++requestId
    const hasData = data.value !== null
    if (mode === 'initial' && !hasData) {
      loading.value = true
    } else {
      refreshing.value = true
    }
    error.value = null
    let result: ResourceRunResult
    try {
      const value = await options.fetcher(localController.signal)
      if (localId !== requestId) {
        // A newer attempt has started; do not touch shared state.  Surface a
        // synthetic aborted result so callers that chain on this attempt see
        // it as a no-op, not as a success.
        return {
          ok: false,
          aborted: true,
          error: null,
          completedAt: Date.now(),
        }
      }
      data.value = value
      const completedAt = Date.now()
      lastAttemptAt.value = completedAt
      lastSuccessAt.value = completedAt
      result = { ok: true, aborted: false, error: null, completedAt }
    } catch (err) {
      if (localId !== requestId) {
        return {
          ok: false,
          aborted: true,
          error: null,
          completedAt: Date.now(),
        }
      }
      if (err instanceof DOMException && err.name === 'AbortError') {
        result = {
          ok: false,
          aborted: true,
          error: null,
          completedAt: Date.now(),
        }
      } else {
        const mapped = toAdminError(err, {
          code: 'INTERNAL',
          message: err instanceof Error ? err.message : '请求失败，请稍后重试。',
          retryable: false,
          requestId: null,
        })
        error.value = mapped
        lastAttemptAt.value = Date.now()
        result = { ok: false, aborted: false, error: mapped, completedAt: Date.now() }
      }
    } finally {
      if (localId === requestId) {
        loading.value = false
        refreshing.value = false
      }
    }
    return result
  }

  if (options.immediate !== false) {
    void run('initial')
  }

  const phase = computed<AsyncResourcePhase>(() => {
    if (loading.value) return 'initial-loading'
    if (refreshing.value) return 'refreshing'
    if (error.value !== null) return data.value === null ? 'error' : 'stale'
    if (data.value === null) return 'idle'
    return 'ready'
  })

  function isStale(thresholdMs: number, now: number = Date.now()): boolean {
    if (data.value === null) return false
    if (lastSuccessAt.value === null) return false
    return now - lastSuccessAt.value > thresholdMs
  }

  onBeforeUnmount(() => {
    if (controller) {
      controller.abort()
    }
  })

  return {
    data,
    loading,
    refreshing,
    error,
    phase,
    lastAttemptAt,
    lastSuccessAt,
    isStale,
    refresh: run,
  }
}
