import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

import { describe, expect, it, vi } from 'vitest'

import { AdminRepositoryError } from '@/contracts/error'

import { HttpAdminCommandRepository } from './http-admin-command-repository'
import { HttpClient } from './http-client'

const meta = {
  schemaVersion: 2,
  generatedAt: 1_787_652_000_000,
  adminApiVersion: 2,
  relayVersion: '0.3.0-rc.1',
  capabilities: [
    'admin_read_v2',
    'admin_device_manage_v2',
    'admin_maintenance_v2',
    'admin_wechat_manage_v2',
    'admin_wechat_login_v2',
  ],
}

const fixtureRoot = resolve(process.cwd(), '../../contracts/admin-api/v2/fixtures')

function fixture(name: string): Record<string, unknown> {
  return JSON.parse(readFileSync(resolve(fixtureRoot, name), 'utf8')) as Record<string, unknown>
}

function json(value: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  })
}

function repository(
  fetcher: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>,
): HttpAdminCommandRepository {
  return new HttpAdminCommandRepository({
    client: new HttpClient({ fetch: vi.fn(fetcher) }),
  })
}

describe('HttpAdminCommandRepository', () => {
  it('preflights capability and sends exact same-origin mutation requests', async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = []
    const fetcher = async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      calls.push({ url, init })
      if (url.endsWith('/meta')) return json(meta)
      if (url.endsWith('/devices')) {
        return json(fixture('device-create-receipt-v2.json'), 201)
      }
      if (url.endsWith('/devices/device-1/rotate')) {
        return json(fixture('device-rotate-receipt-v2.json'))
      }
      if (url.endsWith('/devices/device-1/disable')) {
        return json(fixture('device-action-receipt-v2.json'))
      }
      if (url.endsWith('/maintenance/retention')) {
        return json(fixture('retention-action-receipt-v2.json'))
      }
      if (url.endsWith('/wechat/test')) {
        return json(fixture('wechat-test-receipt-v2.json'), 202)
      }
      if (url.endsWith('/wechat/disconnect')) {
        return json(fixture('wechat-action-receipt-v2.json'))
      }
      throw new Error(`unexpected URL ${url}`)
    }
    const repo = repository(fetcher)

    const created = await repo.createDevice({
      name: 'relay device',
      scopes: ['notify:write'],
    })
    const rotated = await repo.rotateDevice({ deviceId: 'device-1' })
    const disabled = await repo.setDeviceEnabled({ deviceId: 'device-1', enabled: false })
    const retention = await repo.runRetention()
    const test = await repo.sendWechatTest()
    const disconnected = await repo.disconnectWechat({ reason: 'operator requested disconnect' })

    expect(created.action).toBe('create')
    expect(rotated.action).toBe('rotate')
    expect(disabled.action).toBe('device.disable')
    expect(retention).toMatchObject({
      outboxDeleted: 0,
      inboundDeleted: 0,
      selectionDeleted: 0,
    })
    expect(retention.summary).toContain('outbox 0')
    expect(test).toMatchObject({ state: 'accepted_by_relay' })
    expect(disconnected.action).toBe('wechat.disconnect')
    expect(calls.map((call) => call.url)).toEqual([
      '/admin/api/v2/meta',
      '/admin/api/v2/devices',
      '/admin/api/v2/devices/device-1/rotate',
      '/admin/api/v2/devices/device-1/disable',
      '/admin/api/v2/maintenance/retention',
      '/admin/api/v2/wechat/test',
      '/admin/api/v2/wechat/disconnect',
    ])
    for (const [index, call] of calls.slice(1).entries()) {
      expect(call.init?.method).toBe('POST')
      expect(call.init?.credentials).toBe('same-origin')
      expect(call.init?.cache).toBe('no-store')
      expect(new Headers(call.init?.headers).get('X-PromptDock-Admin-Action')).toBe('1')
      expect(new Headers(call.init?.headers).get('Content-Type')).toBe('application/json')
      expect(call.init?.body).toBe(
        index === 0
          ? '{"name":"relay device","scopes":["notify:write"]}'
          : index === 5
            ? '{"reason":"operator requested disconnect"}'
            : '{}',
      )
    }
    expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({
      name: 'relay device',
      scopes: ['notify:write'],
    })
  })

  it('uses all device paths and rejects invalid scope before network access', async () => {
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      if (url.endsWith('/meta')) return json(meta)
      return json({ receiptId: 'a', action: 'device.revoke', completedAt: 1 })
    })
    const repo = repository(fetcher)

    await repo.revokeDevice({ deviceId: 'device/with slash' })
    expect(String(fetcher.mock.calls[1]?.[0])).toBe(
      '/admin/api/v2/devices/device%2Fwith%20slash/revoke',
    )
    await expect(
      // @ts-expect-error exercises the runtime boundary against untyped callers.
      repo.createDevice({ name: 'bad scope', scopes: ['not-a-relay-scope'] }),
    ).rejects.toMatchObject({ adminError: { code: 'INVALID_INPUT' } })
  })

  it('fails closed when mutation capability is absent', async () => {
    const fetcher = vi.fn(async () => json({ ...meta, capabilities: ['admin_read_v2'] }))
    const repo = repository(fetcher)
    await expect(repo.runRetention()).rejects.toMatchObject({
      adminError: { code: 'INTERNAL', retryable: false },
    })
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('requires the exact 202 Relay acceptance status and validates disconnect reason locally', async () => {
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      if (String(input).endsWith('/meta')) return json(meta)
      return json(fixture('wechat-test-receipt-v2.json'), 200)
    })
    const repo = repository(fetcher)
    await expect(repo.sendWechatTest()).rejects.toMatchObject({
      adminError: { code: 'INTERNAL', retryable: false },
    })
    expect(fetcher).toHaveBeenCalledTimes(2)

    const invalid = repository(async (input) => {
      if (String(input).endsWith('/meta')) return json(meta)
      throw new Error('must not reach network')
    })
    await expect(invalid.disconnectWechat({ reason: '' })).rejects.toMatchObject({
      adminError: { code: 'INVALID_INPUT', retryable: false },
    })
    await expect(invalid.disconnectWechat({ reason: 'x'.repeat(121) })).rejects.toMatchObject({
      adminError: { code: 'INVALID_INPUT', retryable: false },
    })
  })

  it('requires Relay mutation success statuses exactly', async () => {
    const repo = repository(async (input) => {
      if (String(input).endsWith('/meta')) return json(meta)
      return json(fixture('device-create-receipt-v2.json'), 200)
    })

    await expect(
      repo.createDevice({ name: 'wrong status', scopes: ['gateway:connect'] }),
    ).rejects.toMatchObject({ adminError: { code: 'INTERNAL', retryable: false } })
  })

  it('does not log or transmit provider identifiers, test body, or an unrequested disconnect reason', async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = []
    const repo = repository(async (input, init) => {
      calls.push({ url: String(input), init })
      if (String(input).endsWith('/meta')) return json(meta)
      if (String(input).endsWith('/wechat/test')) {
        return json(fixture('wechat-test-receipt-v2.json'), 202)
      }
      return json(fixture('wechat-action-receipt-v2.json'))
    })
    await repo.sendWechatTest()
    await repo.disconnectWechat({ reason: 'explicit operator reason' })
    const serialized = JSON.stringify(calls)
    expect(serialized).not.toContain('provider')
    expect(serialized).not.toContain('message')
    expect(serialized).toContain('explicit operator reason')
    expect(calls[1]?.init?.body).toBe('{}')
  })

  it('rejects tampered success payloads and maps server statuses safely', async () => {
    const tampered = repository(async (input) => {
      if (String(input).endsWith('/meta')) return json(meta)
      return json({ receiptId: 'a', action: 'device.disable', completedAt: 1, extra: true })
    })
    await expect(
      tampered.setDeviceEnabled({ deviceId: 'device-1', enabled: false }),
    ).rejects.toMatchObject({
      adminError: {
        code: 'ADMIN_CONTRACT_MISMATCH',
        message: 'Admin 与 Relay 合同不兼容，请更新 Admin 或 Relay。',
      },
    })

    for (const status of [400, 403, 404, 409, 422, 429, 503]) {
      const repo = repository(async (input) => {
        if (String(input).endsWith('/meta')) return json(meta)
        return json(
          { code: 'SAFE_ERROR', message: 'safe error', requestId: `req-${status}` },
          status,
        )
      })
      await expect(repo.revokeDevice({ deviceId: 'device-1' })).rejects.toBeInstanceOf(
        AdminRepositoryError,
      )
    }
  })

  it('passes AbortError through and does not retry an unknown network result', async () => {
    const abort = new DOMException('aborted', 'AbortError')
    const fetcher = vi.fn(async () => {
      throw abort
    })
    const repo = repository(fetcher)
    await expect(repo.rotateDevice({ deviceId: 'device-1' })).rejects.toBe(abort)
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('uses the exact Admin-owned login contract and requires the canonical DELETE body', async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = []
    const loginId = '00000000-0000-4000-8000-000000000005'
    const repo = repository(async (input, init) => {
      const url = String(input)
      calls.push({ url, init })
      if (url.endsWith('/meta')) return json(meta)
      if (url.endsWith('/wechat/login')) return json(fixture('wechat-login-waiting-scan-v2.json'))
      if (url.endsWith(`/wechat/login/${loginId}`) && init?.method === 'GET') {
        return json(fixture('wechat-login-verify-required-v2.json'))
      }
      if (url.endsWith(`/wechat/login/${loginId}/verify`)) {
        return json(fixture('wechat-login-verify-required-v2.json'))
      }
      if (url.endsWith(`/wechat/login/${loginId}`) && init?.method === 'DELETE') {
        return json(fixture('wechat-login-cancelled-v2.json'))
      }
      throw new Error(`unexpected URL ${url}`)
    })

    await repo.startWechatLogin({ forceFresh: false })
    await repo.getWechatLogin(loginId)
    await repo.verifyWechatLogin({ loginId, code: '123456' })
    const cancelled = await repo.cancelWechatLogin(loginId)

    expect(cancelled.state).toBe('cancelled')
    expect(calls.map((call) => call.url)).toEqual([
      '/admin/api/v2/meta',
      '/admin/api/v2/wechat/login',
      `/admin/api/v2/wechat/login/${loginId}`,
      `/admin/api/v2/wechat/login/${loginId}/verify`,
      `/admin/api/v2/wechat/login/${loginId}`,
    ])
    expect(calls[1]?.init?.body).toBe('{"forceFresh":false}')
    expect(calls[2]?.init?.method).toBe('GET')
    expect(calls[3]?.init?.body).toBe('{"code":"123456"}')
    expect(calls[4]?.init?.method).toBe('DELETE')
    expect(calls[4]?.init?.body).toBe('{}')
    for (const call of [calls[1], calls[3], calls[4]]) {
      expect(new Headers(call?.init?.headers).get('X-PromptDock-Admin-Action')).toBe('1')
      expect(new Headers(call?.init?.headers).get('Content-Type')).toBe('application/json')
      expect(call?.init?.credentials).toBe('same-origin')
      expect(call?.init?.cache).toBe('no-store')
    }
  })

  it('rejects invalid login start, login id and verify input before network access', async () => {
    const fetcher = vi.fn(async () => json(meta))
    const repo = repository(fetcher)
    await expect(repo.startWechatLogin({ forceFresh: 'invalid' } as never)).rejects.toMatchObject({
      adminError: { code: 'INVALID_INPUT' },
    })
    await expect(repo.getWechatLogin('not-a-uuid')).rejects.toMatchObject({
      adminError: { code: 'INVALID_INPUT' },
    })
    await expect(
      repo.verifyWechatLogin({
        loginId: '00000000-0000-4000-8000-000000000005',
        code: '١٢٣',
      }),
    ).rejects.toMatchObject({ adminError: { code: 'INVALID_INPUT' } })
    expect(fetcher).not.toHaveBeenCalled()
  })
})
