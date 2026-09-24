import { afterEach, describe, expect, it, vi } from 'vitest'
import { nextTick } from 'vue'

import { createAdminSession } from './useAdminSession'
import { AdminRepositoryError, type AdminError } from '@/contracts/error'
import type { AdminRepositoryBundle } from '@/data/create-admin-repository'
import type { AdminCapability, AdminMeta } from '@promptdock/relay-admin-api-generated'
import { AdminCapabilityMetadata } from '@/data/http/admin-capability-metadata'
import { HttpClient } from '@/data/http/http-client'

function makeMeta(capabilities: AdminCapability[]): AdminMeta {
  return {
    schemaVersion: 2,
    generatedAt: 0,
    adminApiVersion: 2,
    relayVersion: 'test',
    capabilities,
  }
}

function makeError(): AdminError {
  return {
    code: 'RATE_LIMITED',
    message: 'too many requests',
    retryable: true,
    requestId: 'req-123',
  }
}

function makeRepository(
  getMeta: NonNullable<AdminRepositoryBundle['read']['getMeta']>,
): AdminRepositoryBundle {
  return {
    read: {
      getMeta,
      getOverview: vi.fn(),
      listDevices: vi.fn(),
      getDevice: vi.fn(),
      getWechatStatus: vi.fn(),
      listChannelEvents: vi.fn(),
      listDeliveries: vi.fn(),
      listInteractiveReplies: vi.fn(),
      listInboundCommands: vi.fn(),
      listResults: vi.fn(),
      getSystemSnapshot: vi.fn(),
    },
    command: {} as AdminRepositoryBundle['command'],
    clock: {} as AdminRepositoryBundle['clock'],
    reset: vi.fn(),
  }
}

