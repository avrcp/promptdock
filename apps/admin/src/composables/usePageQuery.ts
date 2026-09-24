import { computed, reactive, watch, type ComputedRef } from 'vue'
import { useRoute, useRouter } from 'vue-router'

export function usePageQuery(initial: Record<string, string | null> = {}): {
  values: Record<string, string | null>
  set: (key: string, value: string | null, mode?: 'replace' | 'push') => void
  reset: () => void
} {
  const route = useRoute()
  const router = useRouter()

  const values = reactive<Record<string, string | null>>({ ...initial })

  function applyFromQuery(): void {
    const query = route.query
    for (const key of Object.keys(initial)) {
      const raw = query[key]
      values[key] = typeof raw === 'string' && raw.length > 0 ? raw : null
    }
  }

  applyFromQuery()

  watch(
    () => route.query,
    () => {
      applyFromQuery()
    },
  )

  function set(key: string, value: string | null, mode: 'replace' | 'push' = 'replace'): void {
    values[key] = value
    const next: Record<string, string> = {}
    for (const [k, v] of Object.entries(route.query)) {
      if (typeof v === 'string' && !Object.prototype.hasOwnProperty.call(initial, k)) {
        next[k] = v
      }
    }
    for (const [k, v] of Object.entries(values)) {
      if (v !== null && v !== '') {
        next[k] = v
      }
    }
    void (mode === 'push' ? router.push({ query: next }) : router.replace({ query: next }))
  }

  function reset(): void {
    for (const key of Object.keys(initial)) {
      values[key] = null
    }
    const next: Record<string, string> = {}
    for (const [k, v] of Object.entries(route.query)) {
      if (typeof v === 'string' && !Object.prototype.hasOwnProperty.call(initial, k)) {
        next[k] = v
      }
    }
    void router.replace({ query: next })
  }

  return {
    values,
    set,
    reset,
  }
}

export function useQueryString(values: Record<string, string | null>): ComputedRef<string> {
  return computed(() => {
    const params = new URLSearchParams()
    for (const [k, v] of Object.entries(values)) {
      if (v !== null && v !== '') {
        params.set(k, v)
      }
    }
    const result = params.toString()
    return result.length > 0 ? `?${result}` : ''
  })
}
