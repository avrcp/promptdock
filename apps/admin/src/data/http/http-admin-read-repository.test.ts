import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

import { describe, expect, it, vi } from 'vitest'

import { HttpClient } from './http-client'
import { HttpAdminReadRepository } from './http-admin-read-repository'

const fixtureRoot = resolve(process.cwd(), '../../contracts/admin-api/v2/fixtures')

function fixture(name: string): Response {
  return new Response(readFileSync(resolve(fixtureRoot, name), 'utf8'), {
    headers: { 'Content-Type': 'application/json' },
  })
}

describe('HttpAdminReadRepository', () => {
  it('preflights meta once and reads all ten GET endpoints through the same-origin client', async () => {
    const calls: string[] = []
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      calls.push(url)
      const filename = url.endsWith('/meta')
        ? 'meta-v2.json'
        : url.endsWith('/overview')
          ? 'overview-v2.json'
          : url.includes('/devices/')
            ? 'device-detail-v2.json'
            : url.includes('/devices?')
              ? 'devices-page-v2.json'
              : url.endsWith('/devices')
                ? 'devices-page-v2.json'
                : url.endsWith('/wechat/status')
                  ? 'wechat-status-v2.json'
                  : url.includes('/wechat/events')
                    ? 'wechat-events-page-v2.json'
                    : url.includes('/deliveries')
                      ? 'deliveries-page-v2.json'
                      : url.includes('/interactive-replies')
                        ? 'interactive-replies-page-v2.json'
                        : url.includes('/inbound-commands')
                          ? 'inbound-commands-page-v2.json'
                          : 'system-v2.json'
      return fixture(filename)
    })
    const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })

    await repository.getMeta()
    await repository.getOverview()
    await repository.listDevices({ limit: 100, cursor: null })
    await repository.getDevice('00000000-0000-4000-8000-000000000001')
    await repository.getWechatStatus()
    await repository.listChannelEvents({ limit: 100, kinds: ['poll', 'test'] })
    await repository.listDeliveries({ limit: 100 })
    await repository.listInteractiveReplies({ limit: 100 })
    await repository.listInboundCommands({ limit: 100 })
    await repository.getSystemSnapshot()

    expect(fetcher).toHaveBeenCalledTimes(10)
    expect(calls).toEqual([
      '/admin/api/v2/meta',
      '/admin/api/v2/overview',
      '/admin/api/v2/devices?limit=100',
      '/admin/api/v2/devices/00000000-0000-4000-8000-000000000001',
      '/admin/api/v2/wechat/status',
      '/admin/api/v2/wechat/events?kinds=poll&kinds=test&limit=100',
      '/admin/api/v2/deliveries?limit=100',
      '/admin/api/v2/interactive-replies?limit=100',
      '/admin/api/v2/inbound-commands?limit=100',
      '/admin/api/v2/system',
    ])
  })

  it('rejects unknown filters and limits above the server maximum before making a request', async () => {
    const fetcher = vi.fn().mockResolvedValue(fixture('meta-v2.json'))
    const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })

    await expect(repository.listDevices({ limit: 101, cursor: null })).rejects.toMatchObject({
      adminError: { code: 'INVALID_INPUT' },
    })
    await expect(repository.listDevices({ state: 'unknown' } as never)).rejects.toMatchObject({
      adminError: { code: 'INVALID_INPUT' },
    })
    await expect(
      repository.listDeliveries({ limit: 50, cursor: null, unexpected: 'x' } as never),
    ).rejects.toMatchObject({ adminError: { code: 'INVALID_INPUT' } })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('maps HTTP statuses to safe domain errors and preserves request metadata', async () => {
    for (const status of [404, 409, 429, 503]) {
      const fetcher = vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            code: 'ADMIN_STATUS',
            message: 'safe failure',
            requestId: `req-${status}`,
          }),
          {
            status,
            headers: { 'Content-Type': 'application/json', 'X-Request-Id': `header-${status}` },
          },
        ),
      )
      const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })
      await expect(repository.getMeta()).rejects.toMatchObject({
        adminError: {
          message: 'safe failure',
          requestId: `header-${status}`,
        },
      })
    }
  })

  it('discovers and reuses the exact lower Relay page-size ceiling', async () => {
    const calls: string[] = []
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      calls.push(url)
      if (url.endsWith('/meta')) return fixture('meta-v2.json')
      const limit = Number(new URL(url, 'https://admin.local').searchParams.get('limit'))
      if (limit > 12) {
        return new Response(
          JSON.stringify({
            code: 'ADMIN_VALIDATION_FAILED',
            message: 'request validation failed',
            requestId: 'req-page-size',
          }),
          { status: 400, headers: { 'Content-Type': 'application/json' } },
        )
      }
      return fixture(
        url.includes('/deliveries') ? 'deliveries-page-v2.json' : 'devices-page-v2.json',
      )
    })
    const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })

    await repository.listDevices({ limit: 50, cursor: null })
    await repository.listDeliveries({ limit: 50, cursor: null })

    expect(calls).toEqual([
      '/admin/api/v2/meta',
      '/admin/api/v2/devices?limit=50',
      '/admin/api/v2/devices?limit=25',
      '/admin/api/v2/devices?limit=12',
      '/admin/api/v2/devices?limit=18',
      '/admin/api/v2/devices?limit=15',
      '/admin/api/v2/devices?limit=13',
      '/admin/api/v2/deliveries?limit=12',
    ])
  })

  it('uses a known accepted page size before halving below it', async () => {
    const calls: string[] = []
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      calls.push(url)
      if (url.endsWith('/meta')) return fixture('meta-v2.json')
      const limit = Number(new URL(url, 'https://admin.local').searchParams.get('limit'))
      if (limit > 20) {
        return new Response(
          JSON.stringify({
            code: 'ADMIN_VALIDATION_FAILED',
            message: 'request validation failed',
            requestId: 'req-page-size',
          }),
          { status: 400, headers: { 'Content-Type': 'application/json' } },
        )
      }
      return fixture(
        url.includes('/deliveries') ? 'deliveries-page-v2.json' : 'devices-page-v2.json',
      )
    })
    const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })

    await repository.listDevices({ limit: 20, cursor: null })
    await repository.listDeliveries({ limit: 50, cursor: null })

    expect(calls).toEqual([
      '/admin/api/v2/meta',
      '/admin/api/v2/devices?limit=20',
      '/admin/api/v2/deliveries?limit=50',
      '/admin/api/v2/deliveries?limit=20',
      '/admin/api/v2/deliveries?limit=35',
      '/admin/api/v2/deliveries?limit=27',
      '/admin/api/v2/deliveries?limit=23',
      '/admin/api/v2/deliveries?limit=21',
    ])
  })

  it('invalidates cached page-size knowledge after Relay lowers its limit', async () => {
    const calls: string[] = []
    let relayLimit = 20
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      calls.push(url)
      if (url.endsWith('/meta')) return fixture('meta-v2.json')
      const limit = Number(new URL(url, 'https://admin.local').searchParams.get('limit'))
      if (limit > relayLimit) {
        return new Response(
          JSON.stringify({
            code: 'ADMIN_VALIDATION_FAILED',
            message: 'request validation failed',
            requestId: 'req-page-size',
          }),
          { status: 400, headers: { 'Content-Type': 'application/json' } },
        )
      }
      return fixture('devices-page-v2.json')
    })
    const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })

    await repository.listDevices({ limit: 20, cursor: null })
    relayLimit = 10
    await repository.listDevices({ search: 'relay', limit: 20, cursor: null })

    expect(calls).toEqual([
      '/admin/api/v2/meta',
      '/admin/api/v2/devices?limit=20',
      '/admin/api/v2/devices?search=relay&limit=20',
      '/admin/api/v2/devices?limit=20',
      '/admin/api/v2/devices?limit=10',
      '/admin/api/v2/devices?limit=15',
      '/admin/api/v2/devices?limit=12',
      '/admin/api/v2/devices?limit=11',
      '/admin/api/v2/devices?search=relay&limit=10',
    ])
  })

  it('rediscovers a higher Relay page-size ceiling after the knowledge TTL', async () => {
    const calls: string[] = []
    let relayLimit = 12
    let now = 0
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      calls.push(url)
      if (url.endsWith('/meta')) return fixture('meta-v2.json')
      const limit = Number(new URL(url, 'https://admin.local').searchParams.get('limit'))
      if (limit > relayLimit) {
        return new Response(
          JSON.stringify({
            code: 'ADMIN_VALIDATION_FAILED',
            message: 'request validation failed',
            requestId: 'req-page-size',
          }),
          { status: 400, headers: { 'Content-Type': 'application/json' } },
        )
      }
      return fixture(
        url.includes('/deliveries') ? 'deliveries-page-v2.json' : 'devices-page-v2.json',
      )
    })
    const repository = new HttpAdminReadRepository({
      client: new HttpClient({ fetch: fetcher }),
      pageSizeKnowledgeTtlMs: 10,
      now: () => now,
    })

    await repository.listDevices({ limit: 50, cursor: null })
    relayLimit = 20
    now = 10
    await repository.listDeliveries({ limit: 50, cursor: null })

    expect(calls.filter((url) => url.includes('/deliveries?'))).toEqual([
      '/admin/api/v2/deliveries?limit=50',
      '/admin/api/v2/deliveries?limit=25',
      '/admin/api/v2/deliveries?limit=12',
      '/admin/api/v2/deliveries?limit=18',
      '/admin/api/v2/deliveries?limit=21',
      '/admin/api/v2/deliveries?limit=19',
      '/admin/api/v2/deliveries?limit=20',
    ])
  })

  it('probes page size independently and does not retry a filtered validation failure', async () => {
    const calls: string[] = []
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      calls.push(url)
      if (url.endsWith('/meta')) return fixture('meta-v2.json')

      const requestUrl = new URL(url, 'https://admin.local')
      const limit = Number(requestUrl.searchParams.get('limit'))
      if (!requestUrl.searchParams.has('search') && limit > 12) {
        return new Response(
          JSON.stringify({
            code: 'ADMIN_VALIDATION_FAILED',
            message: 'request validation failed',
            requestId: 'req-page-size',
          }),
          { status: 400, headers: { 'Content-Type': 'application/json' } },
        )
      }
      if (requestUrl.searchParams.has('search')) {
        return new Response(
          JSON.stringify({
            code: 'ADMIN_VALIDATION_FAILED',
            message: 'filtered request validation failed',
            requestId: 'req-filter',
          }),
          { status: 400, headers: { 'Content-Type': 'application/json' } },
        )
      }
      return fixture('devices-page-v2.json')
    })
    const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })

    await expect(
      repository.listDevices({ search: 'invalid combination', limit: 50, cursor: null }),
    ).rejects.toMatchObject({ adminError: { requestId: 'req-filter' } })

    expect(calls).toEqual([
      '/admin/api/v2/meta',
      '/admin/api/v2/devices?limit=50',
      '/admin/api/v2/devices?limit=25',
      '/admin/api/v2/devices?limit=12',
      '/admin/api/v2/devices?limit=18',
      '/admin/api/v2/devices?limit=15',
      '/admin/api/v2/devices?limit=13',
      '/admin/api/v2/devices?search=invalid+combination&limit=12',
    ])
  })

  it.each([
    ['ADMIN_CURSOR_INVALID', 'CURSOR_INVALID'],
    ['ADMIN_CURSOR_EXPIRED', 'CURSOR_EXPIRED'],
  ] as const)('maps %s without retrying an opaque cursor request', async (responseCode, code) => {
    const calls: string[] = []
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      calls.push(url)
      if (url.endsWith('/meta')) return fixture('meta-v2.json')
      if (!new URL(url, 'https://admin.local').searchParams.has('cursor')) {
        return fixture('deliveries-page-v2.json')
      }
      return new Response(
        JSON.stringify({
          code: responseCode,
          message: 'cursor failure',
          requestId: 'req-cursor',
        }),
        { status: 400, headers: { 'Content-Type': 'application/json' } },
      )
    })
    const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })

    await expect(
      repository.listDeliveries({ limit: 50, cursor: 'opaque-cursor' }),
    ).rejects.toMatchObject({
      adminError: { code, retryable: true, requestId: 'req-cursor' },
    })
    expect(calls).toEqual([
      '/admin/api/v2/meta',
      '/admin/api/v2/deliveries?limit=50',
      '/admin/api/v2/deliveries?limit=50&cursor=opaque-cursor',
    ])
  })

  it('reproduces the Relay list_runs payload that Admin API v2 currently rejects', async () => {
    const relayListRunsPayload = {
      items: [
        {
          id: 'inbound_0123456789abcdef',
          command: 'list_runs',
          senderHint: 'wx:0123…cdef',
          state: 'reply_queued',
          attemptCount: 1,
          errorCode: null,
          createdAt: 1787651900000,
          expiresAt: 1787652500000,
          updatedAt: 1787651990000,
        },
      ],
      nextCursor: null,
      total: null,
      generatedAt: 1787652000000,
    }
    const fetcher = vi.fn(async (input: RequestInfo | URL) =>
      String(input).endsWith('/meta')
        ? fixture('meta-v2.json')
        : new Response(JSON.stringify(relayListRunsPayload), {
            headers: { 'Content-Type': 'application/json' },
          }),
    )
    const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })

    await expect(repository.listInboundCommands({ limit: 100 })).rejects.toMatchObject({
      adminError: {
        code: 'ADMIN_CONTRACT_MISMATCH',
        retryable: false,
      },
    })
    expect(fetcher).toHaveBeenCalledTimes(2)
  })

  it('passes AbortError through without converting it to a normal repository error', async () => {
    const abort = new DOMException('aborted', 'AbortError')
    const fetcher = vi.fn().mockRejectedValue(abort)
    const repository = new HttpAdminReadRepository({ client: new HttpClient({ fetch: fetcher }) })
    await expect(repository.getOverview()).rejects.toBe(abort)
  })
})
