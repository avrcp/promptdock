import { describe, expect, it, vi } from 'vitest'
import { z } from 'zod'

import { HttpClient, HttpClientError } from './http-client'

function jsonResponse(body: unknown, status = 200, headers: Record<string, string> = {}) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  })
}

describe('HttpClient', () => {
  it('uses a relative same-origin request with no-store and no retries', async () => {
    const fetcher = vi.fn().mockResolvedValue(jsonResponse({ ok: true }))
    const signal = new AbortController().signal
    const client = new HttpClient({ fetch: fetcher })

    const response = await client.request({
      path: '/overview',
      query: { cursor: 'next page' },
      signal,
    })

    expect(response.data).toEqual({ ok: true })
    expect(fetcher).toHaveBeenCalledTimes(1)
    expect(fetcher).toHaveBeenCalledWith('/admin/api/v2/overview?cursor=next+page', {
      method: 'GET',
      credentials: 'same-origin',
      cache: 'no-store',
      signal,
      headers: expect.any(Headers),
    })
  })

  it('adds JSON and action headers for writes', async () => {
    const fetcher = vi.fn().mockResolvedValue(jsonResponse({ accepted: true }))
    const client = new HttpClient({ fetch: fetcher })

    await client.request({ path: '/devices', method: 'POST', body: { name: 'demo' } })

    const init = fetcher.mock.calls[0]?.[1] as RequestInit
    const headers = new Headers(init.headers)
    expect(headers.get('Content-Type')).toBe('application/json')
    expect(headers.get('X-PromptDock-Admin-Action')).toBe('1')
    expect(init.body).toBe('{"name":"demo"}')
  })

  it('rejects non-JSON responses and caps response bytes', async () => {
    const nonJson = vi.fn().mockResolvedValue(new Response('<html>bad</html>', { status: 502 }))
    await expect(
      new HttpClient({ fetch: nonJson }).request({ path: '/overview' }),
    ).rejects.toMatchObject({
      code: 'INVALID_CONTENT_TYPE',
      status: 502,
    })

    const oversized = vi.fn().mockResolvedValue(jsonResponse({ value: '123456789' }))
    await expect(
      new HttpClient({ fetch: oversized, maxResponseBytes: 8 }).request({ path: '/overview' }),
    ).rejects.toMatchObject({ code: 'RESPONSE_TOO_LARGE', status: 200 })
  })

  it('maps safe HTTP error fields and Retry-After without exposing other payload data', async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValue(
        jsonResponse(
          { code: 'RELAY_BUSY', message: '暂时不可用', requestId: 'req-1', secret: 'do-not-store' },
          429,
          { 'Retry-After': '3' },
        ),
      )
    const error = await new HttpClient({ fetch: fetcher })
      .request({ path: '/overview' })
      .catch((value) => value)

    expect(error).toBeInstanceOf(HttpClientError)
    expect(error).toMatchObject({
      code: 'HTTP_ERROR',
      status: 429,
      requestId: 'req-1',
      responseCode: 'RELAY_BUSY',
      retryAfterSeconds: 3,
      retryable: true,
      message: '暂时不可用',
    })
    expect(JSON.stringify(error)).not.toContain('do-not-store')
  })

  it('does not convert AbortError into an ordinary failure', async () => {
    const abort = new DOMException('aborted', 'AbortError')
    const fetcher = vi.fn().mockRejectedValue(abort)
    await expect(new HttpClient({ fetch: fetcher }).request({ path: '/overview' })).rejects.toBe(
      abort,
    )
  })

  it('rejects absolute and backslash-normalized URLs', async () => {
    const fetcher = vi.fn()
    const client = new HttpClient({ fetch: fetcher })
    await expect(client.request({ path: 'https://example.test/steal' })).rejects.toThrow(
      'relative same-origin',
    )
    await expect(client.request({ path: '\\example.test\\steal' })).rejects.toThrow(
      'relative same-origin',
    )
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('rejects traversal, fragments, control characters, and recursively encoded traversal paths', async () => {
    const fetcher = vi.fn()
    const client = new HttpClient({ fetch: fetcher })
    for (const path of [
      '/../secret',
      '/%2e%2e/secret',
      '/%252e%252e/secret',
      '/%5cadmin',
      '/overview#fragment',
      '/overview\u0000',
    ]) {
      await expect(client.request({ path })).rejects.toThrow('forbidden')
    }
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('combines a caller abort signal with an injectable default deadline', async () => {
    vi.useFakeTimers()
    try {
      const fetcher = vi.fn(
        (_input: RequestInfo | URL, init?: RequestInit) =>
          new Promise<Response>((_resolve, reject) => {
            init?.signal?.addEventListener(
              'abort',
              () => reject(new DOMException('aborted', 'AbortError')),
              { once: true },
            )
          }),
      )
      const client = new HttpClient({ fetch: fetcher, deadlines: { read: 10 } })
      const pending = client.request({ path: '/overview' })
      const timedOut = expect(pending).rejects.toMatchObject({
        code: 'REQUEST_TIMEOUT',
        retryable: false,
      })
      await vi.advanceTimersByTimeAsync(10)
      await timedOut

      const controller = new AbortController()
      const callerAbort = client.request({ path: '/overview', signal: controller.signal })
      const aborted = expect(callerAbort).rejects.toMatchObject({ name: 'AbortError' })
      controller.abort()
      await aborted
    } finally {
      vi.useRealTimers()
    }
  })

  it('uses deliberate deadlines for reads, mutations, login start/verify, and login polling', async () => {
    const deadlines: number[] = []
    const client = new HttpClient({
      fetch: vi.fn().mockResolvedValue(jsonResponse({ ok: true })),
      setTimeout: (_handler, timeoutMs) => {
        deadlines.push(timeoutMs)
        return 1 as unknown as ReturnType<typeof setTimeout>
      },
      clearTimeout: vi.fn(),
    })

    await client.request({ path: '/overview' })
    await client.request({ path: '/devices', method: 'POST', body: {} })
    await client.request({
      path: '/wechat/login',
      method: 'POST',
      body: {},
      purpose: 'login-start',
    })
    await client.request({
      path: '/wechat/login/session-1/verify',
      method: 'POST',
      body: {},
      purpose: 'login-verify',
    })
    await client.request({ path: '/wechat/login/session-1', purpose: 'login-poll' })

    expect(deadlines).toEqual([15_000, 30_000, 15_000, 15_000, 10_000])
  })

  it('caps declared Content-Length before consuming a body', async () => {
    const fetcher = vi.fn().mockResolvedValue(
      new Response('{}', {
        headers: { 'Content-Type': 'application/json', 'Content-Length': '999' },
      }),
    )
    await expect(
      new HttpClient({ fetch: fetcher, maxResponseBytes: 8 }).request({ path: '/overview' }),
    ).rejects.toMatchObject({ code: 'RESPONSE_TOO_LARGE', retryable: false })
  })

  it('turns strict schema failures into a safe contract mismatch without retaining payload data', async () => {
    const client = new HttpClient({
      fetch: vi.fn().mockResolvedValue(
        jsonResponse({ status: { id: 42, secret: 'must-not-appear' } }, 200, {
          'X-Request-Id': 'req-contract',
        }),
      ),
    })
    const error = await client
      .request({ path: '/overview', schema: z.object({ status: z.object({ id: z.string() }) }) })
      .catch((value) => value)

    expect(error).toMatchObject({
      code: 'ADMIN_CONTRACT_MISMATCH',
      requestId: 'req-contract',
      contractPath: 'status.id',
      retryable: false,
    })
    expect(JSON.stringify(error)).not.toContain('must-not-appear')
  })

  it('notifies capability lifecycle subscribers for non-meta 401/403 responses only', async () => {
    const client = new HttpClient({
      fetch: vi.fn().mockResolvedValue(jsonResponse({ code: 'DENIED' }, 403)),
    })
    const listener = vi.fn()
    client.onAuthorizationFailure(listener)

    await expect(client.request({ path: '/wechat/login/session-1' })).rejects.toMatchObject({
      status: 403,
    })
    await expect(client.request({ path: '/meta' })).rejects.toMatchObject({ status: 403 })
    expect(listener).toHaveBeenCalledTimes(1)
    expect(listener).toHaveBeenCalledWith({ path: '/wechat/login/session-1', status: 403 })
  })
})
