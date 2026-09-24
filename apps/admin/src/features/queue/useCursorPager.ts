import { computed, ref, type ComputedRef, type Ref } from 'vue'

import type { AdminError } from '@/contracts/error'

export interface CursorPager {
  cursor: Ref<string | null>
  history: Ref<readonly (string | null)[]>
  page: ComputedRef<number>
  hasPrevious: ComputedRef<boolean>
  reset: () => void
  recoverFromCursorError: (error: AdminError | null) => boolean
  next: (cursor: string | null) => boolean
  previous: () => boolean
}

/**
 * Opaque cursor navigation. Cursors never reach the UI or the URL and every
 * filter change resets the complete history through `reset`.
 */
export function useCursorPager(): CursorPager {
  const cursor = ref<string | null>(null)
  const history = ref<readonly (string | null)[]>([])

  function reset(): void {
    cursor.value = null
    history.value = []
  }

  /**
   * Relay cursors are intentionally short-lived opaque positions.  Only
   * recover when a later page was active: a cursor error on page one must be
   * surfaced instead of repeatedly reloading the same request.
   */
  function recoverFromCursorError(error: AdminError | null): boolean {
    if (
      cursor.value === null ||
      (error?.code !== 'CURSOR_INVALID' && error?.code !== 'CURSOR_EXPIRED')
    ) {
      return false
    }
    reset()
    return true
  }

  function next(nextCursor: string | null): boolean {
    if (!nextCursor) return false
    history.value = [...history.value, cursor.value]
    cursor.value = nextCursor
    return true
  }

  function previous(): boolean {
    if (history.value.length === 0) return false
    const previousHistory = [...history.value]
    cursor.value = previousHistory.pop() ?? null
    history.value = previousHistory
    return true
  }

  return {
    cursor,
    history,
    page: computed(() => history.value.length + 1),
    hasPrevious: computed(() => history.value.length > 0),
    reset,
    recoverFromCursorError,
    next,
    previous,
  }
}
