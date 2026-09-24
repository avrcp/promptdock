import { describe, expect, it, vi } from 'vitest'

import { AdminCapabilityMetadata } from './admin-capability-metadata'
import { HttpClient } from './http-client'

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

const meta = {
  schemaVersion: 2,
  generatedAt: 1,
  adminApiVersion: 2,
  relayVersion: 'test',
  capabilities: ['admin_read_v2'],
}

describe('AdminCapabilityMetadata', () => {
  it('shares one in-flight request, caches only for its TTL, and force-refreshes stale metadata', async () => {
    let now = 100
    const pending: Array<(response: Response) => void> = []
    const fetcher = vi.fn(
      () =>
        new Promise<Response>((resolve) => {
          pending.push(resolve)
        }),
    )
    const store = new AdminCapabilityMetadata({
      client: new HttpClient({ fetch: fetcher }),
      ttlMs: 60_000,
      now: () => now,
    })

    const first = store.get()
    const concurrent = store.get()
    expect(fetcher).toHaveBeenCalledTimes(1)
    pending.shift()?.(json(meta))
    await expect(first).resolves.toEqual(meta)
    await expect(concurrent).resolves.toEqual(meta)
    await expect(store.get()).resolves.toEqual(meta)
    expect(fetcher).toHaveBeenCalledTimes(1)

    now += 60_000
    const refreshed = store.get()
    expect(fetcher).toHaveBeenCalledTimes(2)
    pending.shift()?.(json({ ...meta, relayVersion: 'next' }))
    await expect(refreshed).resolves.toMatchObject({ relayVersion: 'next' })
  })

  it('invalidates and notifies after a non-meta authorization failure without looping for /meta', async () => {
    const responses = [json(meta), json({ code: 'DENIED' }, 403), json({ code: 'DENIED' }, 403)]
    const client = new HttpClient({ fetch: vi.fn().mockImplementation(() => responses.shift()!) })
    const store = new AdminCapabilityMetadata({ client })
    const invalidated = vi.fn()
    store.subscribeInvalidation(invalidated)

    await store.get()
    await expect(client.request({ path: '/wechat/login/session-1' })).rejects.toMatchObject({
      status: 403,
    })
    expect(store.isStale()).toBe(true)
    expect(invalidated).toHaveBeenCalledTimes(1)

    await expect(client.request({ path: '/meta' })).rejects.toMatchObject({ status: 403 })
    expect(invalidated).toHaveBeenCalledTimes(1)
  })
})