describe('createAdminSession', () => {
  afterEach(() => vi.restoreAllMocks())

  it('uses the actual production session implementation and enters operator mode', async () => {
    const repository = makeRepository(
      vi
        .fn()
        .mockResolvedValue(
          makeMeta([
            'admin_read_v2',
            'admin_device_manage_v2',
            'admin_wechat_manage_v2',
            'admin_wechat_login_v2',
            'admin_maintenance_v2',
            'admin_results_manage_v2',
          ]),
        ),
    )
    const session = createAdminSession({ repository, production: true })
    await Promise.resolve()
    await nextTick()
    expect(session.mode.value).toBe('operator')
    expect(session.can('admin_device_manage_v2')).toBe(true)
    expect(session.readOnlyReason.value).toBeNull()
  })

  it('preserves the wrapped production error contract and can recover', async () => {
    const getMeta = vi
      .fn<NonNullable<AdminRepositoryBundle['read']['getMeta']>>()
      .mockRejectedValueOnce(new AdminRepositoryError(makeError()))
      .mockResolvedValueOnce(makeMeta(['admin_read_v2']))
    const session = createAdminSession({ repository: makeRepository(getMeta), production: true })
    await Promise.resolve()
    await nextTick()
    expect(session.state.value.kind).toBe('unavailable')
    expect(session.error.value).toEqual(makeError())
    expect(session.capabilityReason('admin_device_manage_v2')).toContain('读取失败')

    await session.refresh()
    expect(session.mode.value).toBe('read-only')
    expect(session.readOnlyReason.value).toContain('未授予任何写操作能力')
  })

  it('explains the exact missing capability in partial mode', async () => {
    const session = createAdminSession({
      repository: makeRepository(
        vi.fn().mockResolvedValue(makeMeta(['admin_read_v2', 'admin_device_manage_v2'])),
      ),
      production: true,
    })
    await Promise.resolve()
    await nextTick()
    expect(session.mode.value).toBe('partial')
    expect(session.capabilityReason('admin_device_manage_v2')).toBeNull()
    expect(session.capabilityReason('admin_maintenance_v2')).toBe('当前账号未授予“系统维护”能力。')
  })

  it('marks mock sessions as unsupported and grants local mutations', async () => {
    const session = createAdminSession({
      repository: makeRepository(vi.fn()),
      production: false,
    })
    await Promise.resolve()
    expect(session.mode.value).toBe('unsupported')
    expect(session.can('admin_device_manage_v2')).toBe(true)
  })

  it('aborts and fences an older refresh so a late capability receipt cannot overwrite the latest one', async () => {
    const pending: Array<{ resolve: (meta: AdminMeta) => void }> = []
    const getMeta = vi.fn(
      () =>
        new Promise<AdminMeta>((resolve) => {
          pending.push({ resolve })
        }),
    )
    const session = createAdminSession({ repository: makeRepository(getMeta), production: true })
    expect(getMeta).toHaveBeenCalledTimes(1)

    const latest = session.refresh()
    expect(getMeta).toHaveBeenCalledTimes(2)
    pending[1]?.resolve(makeMeta(['admin_read_v2', 'admin_maintenance_v2']))
    await latest
    pending[0]?.resolve(makeMeta(['admin_read_v2', 'admin_device_manage_v2']))
    await Promise.resolve()
    await nextTick()

    expect(session.can('admin_maintenance_v2')).toBe(true)
    expect(session.can('admin_device_manage_v2')).toBe(false)
    session.dispose()
  })

  it('retains the last success as stale when a background refresh fails', async () => {
    const getMeta = vi
      .fn<NonNullable<AdminRepositoryBundle['read']['getMeta']>>()
      .mockResolvedValueOnce(makeMeta(['admin_read_v2', 'admin_device_manage_v2']))
      .mockRejectedValueOnce(new AdminRepositoryError(makeError()))
    const session = createAdminSession({ repository: makeRepository(getMeta), production: true })
    await Promise.resolve()
    await nextTick()
    const successfulAt = session.lastSuccessAt.value

    await session.refresh()

    expect(session.state.value).toMatchObject({ kind: 'ready' })
    expect(session.can('admin_device_manage_v2')).toBe(true)
    expect(session.stale.value).toBe(true)
    expect(session.lastSuccessAt.value).toBe(successfulAt)
    expect(session.error.value).toEqual(makeError())
    session.dispose()
  })

  it('refreshes stale capability metadata on focus, visibility, online, and the TTL timer', async () => {
    let now = 0
    const eventTarget = new EventTarget()
    const visibilitySource = Object.assign(new EventTarget(), {
      visibilityState: 'visible' as DocumentVisibilityState,
    })
    const getMeta = vi
      .fn<NonNullable<AdminRepositoryBundle['read']['getMeta']>>()
      .mockResolvedValue(makeMeta(['admin_read_v2']))
    const scheduled: Array<() => void> = []
    const session = createAdminSession({
      repository: makeRepository(getMeta),
      production: true,
      now: () => now,
      eventTarget,
      visibilitySource,
      setInterval: (handler) => {
        scheduled.push(handler)
        return 1 as unknown as ReturnType<typeof globalThis.setInterval>
      },
      clearInterval: vi.fn(),
    })
    await Promise.resolve()
    await nextTick()
    expect(getMeta).toHaveBeenCalledTimes(1)

    now = 60_001
    eventTarget.dispatchEvent(new Event('focus'))
    await Promise.resolve()
    await nextTick()
    expect(getMeta).toHaveBeenCalledTimes(2)

    now += 60_001
    visibilitySource.dispatchEvent(new Event('visibilitychange'))
    await Promise.resolve()
    await nextTick()
    expect(getMeta).toHaveBeenCalledTimes(3)

    now += 60_001
    eventTarget.dispatchEvent(new Event('online'))
    await Promise.resolve()
    await nextTick()
    expect(getMeta).toHaveBeenCalledTimes(4)

    now += 60_001
    scheduled[0]?.()
    await Promise.resolve()
    await nextTick()
    expect(getMeta).toHaveBeenCalledTimes(5)
    session.dispose()
  })

  it('refreshes the shared session after a login polling 401/403 invalidates capabilities', async () => {
    const metas = [
      makeMeta(['admin_read_v2', 'admin_wechat_login_v2']),
      makeMeta(['admin_read_v2', 'admin_maintenance_v2']),
    ]
    const client = new HttpClient({
      fetch: vi.fn((input: RequestInfo | URL) => {
        const body = String(input).endsWith('/meta') ? metas.shift() : { code: 'DENIED' }
        const status = String(input).endsWith('/meta') ? 200 : 403
        return Promise.resolve(
          new Response(JSON.stringify(body), {
            status,
            headers: { 'Content-Type': 'application/json' },
          }),
        )
      }),
    })
    const capabilityMetadata = new AdminCapabilityMetadata({ client })
    const getMeta = vi.fn((options?: { force?: boolean }) =>
      capabilityMetadata.get({ force: options?.force }),
    )
    const repository = makeRepository(getMeta)
    repository.capabilityMetadata = capabilityMetadata
    const session = createAdminSession({ repository, production: true })
    await Promise.resolve()
    await nextTick()

    await expect(
      client.request({ path: '/wechat/login/session-1', purpose: 'login-poll' }),
    ).rejects.toMatchObject({
      status: 403,
    })
    await Promise.resolve()
    await nextTick()

    expect(session.can('admin_wechat_login_v2')).toBe(false)
    expect(session.can('admin_maintenance_v2')).toBe(true)
    expect(getMeta).toHaveBeenCalledTimes(2)
    session.dispose()
  })
})
