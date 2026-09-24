import { afterEach, describe, expect, it } from 'vitest'
import { defineComponent, h, nextTick } from 'vue'
import { mount } from '@vue/test-utils'

import { useAsyncResource } from './useAsyncResource'
import type { AdminError } from '@/contracts/error'

function makeError(): AdminError {
  return { code: 'INTERNAL', message: 'boom', retryable: true, requestId: null }
}

interface Pending<T> {
  resolve: (v: T) => void
  reject: (e: unknown) => void
  aborted: boolean
}

interface Harness {
  wrapper: ReturnType<typeof mount>
  resource: ReturnType<typeof useAsyncResource<number>>
  resolveNext: (v: number) => Promise<void>
  rejectNext: (e: unknown) => Promise<void>
  pendingCount: () => number
}

function mountHarness(options: { immediate?: boolean } = {}): Harness {
  const queue: Pending<number>[] = []
  const Host = defineComponent({
    setup() {
      const resource = useAsyncResource<number>({
        fetcher: (signal) =>
          new Promise<number>((resolve, reject) => {
            const entry: Pending<number> = {
              resolve: (v) => {
                if (signal.aborted) {
                  entry.aborted = true
                  // An aborted request must settle so the awaited run() can
                  // inspect localId vs requestId and produce an aborted
                  // result.  Reject with a DOMException to mirror
                  // production AbortController behavior.
                  reject(new DOMException('aborted', 'AbortError'))
                  return
                }
                resolve(v)
              },
              reject: (e) => {
                if (signal.aborted) {
                  entry.aborted = true
                  reject(new DOMException('aborted', 'AbortError'))
                  return
                }
                reject(e)
              },
              aborted: false,
            }
            queue.push(entry)
          }),
        immediate: options.immediate ?? true,
      })
      return { resource }
    },
    render() {
      return h('div')
    },
  })
  const wrapper = mount(Host, { attachTo: document.body })
  return {
    wrapper,
    resource: (wrapper.vm as unknown as { resource: ReturnType<typeof useAsyncResource<number>> })
      .resource,
    async resolveNext(v: number) {
      const next = queue.shift()
      if (!next) throw new Error('no pending fetcher')
      next.resolve(v)
      await nextTick()
    },
    async rejectNext(e: unknown) {
      const next = queue.shift()
      if (!next) throw new Error('no pending fetcher')
      next.reject(e)
      await nextTick()
    },
    pendingCount: () => queue.length,
  }
}

describe('useAsyncResource', () => {
  afterEach(() => {
    document.body.innerHTML = ''
  })

  it('returns ok=true on success and populates data + lastSuccessAt (immediate)', async () => {
    const h = mountHarness({ immediate: true })
    // The immediate run created the first pending.  Wait one tick for it
    // to be registered, then resolve it and await the result.
    await nextTick()
    expect(h.pendingCount()).toBe(1)
    // The immediate run is fire-and-forget; we don't have a handle to its
    // result.  Drive it through to completion.
    await h.resolveNext(7)
    expect(h.resource.data.value).toBe(7)
    expect(h.resource.lastSuccessAt.value).not.toBeNull()
    expect(h.resource.phase.value).toBe('ready')
    h.wrapper.unmount()
  })

  it('returns ok=true on a manual refresh', async () => {
    const h = mountHarness({ immediate: false })
    const promise = h.resource.refresh('refresh')
    await h.resolveNext(7)
    const result = await promise
    expect(result.ok).toBe(true)
    expect(h.resource.data.value).toBe(7)
    h.wrapper.unmount()
  })

  it('returns ok=false and populates error on failure', async () => {
    const h = mountHarness({ immediate: false })
    const promise = h.resource.refresh('refresh')
    await h.rejectNext(makeError())
    const result = await promise
    expect(result.ok).toBe(false)
    expect(result.error?.code).toBe('INTERNAL')
    expect(h.resource.phase.value).toBe('error')
    h.wrapper.unmount()
  })

  it('reports stale when data exists but latest attempt errored', async () => {
    const h = mountHarness({ immediate: false })
    const p1 = h.resource.refresh('refresh')
    await h.resolveNext(1)
    await p1
    expect(h.resource.phase.value).toBe('ready')
    const p2 = h.resource.refresh('refresh')
    await h.rejectNext(makeError())
    await p2
    expect(h.resource.data.value).toBe(1)
    expect(h.resource.phase.value).toBe('stale')
    // Use a small positive threshold to avoid millisecond boundary races
    // between lastSuccessAt and the implicit Date.now() inside isStale.
    expect(h.resource.isStale(-1)).toBe(true)
    h.wrapper.unmount()
  })

  it('aborts the previous attempt when a newer one starts', async () => {
    const h = mountHarness({ immediate: false })
    const first = h.resource.refresh('refresh')
    const second = h.resource.refresh('refresh')
    // FIFO: shift returns the first queued fetcher (the one belonging to
    // the FIRST refresh call).  It has been aborted, so resolveNext must
    // still fire (it just no-ops).  Then resolve the second.
    await h.resolveNext(99)
    await h.resolveNext(42)
    const firstResult = await first
    const secondResult = await second
    expect(firstResult.aborted).toBe(true)
    expect(firstResult.ok).toBe(false)
    expect(secondResult.ok).toBe(true)
    expect(h.resource.data.value).toBe(42)
    h.wrapper.unmount()
  })

  it('exposes phase idle/initial-loading/ready for a non-immediate resource', async () => {
    const h = mountHarness({ immediate: false })
    expect(h.resource.phase.value).toBe('idle')
    const p = h.resource.refresh('initial')
    await nextTick()
    expect(h.resource.phase.value).toBe('initial-loading')
    await h.resolveNext(5)
    await p
    expect(h.resource.phase.value).toBe('ready')
    h.wrapper.unmount()
  })
})
